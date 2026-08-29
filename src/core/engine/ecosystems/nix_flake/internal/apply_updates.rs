use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use crate::core::engine::repository::{FileModification, FileState, ProjectRepositorySnapshot};
use crate::core::engine::UpdateTarget;

use super::flake_lock::{self, FlakeLock};
use super::helpers;

pub async fn run(
    repo: &dyn ProjectRepositorySnapshot,
    temp_dir: &Path,
    targets: &[UpdateTarget],
) -> Result<Vec<FileModification>> {
    let flake_lock_content = match repo.read_file("flake.lock").await {
        Ok(c) => c,
        Err(_) => return Ok(Vec::new()),
    };

    let lock: FlakeLock = match serde_json::from_str(&flake_lock_content) {
        Ok(l) => l,
        Err(_) => return Ok(Vec::new()),
    };

    let target_map: HashMap<&str, &UpdateTarget> =
        targets.iter().map(|t| (t.name.as_str(), t)).collect();

    let overrides = override_args_for_targets(&lock, &target_map);
    if overrides.is_empty() {
        return Ok(Vec::new());
    }

    let flake_nix_content = repo
        .read_file("flake.nix")
        .await
        .context("Missing flake.nix")?;

    fs::write(temp_dir.join("flake.nix"), &flake_nix_content)?;
    fs::write(temp_dir.join("flake.lock"), &flake_lock_content)?;

    let temp_dir_owned = temp_dir.to_path_buf();
    let overrides_owned = overrides.clone();
    let status = tokio::task::spawn_blocking(move || {
        let mut cmd = Command::new("nix");
        cmd.arg("--extra-experimental-features")
            .arg("nix-command flakes")
            .arg("flake")
            .arg("lock")
            .current_dir(&temp_dir_owned);
        for (alias, override_ref) in &overrides_owned {
            cmd.arg("--override-input").arg(alias).arg(override_ref);
        }
        cmd.status()
    })
    .await
    .map_err(|e| anyhow::anyhow!("Task join error: {}", e))??;

    if !status.success() {
        tracing::warn!("nix flake lock --override-input failed, continuing anyway...");
    }

    let updated_lock = fs::read_to_string(temp_dir.join("flake.lock"))?;
    if updated_lock != flake_lock_content {
        Ok(vec![FileModification {
            path: "flake.lock".to_string(),
            state: FileState::Write(updated_lock),
        }])
    } else {
        Ok(Vec::new())
    }
}

