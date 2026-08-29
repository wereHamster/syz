use std::collections::HashSet;

use anyhow::Result;

use crate::core::engine::repository::ProjectRepositorySnapshot;
use crate::core::engine::{DiscoveredDependency, PURL};

use super::flake_lock::FlakeLock;
use super::helpers;

pub async fn run(repo: &dyn ProjectRepositorySnapshot) -> Result<Vec<DiscoveredDependency>> {
    let content = match repo.read_file("flake.lock").await {
        Ok(c) => c,
        Err(_) => return Ok(Vec::new()),
    };

    Ok(parse_flake_lock(&content))
}

fn parse_flake_lock(content: &str) -> Vec<DiscoveredDependency> {
    let lock: FlakeLock = match serde_json::from_str(content) {
        Ok(l) => l,
        Err(_) => return Vec::new(),
    };

    let root_node = match lock.nodes.get(&lock.root) {
        Some(n) => n,
        None => return Vec::new(),
    };

    let mut deps = Vec::new();
    let mut seen = HashSet::new();

    for input_ref in root_node.inputs.values() {
        // Only direct node references are handled; "follows" chains (array values) alias
        // another node's lock rather than owning one, so they're skipped for now.
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

        if original.ref_type != "github" {
            continue;
        }

        let owner = match &original.owner {
            Some(o) => o.clone(),
            None => continue,
        };
        let repo_name = match &original.repo {
            Some(r) => r.clone(),
            None => continue,
        };
        let git_ref = original.git_ref.clone();

        let locked = match &node.locked {
            Some(l) => l,
            None => continue,
        };
        let rev = match &locked.rev {
            Some(r) => r.clone(),
            None => continue,
        };

        let normalized = helpers::format_github_ref(&owner, &repo_name, git_ref.as_deref());
        if !seen.insert(normalized.clone()) {
            continue;
        }

        deps.push(DiscoveredDependency {
            purl: PURL {
                ecosystem: "nix-flake".to_string(),
                namespace: None,
                name: normalized,
                subpath: None,
                version: Some(rev),
            },
            requirement: git_ref.unwrap_or_default(),
            minimum_release_age: None,
        });
    }

    deps.sort_by(|a, b| a.purl.name.cmp(&b.purl.name));
    deps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_flake_lock_direct_github_inputs() {
        let content = r#"
        {
          "nodes": {
            "flake-utils": {
              "locked": { "type": "github", "owner": "numtide", "repo": "flake-utils", "rev": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
              "original": { "type": "github", "owner": "numtide", "repo": "flake-utils" }
            },
            "nixpkgs": {
              "locked": { "type": "github", "owner": "NixOS", "repo": "nixpkgs", "rev": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" },
              "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs", "ref": "nixpkgs-unstable" }
            },
            "root": { "inputs": { "flake-utils": "flake-utils", "nixpkgs": "nixpkgs" } }
          },
          "root": "root",
          "version": 7
        }
        "#;

        let deps = parse_flake_lock(content);
        assert_eq!(deps.len(), 2);

        assert_eq!(deps[0].purl.name, "github:NixOS/nixpkgs/nixpkgs-unstable");
        assert_eq!(
            deps[0].purl.version.as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
        assert_eq!(deps[0].requirement, "nixpkgs-unstable");

        assert_eq!(deps[1].purl.name, "github:numtide/flake-utils");
        assert_eq!(
            deps[1].purl.version.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(deps[1].requirement, "");
    }

    #[test]
    fn test_parse_flake_lock_skips_non_github_input() {
        let content = r#"
        {
          "nodes": {
            "nixpkgs": {
              "locked": { "type": "git", "rev": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
              "original": { "type": "git", "url": "https://example.com/nixpkgs.git" }
            },
            "root": { "inputs": { "nixpkgs": "nixpkgs" } }
          },
          "root": "root",
          "version": 7
        }
        "#;

        assert!(parse_flake_lock(content).is_empty());
    }

    #[test]
    fn test_parse_flake_lock_skips_follows_chain() {
        let content = r#"
        {
          "nodes": {
            "nixpkgs": {
              "locked": { "type": "github", "owner": "NixOS", "repo": "nixpkgs", "rev": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
              "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs" }
            },
            "flake-utils": {
              "inputs": { "nixpkgs": ["nixpkgs"] },
              "locked": { "type": "github", "owner": "numtide", "repo": "flake-utils", "rev": "cccccccccccccccccccccccccccccccccccccccc" },
              "original": { "type": "github", "owner": "numtide", "repo": "flake-utils" }
            },
            "root": { "inputs": { "flake-utils": "flake-utils", "nixpkgs": ["flake-utils", "nixpkgs"] } }
          },
          "root": "root",
          "version": 7
        }
        "#;

        let deps = parse_flake_lock(content);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].purl.name, "github:numtide/flake-utils");
    }

    #[test]
    fn test_parse_flake_lock_dedupes_duplicate_normalized_url() {
        let content = r#"
        {
          "nodes": {
            "nixpkgs": {
              "locked": { "type": "github", "owner": "NixOS", "repo": "nixpkgs", "rev": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
              "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs" }
            },
            "nixpkgs-alias": {
              "locked": { "type": "github", "owner": "NixOS", "repo": "nixpkgs", "rev": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
              "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs" }
            },
            "root": { "inputs": { "nixpkgs": "nixpkgs", "nixpkgs-alias": "nixpkgs-alias" } }
          },
          "root": "root",
          "version": 7
        }
        "#;

        let deps = parse_flake_lock(content);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].purl.name, "github:NixOS/nixpkgs");
    }
}
