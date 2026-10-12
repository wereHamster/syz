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
/// case-sensitively.
pub(super) fn github_owners(lock: &FlakeLock) -> Vec<String> {
    let owners: BTreeSet<String> = lock
        .nodes
        .values()
        .flat_map(|node| [node.original.as_ref(), node.locked.as_ref()])
        .flatten()
        .filter(|flake_ref| flake_ref.ref_type == "github")
        .filter_map(|flake_ref| flake_ref.owner.clone())
        .collect();

    owners.into_iter().collect()
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
