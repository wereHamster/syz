use std::collections::{BTreeSet, HashMap};

use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct FlakeLock {
    pub(super) nodes: HashMap<String, FlakeNode>,
    pub(super) root: String,
}

#[derive(Deserialize, Default)]
pub(super) struct FlakeNode {
    #[serde(default)]
    pub(super) inputs: HashMap<String, serde_json::Value>,
    pub(super) original: Option<FlakeRef>,
    pub(super) locked: Option<FlakeRef>,
}

#[derive(Deserialize)]
pub(super) struct FlakeRef {
    #[serde(rename = "type", default)]
    pub(super) ref_type: String,
    pub(super) owner: Option<String>,
    pub(super) repo: Option<String>,
    pub(super) url: Option<String>,
    #[serde(rename = "ref")]
    pub(super) git_ref: Option<String>,
    pub(super) rev: Option<String>,
    #[serde(rename = "narHash")]
    pub(super) nar_hash: Option<String>,
    #[serde(rename = "lastModified")]
    pub(super) last_modified: Option<i64>,
}

pub(super) struct GithubRootInput<'a> {
    pub(super) alias: &'a str,
    pub(super) owner: &'a str,
    pub(super) repo: &'a str,
    pub(super) git_ref: Option<&'a str>,
    pub(super) rev: Option<&'a str>,
}

pub(super) struct GitRootInput<'a> {
    pub(super) alias: &'a str,
    pub(super) url: &'a str,
    pub(super) git_ref: Option<&'a str>,
    pub(super) rev: Option<&'a str>,
}

pub(super) struct TarballRootInput<'a> {
    pub(super) alias: &'a str,
    pub(super) url: &'a str,
    pub(super) locked_url: Option<&'a str>,
}

/// A direct (non-`follows`) root input, paired with its local alias.
pub(super) enum RootInput<'a> {
    Github(GithubRootInput<'a>),
    Git(GitRootInput<'a>),
    Tarball(TarballRootInput<'a>),
}

impl<'a> RootInput<'a> {
    pub(super) fn alias(&self) -> &'a str {
        match self {
            RootInput::Github(input) => input.alias,
            RootInput::Git(input) => input.alias,
            RootInput::Tarball(input) => input.alias,
        }
    }
}

/// The distinct owners (sorted, spelled as in the lock) of every github input in the lock,
/// including the transitive ones: nix fetches those too, so a private one needs credentials
/// just the same. The spelling matters because nix matches `access-tokens` keys
/// case-sensitively. The lock is controlled by the repository, so owners that are not valid
/// GitHub account names are left out: they end up in an API route and in nix's config.
pub(super) fn github_owners(lock: &FlakeLock) -> Vec<String> {
    let owners: BTreeSet<String> = lock
        .nodes
        .values()
        .flat_map(|node| [node.original.as_ref(), node.locked.as_ref()])
        .flatten()
        .filter(|flake_ref| flake_ref.ref_type == "github")
        .filter_map(|flake_ref| flake_ref.owner.clone())
        .filter(|owner| is_valid_owner(owner))
        .collect();

    owners.into_iter().collect()
}

/// GitHub repository names consist of ASCII letters, digits, hyphens, underscores and dots, and
/// are never just dots.
pub(super) fn is_valid_repo(repo: &str) -> bool {
    !repo.is_empty()
        && repo.bytes().any(|b| b != b'.')
        && repo
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// GitHub account names consist of ASCII letters, digits and hyphens.
pub(super) fn is_valid_owner(owner: &str) -> bool {
    !owner.is_empty()
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Direct (non-`follows`) root inputs whose `original` is a github ref, a generic git ref
/// (nix's host-agnostic `git+https://`/`git+ssh://` fetcher), or a plain https tarball ref,
/// paired with their local alias.
pub(super) fn root_inputs(lock: &FlakeLock) -> Vec<RootInput<'_>> {
    let mut result = Vec::new();
    let root_node = match lock.nodes.get(&lock.root) {
        Some(n) => n,
        None => return result,
    };

    for (alias, input_ref) in &root_node.inputs {
        // Only direct node references are handled; "follows" chains (array values) alias
        // another node's lock rather than owning one, so they're skipped.
        let node_name = match input_ref.as_str() {
            Some(n) => n,
            None => continue,
        };

        let node = match lock.nodes.get(node_name) {
            Some(n) => n,
            None => continue,
        };

        let original = match &node.original {
            Some(o) => o,
            None => continue,
        };

        let rev = node.locked.as_ref().and_then(|l| l.rev.as_deref());

        match original.ref_type.as_str() {
            "github" => {
                let (owner, repo) = match (&original.owner, &original.repo) {
                    (Some(o), Some(r)) => (o.as_str(), r.as_str()),
                    _ => continue,
                };

                result.push(RootInput::Github(GithubRootInput {
                    alias,
                    owner,
                    repo,
                    git_ref: original.git_ref.as_deref(),
                    rev,
                }));
            }
            "git" => {
                let url = match &original.url {
                    Some(u) => u.as_str(),
                    None => continue,
                };

                result.push(RootInput::Git(GitRootInput {
                    alias,
                    url,
                    git_ref: original.git_ref.as_deref(),
                    rev,
                }));
            }
            "tarball" => {
                let url = match &original.url {
                    Some(u) => u.as_str(),
                    None => continue,
                };

                let locked_url = node.locked.as_ref().and_then(|l| l.url.as_deref());

                result.push(RootInput::Tarball(TarballRootInput {
                    alias,
                    url,
                    locked_url,
                }));
            }
            _ => continue,
        }
    }

    result
}

/// How deep a `follows` path may lead through other `follows` before it is given up on.
const MAX_FOLLOWS_DEPTH: usize = 16;

/// The node an entry of `inputs` leads to: either the node's id, or a `follows` path of input
/// names that starts at the root and may itself pass through `follows`.
fn resolve_input<'a>(
    lock: &'a FlakeLock,
    value: &serde_json::Value,
    depth: usize,
) -> Option<&'a str> {
    if depth > MAX_FOLLOWS_DEPTH {
        return None;
    }

    match value {
        serde_json::Value::String(id) => lock.nodes.get_key_value(id).map(|(id, _)| id.as_str()),
        serde_json::Value::Array(path) => {
            let mut current = lock.nodes.get_key_value(&lock.root)?.0.as_str();
            for name in path {
                let next = lock.nodes.get(current)?.inputs.get(name.as_str()?)?;
                current = resolve_input(lock, next, depth + 1)?;
            }
            Some(current)
        }
        _ => None,
    }
}

