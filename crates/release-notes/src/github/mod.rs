//! GitHub implementation of [`Forge`] — the first forge, extracted from the
//! legacy `GithubReleaseNotesResolver`.

use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Arc;

use crate::forge::{is_changelog_path, rank_changelog_paths, Forge};
use crate::http::HttpClient;
use crate::types::{PackageName, ReleaseInfo, Result, Tag};

/// Base URL the forge emits absolute URLs against. The transport adapter
/// derives its prefix from this constant — the two must agree by
/// construction, not by string coincidence.
pub const API_BASE: &str = "https://api.github.com";

/// GitHub forge over an injected transport. `owner`/`repo` identify the
/// upstream repository; all API routes are relative to it.
pub struct GitHubForge {
    http: Arc<dyn HttpClient>,
    owner: String,
    repo: String,
}

impl GitHubForge {
    pub fn new(
        http: Arc<dyn HttpClient>,
        owner: impl Into<String>,
        repo: impl Into<String>,
    ) -> Self {
        Self {
            http,
            owner: owner.into(),
            repo: repo.into(),
        }
    }

    fn repo_api(&self) -> String {
        format!("{API_BASE}/repos/{}/{}", self.owner, self.repo)
    }

    fn retry_after(resp: &crate::http::HttpResponse) -> Option<std::time::Duration> {
        resp.headers
            .get("retry-after")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(std::time::Duration::from_secs)
    }

    /// Raw GET with the shared status classification: 404 → miss, 403/429 →
    /// rate limit, other non-2xx → transport error.
    async fn get_raw(&self, url: &str) -> Result<Option<crate::http::HttpResponse>> {
        let resp = self
            .http
            .get(url)
            .await
            .map_err(|e| crate::types::Error::Transport(e.into_inner()))?;
        match resp.status() {
            404 => Ok(None),
            403 | 429 => Err(crate::types::Error::RateLimited {
                retry_after: Self::retry_after(&resp),
            }),
            _ if resp.status() / 100 != 2 => Err(crate::types::Error::Transport(anyhow::anyhow!(
                "GitHub API returned {} for {}",
                resp.status(),
                url
            ))),
            _ => Ok(Some(resp)),
        }
    }

    async fn get_json(&self, url: &str) -> Result<Option<serde_json::Value>> {
        let Some(resp) = self.get_raw(url).await? else {
            return Ok(None);
        };
        serde_json::from_slice(&resp.body)
            .map(Some)
            .map_err(|e| crate::types::Error::Transport(anyhow::anyhow!(e)))
    }
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    published_at: Option<chrono::DateTime<chrono::Utc>>,
    body: Option<serde_json::Value>,
}

impl From<GhRelease> for ReleaseInfo {
    fn from(r: GhRelease) -> Self {
        ReleaseInfo {
            tag: Tag::new(r.tag_name),
            published_at: r.published_at,
            body: match r.body {
                Some(serde_json::Value::String(s)) => Some(s),
                Some(_) => Some(String::new()),
                None => None,
            },
        }
    }
}

#[async_trait]
impl Forge for GitHubForge {
    async fn releases(&self, pages: usize) -> Result<Vec<ReleaseInfo>> {
        let mut out = Vec::new();
        let mut url: Option<String> =
            Some(format!("{}?per_page=100", self.repo_api() + "/releases"));

        for _ in 0..pages.max(1) {
            let Some(current) = url.take() else { break };
            let Some(resp) = self.get_raw(&current).await? else {
                break;
            };
            let page: Vec<GhRelease> = serde_json::from_slice(&resp.body)
                .map_err(|e| crate::types::Error::Transport(anyhow::anyhow!(e)))?;
            let is_last = page.is_empty();
            out.extend(page.into_iter().map(Into::into));

            if is_last {
                break;
            }
            url = next_link(resp.link_header());
        }

        Ok(out)
    }

    async fn release_by_tag(&self, tag: &Tag) -> Result<Option<ReleaseInfo>> {
        let url = format!("{}/releases/tags/{}", self.repo_api(), tag.url_path());
        let Some(json) = self.get_json(&url).await? else {
            return Ok(None);
        };
        let release: GhRelease = serde_json::from_value(json)
            .map_err(|e| crate::types::Error::Transport(anyhow::anyhow!(e)))?;
        Ok(Some(release.into()))
    }

    async fn changelog_candidates(&self, pkg: &PackageName) -> Result<Vec<String>> {
        let url = format!("{}/git/trees/HEAD?recursive=1", self.repo_api());
        let Some(json) = self.get_json(&url).await? else {
            return Ok(Vec::new());
        };

        if json
            .get("truncated")
            .and_then(|t| t.as_bool())
            .unwrap_or(false)
        {
            tracing::warn!(
                repo = format!("{}/{}", self.owner, self.repo),
                "GitHub tree response truncated; changelog file may be missed"
            );
        }

        let paths: Vec<String> = json
            .get("tree")
            .and_then(|t| t.as_array())
            .into_iter()
            .flatten()
            .filter_map(|item| item.get("path").and_then(|p| p.as_str()))
            .filter(|p| is_changelog_path(p))
            .map(str::to_string)
            .collect();

        Ok(rank_changelog_paths(paths, pkg))
    }

    async fn read_file(&self, path: &str) -> Result<Option<String>> {
        use base64::Engine as _;
        // Encode path segments conservatively, but leave `/` as a separator
        // and `-._~` unencoded (legacy percent-encoding behavior).
        const PATH_SET: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
            .remove(b'/')
            .remove(b'.')
            .remove(b'-')
            .remove(b'_')
            .remove(b'~');
        let url = format!(
            "{}/contents/{}",
            self.repo_api(),
            percent_encoding::utf8_percent_encode(path, PATH_SET)
        );
        let Some(json) = self.get_json(&url).await? else {
            return Ok(None);
        };

        let Some(content_b64) = json.get("content").and_then(|c| c.as_str()) else {
            return Ok(None);
        };
        let cleaned = content_b64.replace('\n', "");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(cleaned)
            .map_err(|e| crate::types::Error::Transport(anyhow::anyhow!(e)))?;
        String::from_utf8(decoded)
            .map(Some)
            .map_err(|e| crate::types::Error::Transport(anyhow::anyhow!(e)))
    }
}

/// Extract the `rel="next"` target from a `Link` header.
fn next_link(link_header: Option<&str>) -> Option<String> {
    let header = link_header?;
    for part in header.split(',') {
        let mut seg_url = None;
        let mut is_next = false;
        for piece in part.split(';') {
            let piece = piece.trim();
            if let Some(rest) = piece.strip_prefix('<').and_then(|r| r.strip_suffix('>')) {
                seg_url = Some(rest.to_string());
            } else if piece == "rel=\"next\"" || piece == "rel=next" {
                is_next = true;
            }
        }
        if is_next {
            return seg_url;
        }
    }
    None
}
