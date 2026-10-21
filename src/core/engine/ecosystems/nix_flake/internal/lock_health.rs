//! How well kept a `flake.lock` is, judged from the lock alone: repositories that are locked at
//! more than one revision, and inputs that bring their own `nixpkgs` instead of following the
//! one of the flake.
//!
//! Nodes are named by the inputs that lead to them, as in [`super::lock_diff`].

use std::collections::BTreeMap;

use super::flake_lock::{self, FlakeLock};
use super::lock_diff::Pin;

/// How many inputs deep a node is that hangs directly off an input of the flake. A `follows`
/// in the flake can redirect those.
const FOLLOWABLE_DEPTH: usize = 2;

/// A repository that is locked at more than one revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Duplicate {
    /// `owner/repo`.
    pub repo: String,
    /// How many different revisions of it are locked. Copies at the same revision don't count.
    pub revisions: usize,
    /// The nodes, by path, the nearest first.
    pub paths: Vec<String>,
    /// The nodes that are an input of an input of the flake and not at the revision of the
    /// nearest node, so that a `follows` there would remove the duplicate.
    pub followable: Vec<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LockHealth {
    /// By repository.
    pub duplicates: Vec<Duplicate>,
    /// The inputs of the flake that have a `nixpkgs` of their own while the flake has one
    /// too, by name.
    pub not_following: Vec<String>,
}

impl LockHealth {
    /// `None` if `content` is not a lock file.
    pub fn of(content: &str) -> Option<LockHealth> {
        let lock: FlakeLock = serde_json::from_str(content).ok()?;
        Some(Self::of_lock(&lock))
    }

    fn of_lock(lock: &FlakeLock) -> LockHealth {
        let nodes = flake_lock::reachable_nodes(lock);

        let has_own_nixpkgs = |path: &[String]| {
            path.len() == FOLLOWABLE_DEPTH && path.last().map(String::as_str) == Some("nixpkgs")
        };
        let flake_has_nixpkgs = nodes.iter().any(|node| node.path == ["nixpkgs"]);

        let mut not_following: Vec<String> = Vec::new();
        if flake_has_nixpkgs {
            // A node that follows the `nixpkgs` of the flake is that node, and is reached
            // through the flake's input first. Only a node of its own is reached through the
            // input that has it.
            not_following = nodes
                .iter()
                .filter(|node| has_own_nixpkgs(&node.path))
                .map(|node| node.path[0].clone())
                .collect();
            not_following.sort();
            not_following.dedup();
        }

        let mut by_repo: BTreeMap<String, Vec<(Vec<String>, String)>> = BTreeMap::new();
        for node in &nodes {
            let Some(locked) = node.node.locked.as_ref() else {
                continue;
            };
            let pin = Pin::from_ref(locked);
            let (Some((owner, repo)), Some(rev)) = (pin.github_repo(), pin.rev()) else {
                continue;
            };
            by_repo
                .entry(format!("{owner}/{repo}"))
                .or_default()
                .push((node.path.clone(), rev.to_string()));
        }

        let duplicates = by_repo
            .into_iter()
            .filter_map(|(repo, copies)| {
                let mut revisions: Vec<&str> = copies.iter().map(|(_, rev)| rev.as_str()).collect();
                revisions.sort();
                revisions.dedup();
                if revisions.len() < 2 {
                    return None;
                }

                let nearest = &copies[0].1;
                let followable = copies
                    .iter()
                    // The own `nixpkgs` of an input is already reported as such.
                    .filter(|(path, _)| !has_own_nixpkgs(path) || !flake_has_nixpkgs)
                    .filter(|(path, rev)| path.len() == FOLLOWABLE_DEPTH && rev != nearest)
                    .map(|(path, _)| path.join("/"))
                    .collect();

                Some(Duplicate {
                    repo,
                    revisions: revisions.len(),
                    paths: copies.iter().map(|(path, _)| path.join("/")).collect(),
                    followable,
                })
            })
            .collect();

        LockHealth {
            duplicates,
            not_following,
        }
    }

    /// The nodes a `follows` would remove a duplicate with, by path.
    pub fn followable(&self) -> impl Iterator<Item = &str> {
        self.duplicates
            .iter()
            .flat_map(|duplicate| duplicate.followable.iter().map(String::as_str))
    }

    /// How many revisions are locked beyond the first one of each repository.
    pub fn extra_revisions(&self) -> usize {
        self.duplicates
            .iter()
            .map(|duplicate| duplicate.revisions - 1)
            .sum()
    }
}