/// A node that can be reached from the root, named by the inputs that lead to it. The ids in
/// the lock (`nixpkgs_3`) say nothing about that and change whenever the lock is rewritten.
pub(super) struct ReachableNode<'a> {
    /// Input names from the root to the node along the shortest way. Of several equally short
    /// ones, the first found wins, visiting the inputs of each node in name order.
    pub(super) path: Vec<String>,
    /// The other ways to the node: through `follows`, or through a longer chain of inputs.
    pub(super) other_paths: Vec<Vec<String>>,
    pub(super) node: &'a FlakeNode,
}

/// Every node reachable from the root other than the root itself, nearest first.
pub(super) fn reachable_nodes(lock: &FlakeLock) -> Vec<ReachableNode<'_>> {
    let mut result: Vec<ReachableNode<'_>> = Vec::new();
    let Some(root) = lock.nodes.get(&lock.root) else {
        return result;
    };

    let mut index: HashMap<&str, usize> = HashMap::new();
    // `None` is the root, `Some(i)` the node at `result[i]`. The nodes are expanded in the
    // order they were found, which makes the first path to a node a shortest one.
    let mut cursor: Option<usize> = None;

    loop {
        let (path, node) = match cursor {
            None => (Vec::new(), root),
            Some(i) => (result[i].path.clone(), result[i].node),
        };

        let mut names: Vec<&String> = node.inputs.keys().collect();
        names.sort();

        for name in names {
            let Some(id) = resolve_input(lock, &node.inputs[name], 0) else {
                continue;
            };
            if id == lock.root {
                continue;
            }

            let mut reached_by = path.clone();
            reached_by.push(name.clone());

            match index.get(id) {
                Some(&i) => result[i].other_paths.push(reached_by),
                None => {
                    index.insert(id, result.len());
                    result.push(ReachableNode {
                        path: reached_by,
                        other_paths: Vec::new(),
                        node: &lock.nodes[id],
                    });
                }
            }
        }

        let next = cursor.map_or(0, |i| i + 1);
        if next >= result.len() {
            break;
        }
        cursor = Some(next);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(nodes: &[ReachableNode<'_>]) -> Vec<String> {
        nodes.iter().map(|n| n.path.join("/")).collect()
    }

    #[test]
    fn test_reachable_nodes_are_named_by_their_shortest_path() {
        let content = r#"
        {
          "nodes": {
            "nixpkgs": {},
            "nixpkgs_2": {},
            "systems": {},
            "systems_2": {},
            "treefmt-nix": { "inputs": { "nixpkgs": "nixpkgs_2" } },
            "tool": { "inputs": { "systems": "systems_2", "treefmt-nix": "treefmt-nix" } },
            "root": { "inputs": { "nixpkgs": "nixpkgs", "tool": "tool", "systems": "systems" } }
          },
          "root": "root"
        }
        "#;
        let lock: FlakeLock = serde_json::from_str(content).unwrap();

        assert_eq!(
            paths(&reachable_nodes(&lock)),
            [
                "nixpkgs",
                "systems",
                "tool",
                "tool/systems",
                "tool/treefmt-nix",
                "tool/treefmt-nix/nixpkgs"
            ]
        );
    }

    #[test]
    fn test_reachable_nodes_resolve_follows_and_keep_the_other_paths() {
        let content = r#"
        {
          "nodes": {
            "nixpkgs": {},
            "tool": { "inputs": { "nixpkgs": ["nixpkgs"] } },
            "other": { "inputs": { "nixpkgs": ["tool", "nixpkgs"], "tool": ["tool"] } },
            "root": { "inputs": { "nixpkgs": "nixpkgs", "tool": "tool", "other": "other" } }
          },
          "root": "root"
        }
        "#;
        let lock: FlakeLock = serde_json::from_str(content).unwrap();
        let nodes = reachable_nodes(&lock);

        assert_eq!(paths(&nodes), ["nixpkgs", "other", "tool"]);
        let others: Vec<String> = nodes[0].other_paths.iter().map(|p| p.join("/")).collect();
        assert_eq!(others, ["other/nixpkgs", "tool/nixpkgs"]);
    }

    #[test]
    fn test_reachable_nodes_survive_cycles_and_dangling_references() {
        let content = r#"
        {
          "nodes": {
            "a": { "inputs": { "b": "b", "missing": "nowhere", "loop": ["a", "b", "a"] } },
            "b": { "inputs": { "a": "a", "self": ["b"] } },
            "root": { "inputs": { "a": "a" } }
          },
          "root": "root"
        }
        "#;
        let lock: FlakeLock = serde_json::from_str(content).unwrap();

        assert_eq!(paths(&reachable_nodes(&lock)), ["a", "a/b"]);
    }

    #[test]
    fn test_github_owners_includes_transitive_inputs_once() {
        let content = r#"
        {
          "nodes": {
            "nixpkgs": {
              "locked": { "type": "github", "owner": "NixOS", "repo": "nixpkgs", "rev": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
              "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs" }
            },
            "internal": {
              "inputs": { "lib": "lib" },
              "locked": { "type": "github", "owner": "acme", "repo": "internal", "rev": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" },
              "original": { "type": "github", "owner": "acme", "repo": "internal" }
            },
            "lib": {
              "locked": { "type": "github", "owner": "other", "repo": "lib", "rev": "cccccccccccccccccccccccccccccccccccccccc" },
              "original": { "type": "github", "owner": "other", "repo": "lib" }
            },
            "core": {
              "locked": { "type": "git", "rev": "dddddddddddddddddddddddddddddddddddddddd" },
              "original": { "type": "git", "url": "https://tangled.org/@tangled.org/core" }
            },
            "root": { "inputs": { "nixpkgs": "nixpkgs", "internal": "internal", "core": "core" } }
          },
          "root": "root"
        }
        "#;
        let lock: FlakeLock = serde_json::from_str(content).unwrap();

        assert_eq!(github_owners(&lock), vec!["NixOS", "acme", "other"]);
    }

    #[test]
    fn test_github_owners_skips_names_that_are_not_valid_accounts() {
        let content = r#"
        {
          "nodes": {
            "newline": {
              "original": { "type": "github", "owner": "evil\nmax-jobs = 0", "repo": "x" }
            },
            "space": {
              "original": { "type": "github", "owner": "a b", "repo": "x" }
            },
            "route": {
              "original": { "type": "github", "owner": "a/b?c#d", "repo": "x" }
            },
            "dots": {
              "original": { "type": "github", "owner": "..", "repo": "x" }
            },
            "empty": {
              "original": { "type": "github", "owner": "", "repo": "x" }
            },
            "ok": {
              "original": { "type": "github", "owner": "my-org", "repo": "x" }
            },
            "root": { "inputs": {} }
          },
          "root": "root"
        }
        "#;
        let lock: FlakeLock = serde_json::from_str(content).unwrap();

        assert_eq!(github_owners(&lock), vec!["my-org"]);
    }
}
