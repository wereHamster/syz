//! Format-preserving edits of `minimumReleaseAgeExclude` in `pnpm-workspace.yaml`.
//!
//! The file is edited line by line (instead of being re-serialized) so that comments, ordering
//! and formatting written by humans survive the edit.

use std::collections::{BTreeMap, BTreeSet};

const KEY: &str = "minimumReleaseAgeExclude";
const DEFAULT_INDENT: &str = "  ";

/// Exact versions per package name, e.g. `next -> {15.5.27}`.
pub(crate) type PinnedVersions = BTreeMap<String, BTreeSet<String>>;

/// A single `minimumReleaseAgeExclude` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Exclusion {
    /// A package name or glob (`next`, `@next/*`): every version is excluded.
    Package(String),
    /// Exact versions of a package (`next@15.5.27 || 15.5.28`).
    Versions { name: String, versions: Vec<String> },
}

impl Exclusion {
    fn parse(raw: &str) -> Self {
        let raw = raw.trim();
        match raw.rfind('@') {
            // `rfind` returns 0 for a bare scoped name like `@next/env`.
            Some(idx) if idx > 0 => Exclusion::Versions {
                name: raw[..idx].trim().to_string(),
                versions: raw[idx + 1..]
                    .split("||")
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
                    .collect(),
            },
            _ => Exclusion::Package(raw.to_string()),
        }
    }

    fn covers_all_versions_of(&self, package: &str) -> bool {
        matches!(self, Exclusion::Package(pattern) if glob_matches(pattern, package))
    }
}

fn render_versions(name: &str, versions: &[String]) -> String {
    format!("'{}@{}'", name, versions.join(" || "))
}

/// Matches `name` against a pnpm package pattern where `*` matches any run of characters.
fn glob_matches(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }

    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !name.starts_with(first) || !name[first.len()..].ends_with(last) {
        return false;
    }

    let mut rest = &name[first.len()..name.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(idx) => rest = &rest[idx + part.len()..],
            None => return false,
        }
    }
    true
}

struct Item {
    line: usize,
    indent: String,
    exclusion: Exclusion,
}

enum Block {
    Missing,
    /// `minimumReleaseAgeExclude: [a, b]` or any other value on the key line itself.
    Inline {
        line: usize,
        entries: Vec<String>,
    },
    List {
        line: usize,
        items: Vec<Item>,
    },
}

fn strip_comment(value: &str) -> &str {
    let value = value.trim();
    if value.starts_with('\'') || value.starts_with('"') {
        return value;
    }
    match value.find(" #") {
        Some(idx) => value[..idx].trim_end(),
        None if value.starts_with('#') => "",
        None => value,
    }
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    if let Some(inner) = value.strip_prefix('\'') {
        if let Some(end) = inner.rfind('\'') {
            return inner[..end].replace("''", "'");
        }
    }
    if let Some(inner) = value.strip_prefix('"') {
        if let Some(end) = inner.rfind('"') {
            return inner[..end].to_string();
        }
    }
    value.to_string()
}

fn find_block(lines: &[String]) -> Block {
    let Some(line) = lines
        .iter()
        .position(|l| l.starts_with(KEY) && l[KEY.len()..].trim_start().starts_with(':'))
    else {
        return Block::Missing;
    };

    let after_colon = &lines[line][lines[line].find(':').unwrap() + 1..];
    let inline = strip_comment(after_colon);
    if !inline.is_empty() {
        let entries = serde_yml::from_str::<Vec<String>>(inline).unwrap_or_default();
        return Block::Inline { line, entries };
    }

    let mut items = Vec::new();
    for (idx, text) in lines.iter().enumerate().skip(line + 1) {
        let trimmed = text.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Anything that isn't a list item (usually the next key) ends the block.
        let Some(value) = trimmed.strip_prefix('-') else {
            break;
        };
        if !(value.is_empty() || value.starts_with(' ')) {
            break;
        }
        items.push(Item {
            line: idx,
            indent: text[..text.len() - trimmed.len()].to_string(),
            exclusion: Exclusion::parse(&unquote(strip_comment(value))),
        });
    }

    Block::List { line, items }
}

fn split_lines(content: &str) -> (Vec<String>, bool) {
    (
        content.lines().map(str::to_string).collect(),
        content.is_empty() || content.ends_with('\n'),
    )
}