/// An input of the flake that follows a branch of a github repository, rather than being
/// pinned to a revision in the flake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedInput {
    pub name: String,
    pub owner: String,
    pub repo: String,
    /// The branch or tag it follows, the default branch if not given.
    pub git_ref: Option<String>,
    /// The revision it is locked at.
    pub rev: String,
    /// When that revision was committed, in seconds since the epoch.
    pub last_modified: Option<i64>,
}

/// The inputs of the flake that follow a github repository. `None` if `content` is not a lock
/// file.
pub fn tracked_inputs(content: &str) -> Option<Vec<TrackedInput>> {
    let lock: FlakeLock = serde_json::from_str(content).ok()?;

    let mut inputs: Vec<TrackedInput> = flake_lock::reachable_nodes(&lock)
        .into_iter()
        .filter(|node| node.path.len() == 1)
        .filter_map(|node| {
            let original = node.node.original.as_ref()?;
            if original.ref_type != "github" || original.rev.is_some() {
                return None;
            }

            let pin = Pin::from_ref(node.node.locked.as_ref()?);
            let (owner, repo) = pin.github_repo()?;

            Some(TrackedInput {
                name: node.path[0].clone(),
                owner: owner.to_string(),
                repo: repo.to_string(),
                git_ref: original.git_ref.clone(),
                rev: pin.rev()?.to_string(),
                last_modified: pin.last_modified,
            })
        })
        .collect();

    inputs.sort_by(|a, b| a.name.cmp(&b.name));
    Some(inputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rev(c: char) -> String {
        c.to_string().repeat(40)
    }

    fn node(owner: &str, repo: &str, revision: char, inputs: &str) -> String {
        format!(
            r#"{{ "locked": {{ "type": "github", "owner": "{owner}", "repo": "{repo}", "rev": "{}", "narHash": "sha256-{revision}" }},
                 "original": {{ "type": "github", "owner": "{owner}", "repo": "{repo}" }},
                 "inputs": {{ {inputs} }} }}"#,
            rev(revision)
        )
    }

    fn lock(nodes: &[(&str, String)], root_inputs: &str) -> String {
        let nodes: Vec<String> = nodes
            .iter()
            .map(|(id, node)| format!(r#""{id}": {node}"#))
            .collect();
        format!(
            r#"{{ "nodes": {{ {}, "root": {{ "inputs": {{ {root_inputs} }} }} }}, "root": "root" }}"#,
            nodes.join(", ")
        )
    }

    fn health(content: &str) -> LockHealth {
        LockHealth::of(content).unwrap()
    }

    #[test]
    fn a_lock_where_everything_follows_is_healthy() {
        let content = lock(
            &[
                ("nixpkgs", node("NixOS", "nixpkgs", 'a', "")),
                (
                    "tool",
                    node("acme", "tool", 'b', r#""nixpkgs": ["nixpkgs"]"#),
                ),
            ],
            r#""nixpkgs": "nixpkgs", "tool": "tool""#,
        );

        assert_eq!(health(&content), LockHealth::default());
    }

    #[test]
    fn copies_at_the_same_revision_are_not_duplicates() {
        let content = lock(
            &[
                ("nixpkgs", node("NixOS", "nixpkgs", 'a', "")),
                ("utils", node("numtide", "flake-utils", 'c', "")),
                ("utils_2", node("numtide", "flake-utils", 'c', "")),
                ("tool", node("acme", "tool", 'b', r#""utils": "utils_2""#)),
            ],
            r#""nixpkgs": "nixpkgs", "utils": "utils", "tool": "tool""#,
        );

        assert_eq!(health(&content), LockHealth::default());
    }

    #[test]
    fn an_input_with_a_nixpkgs_of_its_own_is_reported_once_as_such() {
        let content = lock(
            &[
                ("nixpkgs", node("NixOS", "nixpkgs", 'a', "")),
                ("nixpkgs_2", node("NixOS", "nixpkgs", 'b', "")),
                (
                    "tool",
                    node("acme", "tool", 'c', r#""nixpkgs": "nixpkgs_2""#),
                ),
            ],
            r#""nixpkgs": "nixpkgs", "tool": "tool""#,
        );
        let health = health(&content);

        assert_eq!(health.not_following, ["tool"]);
        assert_eq!(health.duplicates.len(), 1);
        assert_eq!(health.duplicates[0].repo, "NixOS/nixpkgs");
        assert_eq!(health.duplicates[0].revisions, 2);
        assert_eq!(health.duplicates[0].paths, ["nixpkgs", "tool/nixpkgs"]);
        assert!(health.duplicates[0].followable.is_empty());
    }

    #[test]
    fn without_a_nixpkgs_in_the_flake_there_is_nothing_to_follow() {
        let content = lock(
            &[
                ("nixpkgs", node("NixOS", "nixpkgs", 'a', "")),
                ("tool", node("acme", "tool", 'c', r#""nixpkgs": "nixpkgs""#)),
            ],
            r#""tool": "tool""#,
        );

        assert!(health(&content).not_following.is_empty());
    }

    #[test]
    fn a_duplicate_directly_below_an_input_can_be_followed() {
        let content = lock(
            &[
                ("systems", node("nix-systems", "default", 'a', "")),
                ("systems_2", node("nix-systems", "default", 'b', "")),
                (
                    "tool",
                    node("acme", "tool", 'c', r#""systems": "systems_2""#),
                ),
            ],
            r#""systems": "systems", "tool": "tool""#,
        );
        let health = health(&content);

        assert_eq!(health.followable().collect::<Vec<_>>(), ["tool/systems"]);
        assert_eq!(health.extra_revisions(), 1);
        assert!(health.not_following.is_empty());
    }

    #[test]
    fn a_duplicate_deeper_down_cannot_be_followed_from_the_flake() {
        let content = lock(
            &[
                ("systems", node("nix-systems", "default", 'a', "")),
                ("systems_2", node("nix-systems", "default", 'b', "")),
                (
                    "inner",
                    node("acme", "inner", 'd', r#""systems": "systems_2""#),
                ),
                ("tool", node("acme", "tool", 'c', r#""inner": "inner""#)),
            ],
            r#""systems": "systems", "tool": "tool""#,
        );
        let health = health(&content);

        assert_eq!(health.duplicates.len(), 1);
        assert_eq!(
            health.duplicates[0].paths,
            ["systems", "tool/inner/systems"]
        );
        assert_eq!(health.followable().count(), 0);
    }

    #[test]
    fn nodes_without_a_github_repository_are_left_alone() {
        let tarball =
            r#"{ "locked": { "type": "tarball", "url": "https://example.com/a-1.0.tar.gz" } }"#;
        let content = lock(
            &[("a", tarball.to_string()), ("b", tarball.to_string())],
            r#""a": "a", "b": "b""#,
        );

        assert_eq!(health(&content), LockHealth::default());
    }

    #[test]
    fn rejects_content_that_is_not_a_lock() {
        assert!(LockHealth::of("not json").is_none());
        assert!(tracked_inputs("").is_none());
    }

    #[test]
    fn tracked_inputs_are_the_unpinned_github_inputs_of_the_flake() {
        let pinned = format!(
            r#"{{ "locked": {{ "type": "github", "owner": "acme", "repo": "pinned", "rev": "{}" }},
                  "original": {{ "type": "github", "owner": "acme", "repo": "pinned", "rev": "{}" }} }}"#,
            rev('d'),
            rev('d')
        );
        let on_branch = format!(
            r#"{{ "locked": {{ "type": "github", "owner": "acme", "repo": "branchy", "rev": "{}" }},
                  "original": {{ "type": "github", "owner": "acme", "repo": "branchy", "ref": "release/1" }} }}"#,
            rev('e')
        );
        let content = lock(
            &[
                ("nixpkgs", node("NixOS", "nixpkgs", 'a', "")),
                ("nixpkgs_2", node("NixOS", "nixpkgs", 'b', "")),
                (
                    "tool",
                    node("acme", "tool", 'c', r#""nixpkgs": "nixpkgs_2""#),
                ),
                ("pinned", pinned),
                ("branchy", on_branch),
            ],
            r#""nixpkgs": "nixpkgs", "tool": "tool", "pinned": "pinned", "branchy": "branchy""#,
        );

        let inputs = tracked_inputs(&content).unwrap();
        let names: Vec<(&str, Option<&str>)> = inputs
            .iter()
            .map(|input| (input.name.as_str(), input.git_ref.as_deref()))
            .collect();

        assert_eq!(
            names,
            [
                ("branchy", Some("release/1")),
                ("nixpkgs", None),
                ("tool", None)
            ]
        );
        assert_eq!(inputs[0].rev, rev('e'));
    }
}
