//! What changed between two versions of a `flake.lock`, node by node.
//!
//! The ids of the nodes in a lock (`nixpkgs_3`) are not stable, so nodes are matched by the
//! inputs that lead to them (see [`flake_lock::reachable_nodes`]). Only data in the lock is
//! used.

use std::collections::BTreeMap;

use chrono::NaiveDate;

use super::flake_lock::{self, FlakeLock, FlakeRef};

/// What a node of the lock is pinned to.
#[derive(Debug, Clone)]
pub struct Pin {
    pub ref_type: String,
    pub owner: Option<String>,
    pub repo: Option<String>,
    pub url: Option<String>,
    pub rev: Option<String>,
    pub nar_hash: Option<String>,
    /// Seconds since the epoch. Derived from the revision, so it is not part of what makes two
    /// pins differ.
    pub last_modified: Option<i64>,
}

impl Pin {
    fn from_ref(locked: &FlakeRef) -> Self {
        Self {
            ref_type: locked.ref_type.clone(),
            owner: locked.owner.clone(),
            repo: locked.repo.clone(),
            url: locked.url.clone(),
            rev: locked.rev.clone(),
            nar_hash: locked.nar_hash.clone(),
            last_modified: locked.last_modified,
        }
    }

    fn same_as(&self, other: &Pin) -> bool {
        self.ref_type == other.ref_type
            && self.owner == other.owner
            && self.repo == other.repo
            && self.url == other.url
            && self.rev == other.rev
            && self.nar_hash == other.nar_hash
    }

    /// The owner and name of the repository, if it is a github one that can be put in a link.
    pub fn github_repo(&self) -> Option<(&str, &str)> {
        if self.ref_type != "github" {
            return None;
        }

        match (self.owner.as_deref(), self.repo.as_deref()) {
            (Some(owner), Some(repo))
                if flake_lock::is_valid_owner(owner) && flake_lock::is_valid_repo(repo) =>
            {
                Some((owner, repo))
            }
            _ => None,
        }
    }

    /// The revision, if it is something that can be put in a link.
    pub fn rev(&self) -> Option<&str> {
        self.rev
            .as_deref()
            .filter(|rev| !rev.is_empty() && rev.bytes().all(|b| b.is_ascii_alphanumeric()))
    }

    /// The day the pinned revision was last modified.
    pub fn date(&self) -> Option<NaiveDate> {
        chrono::DateTime::from_timestamp(self.last_modified?, 0).map(|time| time.date_naive())
    }

    /// A short way to tell this pin from another one: the revision, else the version in the
    /// url of a tarball, else the start of the content hash.
    pub fn describe(&self) -> String {
        if let Some(rev) = self.rev() {
            return rev[..rev.len().min(7)].to_string();
        }

        if let Some(version) = self.url.as_deref().and_then(version_in_url) {
            return version.to_string();
        }

        match self.nar_hash.as_deref() {
            Some(hash) => hash
                .strip_prefix("sha256-")
                .unwrap_or(hash)
                .chars()
                .take(8)
                .collect(),
            None => "unknown".to_string(),
        }
    }

    /// Which repository the pin is of, if it has one. Pins of the same repository belong
    /// together, whichever input they are the pin of.
    fn source(&self) -> Option<String> {
        self.github_repo()
            .map(|(owner, repo)| format!("github:{owner}/{repo}"))
    }
}

/// The first part of the path of `url` that looks like a version (`v3.11.2`, `0.1.42`).
fn version_in_url(url: &str) -> Option<&str> {
    let path = url.split(['?', '#']).next().unwrap_or(url);

    path.split('/').rev().find(|segment| {
        let digits = segment.strip_prefix('v').unwrap_or(segment);
        digits.starts_with(|c: char| c.is_ascii_digit())
            && digits.contains('.')
            && segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+' | b'_' | b'%'))
    })
}

#[derive(Debug, Clone)]
pub enum ChangeKind {
    Updated { old: Pin, new: Pin },
    Added(Pin),
    Removed(Pin),
}

