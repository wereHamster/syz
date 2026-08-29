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
}
