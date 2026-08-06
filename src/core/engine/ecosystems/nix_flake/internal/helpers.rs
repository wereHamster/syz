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
}
