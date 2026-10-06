//! GitHub-specific URL handling: stable API endpoints and web URL shapes.
//! Repository URL parsing lives in the core [`crate::Repository`] type.

/// `https://api.github.com/repos/{owner}/{repo}`
pub fn endpoint(owner: &str, repo: &str) -> String {
    format!("https://api.github.com/repos/{}/{}", owner, repo)
}
