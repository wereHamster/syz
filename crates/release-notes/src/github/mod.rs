//! GitHub implementation of [`Forge`] — the first forge, extracted from the
//! legacy `GithubReleaseNotesResolver`.

pub mod repository;

use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Arc;

use crate::forge::Forge;
use crate::http::HttpClient;
use crate::types::{PackageName, ReleaseInfo, Result, Tag};

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

    fn api_base() -> &'static str {
        "https://api.github.com"
    }

    fn repo_api(&self) -> String {
        format!("{}/repos/{}/{}", Self::api_base(), self.owner, self.repo)
    }

    fn retry_after(resp: &crate::http::HttpResponse) -> Option<std::time::Duration> {
        resp.headers
            .get("retry-after")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(std::time::Duration::from_secs)
    }

    async fn get_json(&self, url: &str) -> Result<Option<serde_json::Value>> {
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
            _ => Ok(Some(serde_json::from_slice(&resp.body).map_err(|e| {
                crate::types::Error::Transport(anyhow::anyhow!(e))
            })?)),
        }
    }

    /// Raw GET returning the full response (status preserved) — used for
    /// pagination over the releases list.
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
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[allow(dead_code)]
    #[serde(rename = "draft", default)]
    draft: bool,
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

        let mut changelog_paths: Vec<String> = json
            .get("tree")
            .and_then(|t| t.as_array())
            .map(|tree| {
                tree.iter()
                    .filter_map(|item| item.get("path").and_then(|p| p.as_str()))
                    .filter(|p| {
                        let lower = p.to_lowercase();
                        lower.ends_with("changelog.md") || lower.ends_with("changelog")
                    })
                    .map(|p| p.to_string())
                    .collect()
            })
            .unwrap_or_default();

        if changelog_paths.is_empty() {
            return Ok(Vec::new());
        }

        let short_name = pkg.short().to_lowercase();
        changelog_paths.sort_by(|a, b| {
            let a_contains = a.to_lowercase().contains(&short_name);
            let b_contains = b.to_lowercase().contains(&short_name);
            if a_contains == b_contains {
                a.len().cmp(&b.len())
            } else {
                b_contains.cmp(&a_contains)
            }
        });

        Ok(changelog_paths)
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

    fn release_url(&self, tag: &Tag) -> String {
        format!(
            "https://github.com/{}/{}/releases/tag/{}",
            self.owner,
            self.repo,
            tag.url_path()
        )
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
