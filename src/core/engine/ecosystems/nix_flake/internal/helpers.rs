use crate::core::clients::github::GitHub;

pub struct GithubRepo {
    pub owner: String,
    pub repo: String,
}

/// Formats a github flake input as a normalized flake reference, e.g.
/// `github:NixOS/nixpkgs/nixpkgs-unstable`, or `github:NixOS/nixpkgs` if no ref is tracked.
pub fn format_github_ref(owner: &str, repo: &str, reference: Option<&str>) -> String {
    match reference {
        Some(r) if !r.is_empty() => format!("github:{}/{}/{}", owner, repo, r),
        _ => format!("github:{}/{}", owner, repo),
    }
}

/// Recovers the owner/repo from a normalized flake reference produced by [`format_github_ref`].
pub fn parse_github_repo(normalized: &str) -> Option<GithubRepo> {
    let rest = normalized.strip_prefix("github:")?;
    let mut parts = rest.splitn(3, '/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(GithubRepo { owner, repo })
}

/// Resolves the latest commit sha for a given ref (branch or tag), or the repository's default
/// branch if no ref is given.
/// Builds the GitHub API route for resolving the latest commit on a ref. Flake refs can legally
/// contain '/' (e.g. branch names like "release/23.11"), which must be percent-encoded so it
/// isn't parsed as an extra path segment.
fn commit_route(owner: &str, repo: &str, reference: Option<&str>) -> String {
    match reference {
        Some(r) => format!(
            "/repos/{}/{}/commits/{}",
            owner,
            repo,
            urlencoding::encode(r)
        ),
        None => format!("/repos/{}/{}/commits?per_page=1", owner, repo),
    }
}

pub async fn get_commit_date(
    github_client: &GitHub,
    owner: &str,
    repo: &str,
    sha: &str,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let route = commit_route(owner, repo, Some(sha));
    let res = github_client.get_json(&route).await.ok()?;
    let date_str = res.get("commit")?.get("committer")?.get("date")?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(date_str)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

pub async fn get_latest_commit_sha(
    github_client: &GitHub,
    owner: &str,
    repo: &str,
    reference: Option<&str>,
) -> Option<String> {
    let route = commit_route(owner, repo, reference);
    let res = github_client.get_json(&route).await.ok()?;

    match reference {
        Some(_) => res.get("sha")?.as_str().map(|s| s.to_string()),
        None => res
            .as_array()?
            .first()?
            .get("sha")?
            .as_str()
            .map(|s| s.to_string()),
    }
}

pub struct GitRepo {
    pub url: String,
}

/// Formats a generic git flake input as a normalized flake reference, e.g.
/// `git+https://tangled.org/@tangled.org/core?ref=main`, or `git+<url>` if no ref is tracked.
pub fn format_git_ref(url: &str, reference: Option<&str>) -> String {
    match reference {
        Some(r) if !r.is_empty() => format!("git+{}?ref={}", url, r),
        _ => format!("git+{}", url),
    }
}

/// Recovers the url from a normalized flake reference produced by [`format_git_ref`].
pub fn parse_git_url(normalized: &str) -> Option<GitRepo> {
    let rest = normalized.strip_prefix("git+")?;
    let url = rest.split('?').next().unwrap_or(rest);
    if url.is_empty() {
        return None;
    }
    Some(GitRepo {
        url: url.to_string(),
    })
}

/// Resolves the latest commit sha for a generic git flake input (`git+https://...`,
/// `git+ssh://...`), using nix's own git-fetcher semantics: `refs/heads/{ref}` or
/// `refs/tags/{ref}` when a ref is tracked, `HEAD` otherwise. Uses the SSH-agent credentials
/// callback (as in `clients/tangled.rs`) for `git+ssh` URLs; `git+https` reads need no
/// credentials for a public repo. Fails soft (`None`) on any error, matching
/// [`get_latest_commit_sha`]'s contract.
pub async fn get_latest_git_commit_sha(url: &str, reference: Option<&str>) -> Option<String> {
    let url = url.to_string();
    let reference = reference.map(|r| r.to_string());

    tokio::task::spawn_blocking(move || resolve_latest_git_commit_sha(&url, reference.as_deref()))
        .await
        .ok()
        .flatten()
}

fn resolve_latest_git_commit_sha(url: &str, reference: Option<&str>) -> Option<String> {
    let mut remote = git2::Remote::create_detached(url).ok()?;

    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(|_url, username_from_url, _allowed_types| {
        git2::Cred::ssh_key_from_agent(username_from_url.unwrap_or("git"))
    });

    let connection = remote
        .connect_auth(git2::Direction::Fetch, Some(callbacks), None)
        .ok()?;

    let heads = connection.list().ok()?;

    match reference {
        Some(r) => {
            let branch_ref = format!("refs/heads/{}", r);
            let tag_ref = format!("refs/tags/{}", r);
            heads
                .iter()
                .find(|h| h.name() == branch_ref)
                .or_else(|| heads.iter().find(|h| h.name() == tag_ref))
                .map(|h| h.oid().to_string())
        }
        None => heads
            .iter()
            .find(|h| h.name() == "HEAD")
            .map(|h| h.oid().to_string()),
    }
}

pub struct TarballLocation {
    pub url: String,
}

/// Formats a generic https tarball flake input as a normalized flake reference, e.g.
/// `tarball+https://flakehub.com/f/DeterminateSystems/determinate/3.tar.gz`.
pub fn format_tarball_ref(url: &str) -> String {
    format!("tarball+{}", url)
}

/// Recovers the url from a normalized flake reference produced by [`format_tarball_ref`].
pub fn parse_tarball_url(normalized: &str) -> Option<TarballLocation> {
    let rest = normalized.strip_prefix("tarball+")?;
    if rest.is_empty() {
        return None;
    }
    Some(TarballLocation {
        url: rest.to_string(),
    })
}

/// Resolves the "latest" pinned location for a generic https tarball flake input, by
/// replicating nix's own tarball-fetcher protocol: redirects are followed one at a time
/// until a response carries a `Link: <url>; rel="immutable"` header, whose URL is the
/// canonical resolved location for that content (this is how nix itself locks e.g.
/// FlakeHub inputs — the tracked URL redirects through a version-resolution step to an
/// immutable, permanently-cacheable URL, which is what ends up as `locked.url` in
/// `flake.lock`). A host that doesn't implement this convention has no reliable "latest"
/// signal — notably, blindly following redirects to completion is not safe in general,
/// since the final hop is commonly a presigned URL (expiring signature in the query
/// string) that changes on every request regardless of whether the content did. Fails
/// soft (`None`) in that case, same as when there is no update available.
pub async fn get_latest_tarball_location(url: &str) -> Option<String> {
    // Some hosts along the redirect chain (e.g. api.flakehub.com) reject requests with no
    // `User-Agent` header at all, so this can't just use `reqwest::Client::new()`.
    let client = reqwest::Client::builder()
        .user_agent(format!("Syz/{}", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()?;

    let mut current = url.to_string();

    for _ in 0..10 {
        let response = client.head(&current).send().await.ok()?;

        if let Some(immutable_url) = immutable_link_url(&response) {
            return Some(immutable_url);
        }

        if !response.status().is_redirection() {
            return None;
        }

        let location = response
            .headers()
            .get(reqwest::header::LOCATION)?
            .to_str()
            .ok()?;
        current = response.url().join(location).ok()?.to_string();
    }

    None
}

fn immutable_link_url(response: &reqwest::Response) -> Option<String> {
    let header_value = response.headers().get(reqwest::header::LINK)?.to_str().ok()?;
    parse_immutable_link(header_value).map(|url| url.split('?').next().unwrap_or(&url).to_string())
}

/// Parses an RFC 8288 `Link` header value for a `rel="immutable"` entry, returning its URL.
fn parse_immutable_link(header_value: &str) -> Option<String> {
    for entry in header_value.split(',') {
        let mut segments = entry.trim().split(';');
        let url = segments
            .next()?
            .trim()
            .strip_prefix('<')?
            .strip_suffix('>')?;

        let is_immutable = segments
            .any(|param| matches!(param.trim(), "rel=\"immutable\"" | "rel=immutable"));

        if is_immutable {
            return Some(url.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_github_ref_with_ref() {
        assert_eq!(
            format_github_ref("NixOS", "nixpkgs", Some("nixpkgs-unstable")),
            "github:NixOS/nixpkgs/nixpkgs-unstable"
        );
    }

    #[test]
    fn test_format_github_ref_without_ref() {
        assert_eq!(
            format_github_ref("numtide", "flake-utils", None),
            "github:numtide/flake-utils"
        );
        assert_eq!(
            format_github_ref("numtide", "flake-utils", Some("")),
            "github:numtide/flake-utils"
        );
    }

    #[test]
    fn test_commit_route_encodes_slashes_in_ref() {
        assert_eq!(
            commit_route("NixOS", "nixpkgs", Some("release/23.11")),
            "/repos/NixOS/nixpkgs/commits/release%2F23.11"
        );
        assert_eq!(
            commit_route("NixOS", "nixpkgs", Some("nixpkgs-unstable")),
            "/repos/NixOS/nixpkgs/commits/nixpkgs-unstable"
        );
    }

    #[test]
    fn test_commit_route_no_ref_uses_default_branch_listing() {
        assert_eq!(
            commit_route("NixOS", "nixpkgs", None),
            "/repos/NixOS/nixpkgs/commits?per_page=1"
        );
    }

    #[test]
    fn test_parse_github_repo() {
        let repo = parse_github_repo("github:NixOS/nixpkgs/nixpkgs-unstable").unwrap();
        assert_eq!(repo.owner, "NixOS");
        assert_eq!(repo.repo, "nixpkgs");

        let repo = parse_github_repo("github:numtide/flake-utils").unwrap();
        assert_eq!(repo.owner, "numtide");
        assert_eq!(repo.repo, "flake-utils");

        assert!(parse_github_repo("git:https://example.com/foo.git").is_none());
        assert!(parse_github_repo("github:justowner").is_none());
    }

    #[test]
    fn test_format_git_ref_with_ref() {
        assert_eq!(
            format_git_ref("https://tangled.org/@tangled.org/core", Some("main")),
            "git+https://tangled.org/@tangled.org/core?ref=main"
        );
    }

    #[test]
    fn test_format_git_ref_without_ref() {
        assert_eq!(
            format_git_ref("https://tangled.org/@tangled.org/core", None),
            "git+https://tangled.org/@tangled.org/core"
        );
        assert_eq!(
            format_git_ref("https://tangled.org/@tangled.org/core", Some("")),
            "git+https://tangled.org/@tangled.org/core"
        );
    }

    #[test]
    fn test_parse_git_url() {
        let repo = parse_git_url("git+https://tangled.org/@tangled.org/core?ref=main").unwrap();
        assert_eq!(repo.url, "https://tangled.org/@tangled.org/core");

        let repo = parse_git_url("git+https://tangled.org/@tangled.org/core").unwrap();
        assert_eq!(repo.url, "https://tangled.org/@tangled.org/core");

        assert!(parse_git_url("github:NixOS/nixpkgs").is_none());
        assert!(parse_git_url("git+").is_none());
    }

    #[test]
    fn test_format_tarball_ref() {
        assert_eq!(
            format_tarball_ref("https://flakehub.com/f/DeterminateSystems/determinate/3.tar.gz"),
            "tarball+https://flakehub.com/f/DeterminateSystems/determinate/3.tar.gz"
        );
    }

    #[test]
    fn test_parse_tarball_url() {
        let location = parse_tarball_url(
            "tarball+https://flakehub.com/f/DeterminateSystems/determinate/3.tar.gz",
        )
        .unwrap();
        assert_eq!(
            location.url,
            "https://flakehub.com/f/DeterminateSystems/determinate/3.tar.gz"
        );

        assert!(parse_tarball_url("github:NixOS/nixpkgs").is_none());
        assert!(parse_tarball_url("tarball+").is_none());
    }

    #[test]
    fn test_parse_immutable_link_matches_quoted_rel() {
        let header = r#"<https://api.flakehub.com/f/pinned/DeterminateSystems/determinate/3.22.2/uuid/source.tar.gz?rev=abc&revCount=1>; rel="immutable""#;
        assert_eq!(
            parse_immutable_link(header).as_deref(),
            Some("https://api.flakehub.com/f/pinned/DeterminateSystems/determinate/3.22.2/uuid/source.tar.gz?rev=abc&revCount=1")
        );
    }

    #[test]
    fn test_parse_immutable_link_ignores_other_rels() {
        let header = r#"<https://example.com/next>; rel="next", <https://example.com/pinned>; rel="immutable""#;
        assert_eq!(
            parse_immutable_link(header).as_deref(),
            Some("https://example.com/pinned")
        );
    }

    #[test]
    fn test_parse_immutable_link_returns_none_without_immutable_rel() {
        let header = r#"<https://example.com/next>; rel="next""#;
        assert!(parse_immutable_link(header).is_none());
    }
}
