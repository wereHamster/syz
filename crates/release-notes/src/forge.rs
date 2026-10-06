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
    /// (ranking: paths containing the package short name first, then
    /// ascending path length). The `PackageName` is needed for ranking.
    async fn changelog_candidates(&self, pkg: &PackageName) -> Result<Vec<String>>;

    /// Fetch one file's content as UTF-8. Missing file → `Ok(None)`.
    async fn read_file(&self, path: &str) -> Result<Option<String>>;

    /// Fetch a single release by tag. Missing tag → `Ok(None)`. Used by the
    /// resolver's probe phase for versions older than the fetched releases
    /// pages (legacy deep-history parity).
    async fn release_by_tag(&self, tag: &Tag) -> Result<Option<ReleaseInfo>>;

    /// Stable URL to the release page for a tag on this forge.
    fn release_url(&self, tag: &Tag) -> String;
}
