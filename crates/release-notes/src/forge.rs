//! The [`Forge`] trait: per-forge access to releases, repository trees, and
//! file contents. One implementation per forge host (GitHub first); the
//! resolver is forge-agnostic.

use async_trait::async_trait;

use crate::types::{PackageName, ReleaseInfo, Result, Tag};

#[async_trait]
pub trait Forge: Send + Sync {
    /// All releases of the repository, newest first, including bodies.
    /// Implementations paginate internally up to `pages` (each page holds
    /// forge-specific count, e.g. 100 for GitHub); they return everything
    /// they can.
    async fn releases(&self, pages: usize) -> Result<Vec<ReleaseInfo>>;

    /// Ranked changelog file paths in the repository's default branch
    /// (ranking via [`rank_changelog_paths`]). The `PackageName` is needed
    /// for ranking.
    async fn changelog_candidates(&self, pkg: &PackageName) -> Result<Vec<String>>;

    /// Fetch one file's content as UTF-8. Missing file → `Ok(None)`.
    async fn read_file(&self, path: &str) -> Result<Option<String>>;

    /// Fetch a single release by tag. Missing tag → `Ok(None)`. Used by the
    /// resolver's probe phase for versions older than the fetched releases
    /// pages (legacy deep-history parity).
    async fn release_by_tag(&self, tag: &Tag) -> Result<Option<ReleaseInfo>>;
}

/// A path pointing at a changelog file (case-insensitive suffix match).
pub(crate) fn is_changelog_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with("changelog.md") || lower.ends_with("changelog")
}

/// Legacy changelog ranking, shared by forge implementations and the mock:
/// paths containing the package short name first, then ascending length.
pub(crate) fn rank_changelog_paths(mut paths: Vec<String>, pkg: &PackageName) -> Vec<String> {
    let short_name = pkg.short().to_lowercase();
    paths.sort_by(|a, b| {
        let a_contains = a.to_lowercase().contains(&short_name);
        let b_contains = b.to_lowercase().contains(&short_name);
        if a_contains == b_contains {
            a.len().cmp(&b.len())
        } else {
            b_contains.cmp(&a_contains)
        }
    });
    paths
}