/// A node that is pinned differently in the new lock, or is only in one of them.
#[derive(Debug, Clone)]
pub struct InputChange {
    /// The names of the inputs from the root to the node, joined by `/`. A single name is a
    /// direct input.
    pub path: String,
    /// The other ways to the node, in the lock it is in (the new one, for a node in both).
    pub also_via: Vec<String>,
    pub kind: ChangeKind,
}

impl InputChange {
    /// The pin in the new lock, or in the old one for a removed node.
    pub fn pin(&self) -> &Pin {
        match &self.kind {
            ChangeKind::Updated { new, .. } => new,
            ChangeKind::Added(pin) | ChangeKind::Removed(pin) => pin,
        }
    }

    /// The name of the direct input the node hangs off.
    pub fn root_input(&self) -> &str {
        self.path.split('/').next().unwrap_or(&self.path)
    }
}

#[derive(Debug, Default)]
pub struct LockDiff {
    /// The inputs of the flake itself, by path.
    pub direct: Vec<InputChange>,
    /// The inputs of those inputs, and theirs, by path.
    pub indirect: Vec<InputChange>,
}

struct Entry {
    also_via: Vec<String>,
    pin: Pin,
}

fn entries(lock: &FlakeLock) -> BTreeMap<String, Entry> {
    flake_lock::reachable_nodes(lock)
        .into_iter()
        .filter_map(|node| {
            let pin = Pin::from_ref(node.node.locked.as_ref()?);
            let also_via = node
                .other_paths
                .iter()
                .map(|path| path.join("/"))
                .collect::<Vec<_>>();
            Some((node.path.join("/"), Entry { also_via, pin }))
        })
        .collect()
}

impl LockDiff {
    /// Compares two lock files, or returns `None` if either is not a lock file.
    pub fn between(old: &str, new: &str) -> Option<LockDiff> {
        let old: FlakeLock = serde_json::from_str(old).ok()?;
        let new: FlakeLock = serde_json::from_str(new).ok()?;
        Some(Self::of(&old, &new))
    }

    fn of(old: &FlakeLock, new: &FlakeLock) -> LockDiff {
        let old = entries(old);
        let mut new = entries(new);
        let mut diff = LockDiff::default();

        for (path, before) in old {
            let kind = match new.remove(&path) {
                Some(after) if before.pin.same_as(&after.pin) => continue,
                Some(after) => {
                    let kind = ChangeKind::Updated {
                        old: before.pin,
                        new: after.pin,
                    };
                    diff.push(path, after.also_via, kind);
                    continue;
                }
                None => ChangeKind::Removed(before.pin),
            };
            diff.push(path, before.also_via, kind);
        }

        for (path, after) in new {
            diff.push(path, after.also_via, ChangeKind::Added(after.pin));
        }

        diff.direct.sort_by(|a, b| a.path.cmp(&b.path));
        diff.indirect.sort_by(|a, b| a.path.cmp(&b.path));
        diff
    }

    fn push(&mut self, path: String, also_via: Vec<String>, kind: ChangeKind) {
        let list = if path.contains('/') {
            &mut self.indirect
        } else {
            &mut self.direct
        };
        list.push(InputChange {
            path,
            also_via,
            kind,
        });
    }

    pub fn is_empty(&self) -> bool {
        self.direct.is_empty() && self.indirect.is_empty()
    }
}

/// Changes that do the same thing to the same source, shown as one.
#[derive(Debug, Clone)]
pub struct ChangeGroup {
    /// The paths of the nodes, in order. The first one stands for the group.
    pub paths: Vec<String>,
    /// The other ways to the node, if the group is just that one.
    pub also_via: Vec<String>,
    pub kind: ChangeKind,
}

