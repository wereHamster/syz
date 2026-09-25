use anyhow::Result;
use toml_edit::{DocumentMut, Item};

use crate::core::engine::repository::ProjectRepositorySnapshot;

/// A Cargo workspace member manifest, relative to the repo root.
pub struct WorkspaceMember {
    /// e.g. "crates/tdsm/Cargo.toml"
    pub manifest_path: String,
    /// e.g. "crates/tdsm"
    pub dir: String,
}

/// Resolves the `[workspace].members` (and `.exclude`) declared in the root
/// `Cargo.toml` into a list of member manifest paths, by matching literal
/// paths and simple trailing-`*` glob patterns (e.g. "crates/*") against the
/// repository's file listing.
///
/// Returns an empty list if the root manifest has no `[workspace]` table or
/// no `members` entry, which is the common single-package-repo case.
pub async fn resolve_members(
    root_doc: &DocumentMut,
    repo: &dyn ProjectRepositorySnapshot,
) -> Result<Vec<WorkspaceMember>> {
    let members_patterns = match string_array(root_doc, "members") {
        Some(patterns) if !patterns.is_empty() => patterns,
        _ => return Ok(Vec::new()),
    };
    let exclude_patterns = string_array(root_doc, "exclude").unwrap_or_default();

    let files = repo.list_files().await?;
    let manifests: Vec<&str> = files
        .iter()
        .map(String::as_str)
        .filter(|p| p.ends_with("Cargo.toml"))
        .collect();

    let mut dirs: Vec<String> = Vec::new();
    for pattern in &members_patterns {
        dirs.extend(resolve_pattern(pattern, &manifests));
    }

    dirs.retain(|dir| {
        !exclude_patterns
            .iter()
            .any(|excl| dir_matches_pattern(dir, excl))
    });

    dirs.sort();
    dirs.dedup();

    Ok(dirs
        .into_iter()
        .map(|dir| WorkspaceMember {
            manifest_path: format!("{dir}/Cargo.toml"),
            dir,
        })
        .collect())
}

/// Reads `[workspace].<key>` from the root document as a list of strings, if present.
fn string_array(doc: &DocumentMut, key: &str) -> Option<Vec<String>> {
    let Item::Table(workspace) = doc.get("workspace")? else {
        return None;
    };
    let array = workspace.get(key)?.as_array()?;
    Some(
        array
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::to_string)
            .collect(),
    )
}

/// Resolves a single `[workspace].members` entry (literal path or simple
/// trailing-`*` glob) against the list of known `Cargo.toml` paths, returning
/// the matching member directories.
fn resolve_pattern(pattern: &str, manifests: &[&str]) -> Vec<String> {
    let trimmed = pattern.trim_end_matches('/');

    if let Some(prefix) = trimmed.strip_suffix('*') {
        if prefix.contains('*') {
            tracing::warn!("Unsupported cargo workspace member glob pattern: {pattern}");
            return Vec::new();
        }
        let prefix = prefix.trim_end_matches('/');
        let scan_prefix = if prefix.is_empty() {
            String::new()
        } else {
            format!("{prefix}/")
        };
        return manifests
            .iter()
            .filter_map(|m| {
                let rest = m.strip_prefix(scan_prefix.as_str())?;
                let dir_name = rest.strip_suffix("/Cargo.toml")?;
                if dir_name.contains('/') {
                    None
                } else {
                    Some(if scan_prefix.is_empty() {
                        dir_name.to_string()
                    } else {
                        format!("{prefix}/{dir_name}")
                    })
                }
            })
            .collect();
    }

    if trimmed.contains('*') {
        tracing::warn!("Unsupported cargo workspace member glob pattern: {pattern}");
        return Vec::new();
    }

    let candidate = format!("{trimmed}/Cargo.toml");
    if manifests.contains(&candidate.as_str()) {
        vec![trimmed.to_string()]
    } else {
        Vec::new()
    }
}

/// Checks whether `dir` is covered by an `exclude` entry (literal prefix or
/// simple trailing-`*` glob).
fn dir_matches_pattern(dir: &str, pattern: &str) -> bool {
    let trimmed = pattern.trim_end_matches('/');
    if let Some(prefix) = trimmed.strip_suffix('*') {
        let prefix = prefix.trim_end_matches('/');
        return prefix.is_empty() || dir == prefix || dir.starts_with(&format!("{prefix}/"));
    }
    dir == trimmed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::repository::test_support::MockSnapshot;

    fn parse(toml: &str) -> DocumentMut {
        toml.parse::<DocumentMut>().unwrap()
    }

    #[tokio::test]
    async fn no_workspace_table_returns_empty() {
        let doc = parse("[package]\nname = \"foo\"\n");
        let repo = MockSnapshot::with_files(&[("Cargo.toml", "[package]\nname = \"foo\"\n")]);
        let members = resolve_members(&doc, &repo).await.unwrap();
        assert!(members.is_empty());
    }

    #[tokio::test]
    async fn literal_member_path_is_resolved() {
        let doc = parse("[workspace]\nmembers = [\"crates/tdsm\"]\n");
        let repo = MockSnapshot::with_files(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"crates/tdsm\"]\n"),
            ("crates/tdsm/Cargo.toml", "[package]\nname = \"tdsm\"\n"),
        ]);
        let members = resolve_members(&doc, &repo).await.unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].dir, "crates/tdsm");
        assert_eq!(members[0].manifest_path, "crates/tdsm/Cargo.toml");
    }

    #[tokio::test]
    async fn glob_member_pattern_matches_multiple_dirs() {
        let doc = parse("[workspace]\nmembers = [\"crates/*\"]\n");
        let repo = MockSnapshot::with_files(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n"),
            ("crates/a/Cargo.toml", "[package]\nname = \"a\"\n"),
            ("crates/b/Cargo.toml", "[package]\nname = \"b\"\n"),
        ]);
        let mut members = resolve_members(&doc, &repo).await.unwrap();
        members.sort_by(|a, b| a.dir.cmp(&b.dir));
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].dir, "crates/a");
        assert_eq!(members[1].dir, "crates/b");
    }

    #[tokio::test]
    async fn excluded_member_is_skipped() {
        let doc = parse("[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"crates/b\"]\n");
        let repo = MockSnapshot::with_files(&[
            ("crates/a/Cargo.toml", "[package]\nname = \"a\"\n"),
            ("crates/b/Cargo.toml", "[package]\nname = \"b\"\n"),
        ]);
        let members = resolve_members(&doc, &repo).await.unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].dir, "crates/a");
    }
}
