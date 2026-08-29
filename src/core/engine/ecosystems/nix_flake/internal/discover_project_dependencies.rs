use std::collections::HashSet;

use anyhow::Result;

use crate::core::engine::repository::ProjectRepositorySnapshot;
use crate::core::engine::{DiscoveredDependency, PURL};

use super::flake_lock::{self, FlakeLock, RootInput};
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

    let mut deps = Vec::new();
    let mut seen = HashSet::new();

    for input in flake_lock::root_inputs(&lock) {
        let (normalized, git_ref, rev) = match &input {
            RootInput::Github(gh) => (
                helpers::format_github_ref(gh.owner, gh.repo, gh.git_ref),
                gh.git_ref,
                gh.rev,
            ),
            RootInput::Git(g) => (helpers::format_git_ref(g.url, g.git_ref), g.git_ref, g.rev),
            RootInput::Tarball(t) => (helpers::format_tarball_ref(t.url), None, t.locked_url),
        };

        let rev = match rev {
            Some(r) => r.to_string(),
            None => continue,
        };

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
            requirement: git_ref.unwrap_or_default().to_string(),
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
    fn test_parse_flake_lock_direct_git_input() {
        let content = r#"
        {
          "nodes": {
            "core": {
              "locked": { "type": "git", "rev": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
              "original": { "type": "git", "url": "https://tangled.org/@tangled.org/core" }
            },
            "root": { "inputs": { "core": "core" } }
          },
          "root": "root",
          "version": 7
        }
        "#;

        let deps = parse_flake_lock(content);
        assert_eq!(deps.len(), 1);
        assert_eq!(
            deps[0].purl.name,
            "git+https://tangled.org/@tangled.org/core"
        );
        assert_eq!(
            deps[0].purl.version.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(deps[0].requirement, "");
    }

    #[test]
    fn test_parse_flake_lock_direct_git_input_with_ref() {
        let content = r#"
        {
          "nodes": {
            "core": {
              "locked": { "type": "git", "rev": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
              "original": { "type": "git", "url": "https://tangled.org/@tangled.org/core", "ref": "main" }
            },
            "root": { "inputs": { "core": "core" } }
          },
          "root": "root",
          "version": 7
        }
        "#;

        let deps = parse_flake_lock(content);
        assert_eq!(deps.len(), 1);
        assert_eq!(
            deps[0].purl.name,
            "git+https://tangled.org/@tangled.org/core?ref=main"
        );
        assert_eq!(deps[0].requirement, "main");
    }

    #[test]
    fn test_parse_flake_lock_direct_tarball_input() {
        let content = r#"
        {
          "nodes": {
            "determinate": {
              "locked": { "type": "tarball", "url": "https://api.flakehub.com/f/pinned/DeterminateSystems/determinate/3.15.2/uuid/source.tar.gz" },
              "original": { "type": "tarball", "url": "https://flakehub.com/f/DeterminateSystems/determinate/3.tar.gz" }
            },
            "root": { "inputs": { "determinate": "determinate" } }
          },
          "root": "root",
          "version": 7
        }
        "#;

        let deps = parse_flake_lock(content);
        assert_eq!(deps.len(), 1);
        assert_eq!(
            deps[0].purl.name,
            "tarball+https://flakehub.com/f/DeterminateSystems/determinate/3.tar.gz"
        );
        assert_eq!(
            deps[0].purl.version.as_deref(),
            Some("https://api.flakehub.com/f/pinned/DeterminateSystems/determinate/3.15.2/uuid/source.tar.gz")
        );
        assert_eq!(deps[0].requirement, "");
    }

    #[test]
    fn test_parse_flake_lock_skips_unsupported_input_type() {
        let content = r#"
        {
          "nodes": {
            "nixpkgs": {
              "locked": { "type": "indirect", "rev": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
              "original": { "type": "indirect", "id": "nixpkgs" }
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