/// Merges the changes that move the same source between the same pins, such as a dependency
/// that several inputs each have their own copy of, or the tarballs of one release for each
/// platform. A source that has no repository is only merged within the direct input it hangs
/// off, as there is nothing else to say the changes belong together. Keeps the order of the
/// first change of each group.
pub fn collapse(changes: &[InputChange]) -> Vec<ChangeGroup> {
    let mut groups: Vec<ChangeGroup> = Vec::new();
    let mut index: BTreeMap<(u8, Option<String>, String, String, String), usize> = BTreeMap::new();

    for change in changes {
        let pin = change.pin();
        let source = pin.source();
        let scope = match source {
            Some(_) => String::new(),
            None => change.root_input().to_string(),
        };
        let (tag, before, after) = match &change.kind {
            ChangeKind::Updated { old, new } => (0, old.describe(), new.describe()),
            ChangeKind::Added(pin) => (1, String::new(), pin.describe()),
            ChangeKind::Removed(pin) => (2, pin.describe(), String::new()),
        };

        match index.get(&(
            tag,
            source.clone(),
            scope.clone(),
            before.clone(),
            after.clone(),
        )) {
            Some(&i) => {
                groups[i].paths.push(change.path.clone());
                groups[i].also_via.clear();
            }
            None => {
                index.insert((tag, source, scope, before, after), groups.len());
                groups.push(ChangeGroup {
                    paths: vec![change.path.clone()],
                    also_via: change.also_via.clone(),
                    kind: change.kind.clone(),
                });
            }
        }
    }

    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD_NIXPKGS: &str = "1111111111111111111111111111111111111111";
    const NEW_NIXPKGS: &str = "2222222222222222222222222222222222222222";
    const OLD_SYSTEMS: &str = "3333333333333333333333333333333333333333";
    const NEW_SYSTEMS: &str = "4444444444444444444444444444444444444444";

    fn github(owner: &str, repo: &str, rev: &str, modified: i64) -> String {
        format!(
            r#"{{ "type": "github", "owner": "{owner}", "repo": "{repo}", "rev": "{rev}", "narHash": "sha256-{rev}", "lastModified": {modified} }}"#
        )
    }

    fn lock(nodes: &[(&str, String, &str)], root_inputs: &str) -> String {
        let nodes: Vec<String> = nodes
            .iter()
            .map(|(id, locked, inputs)| {
                format!(r#""{id}": {{ "locked": {locked}, "inputs": {{ {inputs} }} }}"#)
            })
            .collect();

        format!(
            r#"{{ "nodes": {{ {}, "root": {{ "inputs": {{ {root_inputs} }} }} }}, "root": "root" }}"#,
            nodes.join(", ")
        )
    }

    fn paths(changes: &[InputChange]) -> Vec<&str> {
        changes.iter().map(|c| c.path.as_str()).collect()
    }

    #[test]
    fn test_between_is_empty_for_identical_locks() {
        let content = lock(
            &[(
                "nixpkgs",
                github("NixOS", "nixpkgs", OLD_NIXPKGS, 1_760_000_000),
                "",
            )],
            r#""nixpkgs": "nixpkgs""#,
        );

        assert!(LockDiff::between(&content, &content).unwrap().is_empty());
    }

    #[test]
    fn test_between_rejects_content_that_is_not_a_lock() {
        let content = lock(&[], "");

        assert!(LockDiff::between("not json", &content).is_none());
        assert!(LockDiff::between(&content, "").is_none());
    }

    #[test]
    fn test_between_separates_direct_from_indirect_inputs() {
        let old = lock(
            &[
                (
                    "nixpkgs",
                    github("NixOS", "nixpkgs", OLD_NIXPKGS, 1_760_000_000),
                    "",
                ),
                (
                    "systems",
                    github("nix-systems", "default", OLD_SYSTEMS, 1_700_000_000),
                    "",
                ),
                (
                    "treefmt-nix",
                    github("numtide", "treefmt-nix", OLD_NIXPKGS, 1_760_000_000),
                    r#""systems": "systems""#,
                ),
            ],
            r#""nixpkgs": "nixpkgs", "treefmt-nix": "treefmt-nix""#,
        );
        let new = lock(
            &[
                (
                    "nixpkgs",
                    github("NixOS", "nixpkgs", NEW_NIXPKGS, 1_760_500_000),
                    "",
                ),
                (
                    "systems",
                    github("nix-systems", "default", NEW_SYSTEMS, 1_710_000_000),
                    "",
                ),
                (
                    "treefmt-nix",
                    github("numtide", "treefmt-nix", OLD_NIXPKGS, 1_760_000_000),
                    r#""systems": "systems""#,
                ),
            ],
            r#""nixpkgs": "nixpkgs", "treefmt-nix": "treefmt-nix""#,
        );

        let diff = LockDiff::between(&old, &new).unwrap();

        assert_eq!(paths(&diff.direct), ["nixpkgs"]);
        assert_eq!(paths(&diff.indirect), ["treefmt-nix/systems"]);
        match &diff.direct[0].kind {
            ChangeKind::Updated { old, new } => {
                assert_eq!(old.describe(), "1111111");
                assert_eq!(new.describe(), "2222222");
                assert_eq!(old.date().unwrap().to_string(), "2025-10-09");
                assert_eq!(new.date().unwrap().to_string(), "2025-10-15");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn test_between_matches_nodes_by_path_not_by_id() {
        let old = lock(
            &[
                (
                    "nixpkgs",
                    github("NixOS", "nixpkgs", OLD_NIXPKGS, 1_760_000_000),
                    "",
                ),
                (
                    "nixpkgs_2",
                    github("NixOS", "nixpkgs", OLD_NIXPKGS, 1_760_000_000),
                    "",
                ),
                (
                    "tool",
                    github("acme", "tool", OLD_SYSTEMS, 1_700_000_000),
                    r#""nixpkgs": "nixpkgs_2""#,
                ),
            ],
            r#""nixpkgs": "nixpkgs", "tool": "tool""#,
        );
        // The ids are swapped, the inputs are not.
        let new = lock(
            &[
                (
                    "nixpkgs_2",
                    github("NixOS", "nixpkgs", OLD_NIXPKGS, 1_760_000_000),
                    "",
                ),
                (
                    "nixpkgs",
                    github("NixOS", "nixpkgs", OLD_NIXPKGS, 1_760_000_000),
                    "",
                ),
                (
                    "tool",
                    github("acme", "tool", OLD_SYSTEMS, 1_700_000_000),
                    r#""nixpkgs": "nixpkgs""#,
                ),
            ],
            r#""nixpkgs": "nixpkgs_2", "tool": "tool""#,
        );

        assert!(LockDiff::between(&old, &new).unwrap().is_empty());
    }

    #[test]
    fn test_between_reports_added_and_removed_nodes() {
        let old = lock(
            &[
                (
                    "tool",
                    github("acme", "tool", OLD_SYSTEMS, 1_700_000_000),
                    r#""systems": "systems""#,
                ),
                (
                    "systems",
                    github("nix-systems", "default", OLD_SYSTEMS, 1_700_000_000),
                    "",
                ),
            ],
            r#""tool": "tool""#,
        );
        let new = lock(
            &[
                (
                    "tool",
                    github("acme", "tool", OLD_SYSTEMS, 1_700_000_000),
                    r#""gc": "gc""#,
                ),
                ("gc", github("acme", "gc", NEW_SYSTEMS, 1_710_000_000), ""),
            ],
            r#""tool": "tool""#,
        );

        let diff = LockDiff::between(&old, &new).unwrap();

        assert!(diff.direct.is_empty());
        assert_eq!(paths(&diff.indirect), ["tool/gc", "tool/systems"]);
        assert!(matches!(diff.indirect[0].kind, ChangeKind::Added(_)));
        assert!(matches!(diff.indirect[1].kind, ChangeKind::Removed(_)));
    }

    #[test]
    fn test_between_lists_shared_nodes_that_move_in_opposite_directions() {
        let old = lock(
            &[
                (
                    "a",
                    github("acme", "a", OLD_SYSTEMS, 1_700_000_000),
                    r#""systems": "systems""#,
                ),
                (
                    "b",
                    github("acme", "b", OLD_SYSTEMS, 1_700_000_000),
                    r#""systems": "systems_2""#,
                ),
                (
                    "systems",
                    github("nix-systems", "default", OLD_NIXPKGS, 1_700_000_000),
                    "",
                ),
                (
                    "systems_2",
                    github("nix-systems", "default", NEW_NIXPKGS, 1_710_000_000),
                    "",
                ),
            ],
            r#""a": "a", "b": "b""#,
        );
        let new = lock(
            &[
                (
                    "a",
                    github("acme", "a", OLD_SYSTEMS, 1_700_000_000),
                    r#""systems": "systems""#,
                ),
                (
                    "b",
                    github("acme", "b", OLD_SYSTEMS, 1_700_000_000),
                    r#""systems": "systems_2""#,
                ),
                (
                    "systems",
                    github("nix-systems", "default", NEW_NIXPKGS, 1_710_000_000),
                    "",
                ),
                (
                    "systems_2",
                    github("nix-systems", "default", OLD_NIXPKGS, 1_700_000_000),
                    "",
                ),
            ],
            r#""a": "a", "b": "b""#,
        );

        let diff = LockDiff::between(&old, &new).unwrap();

        assert_eq!(paths(&diff.indirect), ["a/systems", "b/systems"]);
        let direction = |change: &InputChange| match &change.kind {
            ChangeKind::Updated { old, new } => (old.describe(), new.describe()),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            direction(&diff.indirect[0]),
            ("1111111".into(), "2222222".into())
        );
        assert_eq!(
            direction(&diff.indirect[1]),
            ("2222222".into(), "1111111".into())
        );
    }

    #[test]
    fn test_between_keeps_the_other_paths_of_a_shared_node() {
        let old = lock(
            &[
                (
                    "a",
                    github("acme", "a", OLD_SYSTEMS, 1_700_000_000),
                    r#""systems": "systems""#,
                ),
                (
                    "b",
                    github("acme", "b", OLD_SYSTEMS, 1_700_000_000),
                    r#""systems": ["a", "systems"]"#,
                ),
                (
                    "systems",
                    github("nix-systems", "default", OLD_NIXPKGS, 1_700_000_000),
                    "",
                ),
            ],
            r#""a": "a", "b": "b""#,
        );
        let new = old.replace(OLD_NIXPKGS, NEW_NIXPKGS);

        let diff = LockDiff::between(&old, &new).unwrap();

        assert_eq!(paths(&diff.indirect), ["a/systems"]);
        assert_eq!(diff.indirect[0].also_via, ["b/systems"]);
    }

    #[test]
    fn test_pin_ignores_the_modification_time() {
        let old = lock(
            &[("a", github("acme", "a", OLD_SYSTEMS, 1_700_000_000), "")],
            r#""a": "a""#,
        );
        let new = lock(
            &[("a", github("acme", "a", OLD_SYSTEMS, 1_800_000_000), "")],
            r#""a": "a""#,
        );

        assert!(LockDiff::between(&old, &new).unwrap().is_empty());
    }

    #[test]
    fn test_describe_falls_back_to_the_url_and_then_the_hash() {
        let tarball = |url: &str, hash: Option<&str>| Pin {
            ref_type: "tarball".into(),
            owner: None,
            repo: None,
            url: Some(url.into()),
            rev: None,
            nar_hash: hash.map(String::from),
            last_modified: None,
        };

        assert_eq!(
            tarball("https://example.org/tool/tag/v3.11.2/aarch64-linux", None).describe(),
            "v3.11.2"
        );
        assert_eq!(
            tarball(
                "https://example.org/f/0.1.42%2Brev-abc/x.tar.gz?token=1.2",
                None
            )
            .describe(),
            "0.1.42%2Brev-abc"
        );
        assert_eq!(
            tarball(
                "https://example.org/latest.tar.gz",
                Some("sha256-abcdefghijkl")
            )
            .describe(),
            "abcdefgh"
        );
        assert_eq!(
            tarball("https://example.org/latest.tar.gz", None).describe(),
            "unknown"
        );
    }

    #[test]
    fn test_github_repo_and_rev_only_pass_what_is_safe_in_a_link() {
        let pin = |owner: &str, repo: &str, rev: &str| Pin {
            ref_type: "github".into(),
            owner: Some(owner.into()),
            repo: Some(repo.into()),
            url: None,
            rev: Some(rev.into()),
            nar_hash: None,
            last_modified: None,
        };

        assert_eq!(
            pin("NixOS", "nixpkgs", "abc123").github_repo(),
            Some(("NixOS", "nixpkgs"))
        );
        assert_eq!(pin("a/b", "nixpkgs", "abc123").github_repo(), None);
        assert_eq!(pin("NixOS", "x)[y", "abc123").github_repo(), None);
        assert_eq!(pin("NixOS", "nixpkgs", "abc123").rev(), Some("abc123"));
        assert_eq!(pin("NixOS", "nixpkgs", "abc)").rev(), None);
    }

    fn updated(path: &str, owner: &str, repo: &str, old: &str, new: &str) -> InputChange {
        let pin = |rev: &str| Pin {
            ref_type: "github".into(),
            owner: Some(owner.into()),
            repo: Some(repo.into()),
            url: None,
            rev: Some(rev.into()),
            nar_hash: None,
            last_modified: None,
        };

        InputChange {
            path: path.into(),
            also_via: vec!["elsewhere".into()],
            kind: ChangeKind::Updated {
                old: pin(old),
                new: pin(new),
            },
        }
    }

    #[test]
    fn test_collapse_merges_identical_moves_wherever_they_are() {
        let changes = [
            updated(
                "a/systems",
                "nix-systems",
                "default",
                OLD_SYSTEMS,
                NEW_SYSTEMS,
            ),
            updated("b/nixpkgs", "NixOS", "nixpkgs", OLD_NIXPKGS, NEW_NIXPKGS),
            updated(
                "b/systems",
                "nix-systems",
                "default",
                OLD_SYSTEMS,
                NEW_SYSTEMS,
            ),
            updated(
                "c/systems",
                "nix-systems",
                "default",
                OLD_SYSTEMS,
                OLD_NIXPKGS,
            ),
        ];

        let groups = collapse(&changes);

        let paths: Vec<Vec<&str>> = groups
            .iter()
            .map(|g| g.paths.iter().map(String::as_str).collect())
            .collect();
        assert_eq!(
            paths,
            [
                vec!["a/systems", "b/systems"],
                vec!["b/nixpkgs"],
                vec!["c/systems"]
            ]
        );
        assert!(groups[0].also_via.is_empty());
        assert_eq!(groups[1].also_via, ["elsewhere"]);
    }

    fn tarball(path: &str, old_url: &str, url: &str) -> InputChange {
        let pin = |url: &str| Pin {
            ref_type: "tarball".into(),
            owner: None,
            repo: None,
            url: Some(url.into()),
            rev: None,
            nar_hash: None,
            last_modified: None,
        };

        InputChange {
            path: path.into(),
            also_via: Vec::new(),
            kind: ChangeKind::Updated {
                old: pin(old_url),
                new: pin(url),
            },
        }
    }

    #[test]
    fn test_collapse_merges_tarballs_of_a_release_within_one_input() {
        let changes = [
            tarball(
                "tool/daemon-aarch64",
                "https://example.org/daemon/v1.0.0/aarch64",
                "https://example.org/daemon/v1.1.0/aarch64",
            ),
            tarball(
                "tool/daemon-x86_64",
                "https://example.org/daemon/v1.0.0/x86_64",
                "https://example.org/daemon/v1.1.0/x86_64",
            ),
            tarball(
                "other/daemon-x86_64",
                "https://example.org/daemon/v1.0.0/x86_64",
                "https://example.org/daemon/v1.1.0/x86_64",
            ),
        ];

        let groups = collapse(&changes);

        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups[0].paths,
            ["tool/daemon-aarch64", "tool/daemon-x86_64"]
        );
        assert_eq!(groups[1].paths, ["other/daemon-x86_64"]);
    }
}
