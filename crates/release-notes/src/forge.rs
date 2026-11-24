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
    /// (ranking via [`rank_changelog_paths`]). The `PackageName` and the
    /// package's directory in the repository, if known, are needed for
    /// ranking.
    async fn changelog_candidates(
        &self,
        pkg: &PackageName,
        directory: Option<&str>,
    ) -> Result<Vec<String>>;

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

/// Changelog ranking, shared by forge implementations and the mock: paths
/// inside the package's `directory` first (the monorepo layout npm records
/// in `repository.directory`, e.g. `packages/react/tooltip`), then paths
/// containing the package short name, then the rest; ascending length
/// within each group.
pub(crate) fn rank_changelog_paths(
    mut paths: Vec<String>,
    pkg: &PackageName,
    directory: Option<&str>,
) -> Vec<String> {
    let short_name = pkg.short().to_lowercase();
    let prefix = directory
        .map(|d| d.trim_start_matches("./").trim_matches('/'))
        .filter(|d| !d.is_empty())
        .map(|d| format!("{d}/"));
    let group = |path: &str| {
        if prefix.as_deref().is_some_and(|p| path.starts_with(p)) {
            0
        } else if path.to_lowercase().contains(&short_name) {
            1
        } else {
            2
        }
    };
    paths.sort_by_key(|p| (group(p), p.len()));
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn package_directory_ranks_first() {
        let ranked = rank_changelog_paths(
            paths(&[
                "packages/react/tooltip/CHANGELOG.md",
                "packages/react/id/CHANGELOG.md",
                "packages/core/rect/CHANGELOG.md",
                "packages/react/form/CHANGELOG.md",
            ]),
            &PackageName::new("@radix-ui/react-tooltip"),
            Some("packages/react/tooltip"),
        );
        assert_eq!(ranked[0], "packages/react/tooltip/CHANGELOG.md");
    }

    #[test]
    fn directory_forms_are_normalized() {
        for dir in ["./packages/a/", "/packages/a", "packages/a"] {
            let ranked = rank_changelog_paths(
                paths(&[
                    "CHANGELOG.md",
                    "packages/ab/CHANGELOG.md",
                    "packages/a/CHANGELOG.md",
                ]),
                &PackageName::new("x"),
                Some(dir),
            );
            assert_eq!(ranked[0], "packages/a/CHANGELOG.md", "dir: {dir}");
        }
    }

    #[test]
    fn without_directory_short_name_then_length() {
        let ranked = rank_changelog_paths(
            paths(&[
                "CHANGELOG.md",
                "packages/knip/CHANGELOG.md",
                "docs/CHANGELOG",
            ]),
            &PackageName::new("knip"),
            None,
        );
        assert_eq!(
            ranked,
            paths(&[
                "packages/knip/CHANGELOG.md",
                "CHANGELOG.md",
                "docs/CHANGELOG"
            ])
        );
    }
}