/// For each root input whose `original` ref matches an approved target, returns
/// `(local_alias, override_flake_ref)` pairs suitable for `nix flake lock --override-input`.
/// `override_flake_ref` pins to the target's exact approved SHA, not the ref's current tip.
fn override_args_for_targets(
    lock: &FlakeLock,
    target_map: &HashMap<&str, &UpdateTarget>,
) -> Vec<(String, String)> {
    let mut overrides = Vec::new();

    for input in flake_lock::root_inputs(lock) {
        let normalized = match &input {
            flake_lock::RootInput::Github(gh) => {
                helpers::format_github_ref(gh.owner, gh.repo, gh.git_ref)
            }
            flake_lock::RootInput::Git(g) => helpers::format_git_ref(g.url, g.git_ref),
            flake_lock::RootInput::Tarball(t) => helpers::format_tarball_ref(t.url),
        };

        let target = match target_map.get(normalized.as_str()) {
            Some(t) => t,
            None => continue,
        };

        let override_ref = match &input {
            flake_lock::RootInput::Github(gh) => format!(
                "github:{}/{}/{}",
                gh.owner, gh.repo, target.target_version.version
            ),
            flake_lock::RootInput::Git(g) => {
                format!("git+{}?rev={}", g.url, target.target_version.version)
            }
            flake_lock::RootInput::Tarball(_) => {
                format!("tarball+{}", target.target_version.version)
            }
        };

        overrides.push((input.alias().to_string(), override_ref));
    }

    overrides.sort();
    overrides
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::{PackageInfo, RequirementVersion};

    fn make_target(name: &str, sha: &str) -> UpdateTarget {
        UpdateTarget {
            name: name.to_string(),
            current_version: RequirementVersion {
                requirement: "old-ref".to_string(),
                version: "old-sha".to_string(),
            },
            target_version: RequirementVersion {
                requirement: sha.to_string(),
                version: sha.to_string(),
            },
            latest_version: sha.to_string(),
            package_info: PackageInfo { repo_url: None },
            minimum_release_age: None,
        }
    }

    #[test]
    fn test_override_args_matches_direct_github_input() {
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

        let target = make_target(
            "github:NixOS/nixpkgs/nixpkgs-unstable",
            "cccccccccccccccccccccccccccccccccccccccc",
        );
        let target_map: HashMap<&str, &UpdateTarget> = [(target.name.as_str(), &target)].into();

        let overrides = override_args_for_targets(&lock, &target_map);
        assert_eq!(
            overrides,
            vec![(
                "nixpkgs".to_string(),
                "github:NixOS/nixpkgs/cccccccccccccccccccccccccccccccccccccccc".to_string()
            )]
        );
    }

    #[test]
    fn test_override_args_skips_follows_chain() {
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

        let target = make_target(
            "github:numtide/flake-utils",
            "cccccccccccccccccccccccccccccccccccccccc",
        );
        let target_map: HashMap<&str, &UpdateTarget> = [(target.name.as_str(), &target)].into();

        let overrides = override_args_for_targets(&lock, &target_map);
        assert_eq!(
            overrides,
            vec![(
                "flake-utils".to_string(),
                "github:numtide/flake-utils/cccccccccccccccccccccccccccccccccccccccc".to_string()
            )]
        );
    }

    #[test]
    fn test_override_args_matches_direct_tarball_input() {
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

        let target = make_target(
            "tarball+https://flakehub.com/f/DeterminateSystems/determinate/3.tar.gz",
            "https://api.flakehub.com/f/pinned/DeterminateSystems/determinate/3.22.2/other-uuid/source.tar.gz",
        );
        let target_map: HashMap<&str, &UpdateTarget> = [(target.name.as_str(), &target)].into();

        let overrides = override_args_for_targets(&lock, &target_map);
        assert_eq!(
            overrides,
            vec![(
                "determinate".to_string(),
                "tarball+https://api.flakehub.com/f/pinned/DeterminateSystems/determinate/3.22.2/other-uuid/source.tar.gz".to_string()
            )]
        );
    }

    #[test]
    fn test_override_args_skips_unsupported_input_type() {
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

        let target = make_target("indirect:nixpkgs", "sha");
        let target_map: HashMap<&str, &UpdateTarget> = [(target.name.as_str(), &target)].into();

        assert!(override_args_for_targets(&lock, &target_map).is_empty());
    }

    #[test]
    fn test_override_args_matches_direct_git_input() {
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

        let target = make_target(
            "git+https://tangled.org/@tangled.org/core",
            "cccccccccccccccccccccccccccccccccccccccc",
        );
        let target_map: HashMap<&str, &UpdateTarget> = [(target.name.as_str(), &target)].into();

        let overrides = override_args_for_targets(&lock, &target_map);
        assert_eq!(
            overrides,
            vec![(
                "core".to_string(),
                "git+https://tangled.org/@tangled.org/core?rev=cccccccccccccccccccccccccccccccccccccccc".to_string()
            )]
        );
    }

    #[test]
    fn test_override_args_only_emits_matched_targets() {
        let content = r#"
        {
          "nodes": {
            "nixpkgs": {
              "locked": { "type": "github", "owner": "NixOS", "repo": "nixpkgs", "rev": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
              "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs", "ref": "nixpkgs-unstable" }
            },
            "flake-utils": {
              "locked": { "type": "github", "owner": "numtide", "repo": "flake-utils", "rev": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" },
              "original": { "type": "github", "owner": "numtide", "repo": "flake-utils" }
            },
            "root": { "inputs": { "nixpkgs": "nixpkgs", "flake-utils": "flake-utils" } }
          },
          "root": "root",
          "version": 7
        }
        "#;
        let lock: FlakeLock = serde_json::from_str(content).unwrap();

        let target = make_target(
            "github:NixOS/nixpkgs/nixpkgs-unstable",
            "cccccccccccccccccccccccccccccccccccccccc",
        );
        let target_map: HashMap<&str, &UpdateTarget> = [(target.name.as_str(), &target)].into();

        let overrides = override_args_for_targets(&lock, &target_map);
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].0, "nixpkgs");
    }
}