fn join_lines(lines: &[String], trailing_newline: bool) -> String {
    let mut out = lines.join("\n");
    if trailing_newline && !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Parses the `minimumReleaseAgeExclude` entries of a `pnpm-workspace.yaml`.
pub(crate) fn release_age_exclusions(content: &str) -> Vec<Exclusion> {
    let (lines, _) = split_lines(content);
    match find_block(&lines) {
        Block::Missing => Vec::new(),
        Block::Inline { entries, .. } => entries.iter().map(|e| Exclusion::parse(e)).collect(),
        Block::List { items, .. } => items.into_iter().map(|i| i.exclusion).collect(),
    }
}

/// Exact versions in `pinned` that the entries of `content` don't already exclude.
pub(crate) fn missing_exclusions(content: &str, pinned: &PinnedVersions) -> PinnedVersions {
    let existing = release_age_exclusions(content);
    let mut missing = PinnedVersions::new();
    for (name, versions) in pinned {
        if existing.iter().any(|e| e.covers_all_versions_of(name)) {
            continue;
        }
        for version in versions {
            let listed = existing.iter().any(|e| {
                matches!(e, Exclusion::Versions { name: n, versions: vs } if n == name && vs.contains(version))
            });
            if !listed {
                missing
                    .entry(name.clone())
                    .or_default()
                    .insert(version.clone());
            }
        }
    }
    missing
}

/// Adds exact-version exclusions to `content`, merging versions into an existing
/// `name@a || b` entry and skipping packages already excluded by name or glob.
pub(crate) fn add_release_age_exclusions(content: &str, additions: &PinnedVersions) -> String {
    let additions = missing_exclusions(content, additions);
    if additions.is_empty() {
        return content.to_string();
    }

    let (mut lines, trailing_newline) = split_lines(content);

    match find_block(&lines) {
        Block::List { line, items } => {
            let mut new_entries = Vec::new();
            for (name, versions) in &additions {
                let existing = items.iter().find(
                    |i| matches!(&i.exclusion, Exclusion::Versions { name: n, .. } if n == name),
                );
                match existing {
                    Some(Item {
                        line,
                        indent,
                        exclusion:
                            Exclusion::Versions {
                                versions: current, ..
                            },
                    }) => {
                        let mut merged = current.clone();
                        merged.extend(versions.iter().cloned());
                        lines[*line] = format!("{}- {}", indent, render_versions(name, &merged));
                    }
                    _ => new_entries.push(render_versions(
                        name,
                        &versions.iter().cloned().collect::<Vec<_>>(),
                    )),
                }
            }

            let indent = items
                .last()
                .map(|i| i.indent.clone())
                .unwrap_or_else(|| DEFAULT_INDENT.to_string());
            let insert_at = items.last().map(|i| i.line).unwrap_or(line) + 1;
            for (offset, entry) in new_entries.into_iter().enumerate() {
                lines.insert(insert_at + offset, format!("{}- {}", indent, entry));
            }
        }
        Block::Inline { line, entries } => {
            let mut block = vec![format!("{}:", KEY)];
            block.extend(
                entries
                    .iter()
                    .map(|e| format!("{}- '{}'", DEFAULT_INDENT, e.replace('\'', "''"))),
            );
            block.extend(additions.iter().map(|(name, versions)| {
                format!(
                    "{}- {}",
                    DEFAULT_INDENT,
                    render_versions(name, &versions.iter().cloned().collect::<Vec<_>>())
                )
            }));
            lines.splice(line..=line, block);
        }
        Block::Missing => {
            let mut block = vec![format!("{}:", KEY)];
            block.extend(additions.iter().map(|(name, versions)| {
                format!(
                    "{}- {}",
                    DEFAULT_INDENT,
                    render_versions(name, &versions.iter().cloned().collect::<Vec<_>>())
                )
            }));
            // Keep the exclusions next to the setting they relax.
            let insert_at = lines
                .iter()
                .position(|l| l.starts_with("minimumReleaseAge:"))
                .map(|idx| idx + 1)
                .unwrap_or(lines.len());
            lines.splice(insert_at..insert_at, block);
        }
    }

    join_lines(&lines, trailing_newline)
}

/// Removes the given exact versions from `content`. Entries left without versions are dropped,
/// and so is the key once it has no entries. Name and glob entries are never touched.
pub(crate) fn remove_release_age_exclusions(content: &str, removals: &PinnedVersions) -> String {
    let (mut lines, trailing_newline) = split_lines(content);

    let Block::List { line, items } = find_block(&lines) else {
        return content.to_string();
    };

    let mut changed = false;
    let mut removed_lines = BTreeSet::new();
    for item in &items {
        let Exclusion::Versions { name, versions } = &item.exclusion else {
            continue;
        };
        let Some(to_remove) = removals.get(name) else {
            continue;
        };
        let kept: Vec<String> = versions
            .iter()
            .filter(|v| !to_remove.contains(*v))
            .cloned()
            .collect();
        if kept.len() == versions.len() {
            continue;
        }
        changed = true;
        if kept.is_empty() {
            removed_lines.insert(item.line);
        } else {
            lines[item.line] = format!("{}- {}", item.indent, render_versions(name, &kept));
        }
    }

    if !changed {
        return content.to_string();
    }

    if removed_lines.len() == items.len() {
        removed_lines.insert(line);
    }

    let lines: Vec<String> = lines
        .into_iter()
        .enumerate()
        .filter(|(idx, _)| !removed_lines.contains(idx))
        .map(|(_, l)| l)
        .collect();

    join_lines(&lines, trailing_newline)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pins(entries: &[(&str, &[&str])]) -> PinnedVersions {
        entries
            .iter()
            .map(|(name, versions)| {
                (
                    name.to_string(),
                    versions.iter().map(|v| v.to_string()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn parses_entries() {
        let content = "minimumReleaseAge: 1440\nminimumReleaseAgeExclude:\n  - next # framework\n  - '@next/*'\n  - \"webpack@4.47.0 || 5.102.1\"\n  - '@next/env@15.5.27'\npackages:\n  - apps/*\n";
        assert_eq!(
            release_age_exclusions(content),
            vec![
                Exclusion::Package("next".into()),
                Exclusion::Package("@next/*".into()),
                Exclusion::Versions {
                    name: "webpack".into(),
                    versions: vec!["4.47.0".into(), "5.102.1".into()]
                },
                Exclusion::Versions {
                    name: "@next/env".into(),
                    versions: vec!["15.5.27".into()]
                },
            ]
        );
    }

    #[test]
    fn adds_key_after_minimum_release_age_and_keeps_comments() {
        let content =
            "# Supply chain policy\nminimumReleaseAge: 1440 # one day\n\npackages:\n  - apps/*\n";
        let result = add_release_age_exclusions(
            content,
            &pins(&[("next", &["15.5.27"]), ("@next/env", &["15.5.27"])]),
        );
        assert_eq!(
            result,
            "# Supply chain policy\nminimumReleaseAge: 1440 # one day\nminimumReleaseAgeExclude:\n  - '@next/env@15.5.27'\n  - 'next@15.5.27'\n\npackages:\n  - apps/*\n"
        );
    }

    #[test]
    fn appends_to_existing_list_and_merges_versions() {
        let content = "minimumReleaseAgeExclude:\n    - left-pad\n    - 'next@15.5.26' # pinned by hand\npackages:\n  - apps/*\n";
        let result = add_release_age_exclusions(
            content,
            &pins(&[("next", &["15.5.27"]), ("react", &["19.3.1"])]),
        );
        assert_eq!(
            result,
            "minimumReleaseAgeExclude:\n    - left-pad\n    - 'next@15.5.26 || 15.5.27'\n    - 'react@19.3.1'\npackages:\n  - apps/*\n"
        );
    }

    #[test]
    fn skips_packages_excluded_by_name_or_glob() {
        let content = "minimumReleaseAgeExclude:\n  - next\n  - '@next/*'\n";
        let additions = pins(&[
            ("next", &["15.5.27"]),
            ("@next/swc-linux-x64-gnu", &["15.5.27"]),
        ]);
        assert!(missing_exclusions(content, &additions).is_empty());
        assert_eq!(add_release_age_exclusions(content, &additions), content);
    }

    #[test]
    fn rewrites_inline_list_as_block() {
        let content = "minimumReleaseAge: 1440\nminimumReleaseAgeExclude: [left-pad]\n";
        let result = add_release_age_exclusions(content, &pins(&[("next", &["15.5.27"])]));
        assert_eq!(
            result,
            "minimumReleaseAge: 1440\nminimumReleaseAgeExclude:\n  - 'left-pad'\n  - 'next@15.5.27'\n"
        );
    }

    #[test]
    fn creates_file_content_from_scratch() {
        let result = add_release_age_exclusions("", &pins(&[("next", &["15.5.27"])]));
        assert_eq!(result, "minimumReleaseAgeExclude:\n  - 'next@15.5.27'\n");
    }

    #[test]
    fn removes_versions_and_drops_empty_entries() {
        let content = "minimumReleaseAgeExclude:\n  - left-pad\n  - 'next@15.5.26 || 15.5.27'\n  - '@next/env@15.5.27'\npackages:\n  - apps/*\n";
        let result = remove_release_age_exclusions(
            content,
            &pins(&[("next", &["15.5.26"]), ("@next/env", &["15.5.27"])]),
        );
        assert_eq!(
            result,
            "minimumReleaseAgeExclude:\n  - left-pad\n  - 'next@15.5.27'\npackages:\n  - apps/*\n"
        );
    }

    #[test]
    fn removes_key_once_empty() {
        let content = "minimumReleaseAge: 1440\nminimumReleaseAgeExclude:\n  - 'next@15.5.27'\npackages:\n  - apps/*\n";
        let result = remove_release_age_exclusions(content, &pins(&[("next", &["15.5.27"])]));
        assert_eq!(result, "minimumReleaseAge: 1440\npackages:\n  - apps/*\n");
    }

    #[test]
    fn glob_matching() {
        assert!(glob_matches("@next/*", "@next/swc-darwin-arm64"));
        assert!(glob_matches("*-plugin-*", "eslint-plugin-react"));
        assert!(!glob_matches("@next/*", "next"));
        assert!(!glob_matches("next", "next-auth"));
    }
}
