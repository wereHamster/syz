use crate::core::engine::repository::ProjectRepositorySnapshot;

use super::flake_lock::{self, FlakeLock, RootInput};
use super::helpers;

/// Resolves the local flake input alias (e.g. `"nixpkgs"`) whose normalized ref matches
/// `target_name` (e.g. `"github:NixOS/nixpkgs/nixpkgs-unstable"` or
/// `"git+https://tangled.org/@tangled.org/core"`), by re-reading `flake.lock`.
pub async fn run(repo: &dyn ProjectRepositorySnapshot, target_name: &str) -> Option<String> {
    let content = repo.read_file("flake.lock").await.ok()?;
    let lock: FlakeLock = serde_json::from_str(&content).ok()?;
    find_alias(&lock, target_name)
}

fn find_alias(lock: &FlakeLock, target_name: &str) -> Option<String> {
    flake_lock::root_inputs(lock)
        .into_iter()
        .find(|input| {
            let normalized = match input {
                RootInput::Github(gh) => helpers::format_github_ref(gh.owner, gh.repo, gh.git_ref),
                RootInput::Git(g) => helpers::format_git_ref(g.url, g.git_ref),
                RootInput::Tarball(t) => helpers::format_tarball_ref(t.url),
            };
            normalized == target_name
        })
        .map(|input| input.alias().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_alias_matches_direct_github_input() {
        let content = r#"
        {
          "nodes": {
            "nixpkgs": {
              "locked": { "type": "github", "owner": "NixOS", "repo": "nixpkgs", "rev": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
              "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs", "ref": "nixpkgs-unstable" }
            },
            "root": { "inputs": { "nixpkgs": "nixpkgs" } }
          },
          "root": "root",
          "version": 7
        }
        "#;
        let lock: FlakeLock = serde_json::from_str(content).unwrap();

        assert_eq!(
            find_alias(&lock, "github:NixOS/nixpkgs/nixpkgs-unstable"),
            Some("nixpkgs".to_string())
        );
    }

    #[test]
    fn test_find_alias_skips_follows_chain() {
        let content = r#"
        {
          "nodes": {
            "nixpkgs": {
              "locked": { "type": "github", "owner": "NixOS", "repo": "nixpkgs", "rev": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
              "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs" }
            },
            "flake-utils": {
              "inputs": { "nixpkgs": ["nixpkgs"] },
              "locked": { "type": "github", "owner": "numtide", "repo": "flake-utils", "rev": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" },
              "original": { "type": "github", "owner": "numtide", "repo": "flake-utils" }
            },
            "root": { "inputs": { "flake-utils": "flake-utils", "nixpkgs": ["flake-utils", "nixpkgs"] } }
          },
          "root": "root",
          "version": 7
        }
        "#;
        let lock: FlakeLock = serde_json::from_str(content).unwrap();

        assert_eq!(
            find_alias(&lock, "github:numtide/flake-utils"),
            Some("flake-utils".to_string())
        );
        // "nixpkgs" here is only reachable via a follows chain, not a direct root input.
        assert_eq!(find_alias(&lock, "github:NixOS/nixpkgs"), None);
    }

    #[test]
    fn test_find_alias_matches_direct_git_input() {
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
        let lock: FlakeLock = serde_json::from_str(content).unwrap();

        assert_eq!(
            find_alias(&lock, "git+https://tangled.org/@tangled.org/core"),
            Some("core".to_string())
        );
    }

    #[test]
    fn test_find_alias_matches_direct_tarball_input() {
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
        let lock: FlakeLock = serde_json::from_str(content).unwrap();

        assert_eq!(
            find_alias(
                &lock,
                "tarball+https://flakehub.com/f/DeterminateSystems/determinate/3.tar.gz"
            ),
            Some("determinate".to_string())
        );
    }

    #[test]
    fn test_find_alias_skips_unsupported_input_type() {
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
        let lock: FlakeLock = serde_json::from_str(content).unwrap();

        assert_eq!(find_alias(&lock, "indirect:nixpkgs"), None);
    }

    #[test]
    fn test_find_alias_returns_none_when_no_match() {
        let content = r#"
        {
          "nodes": {
            "nixpkgs": {
              "locked": { "type": "github", "owner": "NixOS", "repo": "nixpkgs", "rev": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
              "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs", "ref": "nixpkgs-unstable" }
            },
            "root": { "inputs": { "nixpkgs": "nixpkgs" } }
          },
          "root": "root",
          "version": 7
        }
        "#;
        let lock: FlakeLock = serde_json::from_str(content).unwrap();

        assert_eq!(find_alias(&lock, "github:someone/else"), None);
    }
}
