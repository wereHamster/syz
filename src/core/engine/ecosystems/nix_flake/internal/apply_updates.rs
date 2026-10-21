use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use futures::{stream, StreamExt};

use crate::core::clients::github::GitHub;
use crate::core::engine::repository::{FileModification, FileState, ProjectRepositorySnapshot};
use crate::core::engine::UpdateTarget;

use super::flake_lock::{self, FlakeLock, RootInput};
use super::helpers;

pub async fn run(
    github: &GitHub,
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

    // The overridden inputs bring the inputs of their new revision along, which may belong to
    // owners that the current lock doesn't mention yet.
    let mut owners: BTreeSet<String> = flake_lock::github_owners(&lock).into_iter().collect();
    let sources = github_override_sources(&lock, &target_map);
    owners.extend(dependency_lock_owners(github, &sources).await);
    let owners: Vec<String> = owners.into_iter().collect();

    let NixAccess {
        config: nix_config,
        secrets,
    } = NixAccess::for_owners(github, &owners).await;

    let temp_dir_owned = temp_dir.to_path_buf();
    let overrides_owned = overrides.clone();
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = Command::new("nix");
        cmd.arg("--extra-experimental-features")
            .arg("nix-command flakes")
            .arg("flake")
            .arg("lock")
            .current_dir(&temp_dir_owned);
        for (alias, override_ref) in &overrides_owned {
            cmd.arg("--override-input").arg(alias).arg(override_ref);
        }
        if let Some(config) = nix_config {
            cmd.env("NIX_CONFIG", config);
        }
        cmd.output()
    })
    .await
    .map_err(|e| anyhow::anyhow!("Task join error: {}", e))??;

    if !output.status.success() {
        let stderr = redact(&String::from_utf8_lossy(&output.stderr), &secrets);
        anyhow::bail!(
            "nix flake lock failed in {:?} ({}): {}",
            temp_dir,
            output.status,
            stderr.trim()
        );
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

/// How nix is made to authenticate to the owners of the inputs it fetches.
pub struct NixAccess {
    /// The value for `NIX_CONFIG`, if there is anything to add to the host's own.
    pub config: Option<String>,
    /// Credentials that must not reach a log.
    pub secrets: Vec<String>,
}

impl NixAccess {
    /// Mints tokens for `owners` and builds the nix config that makes use of them.
    pub async fn for_owners(github: &GitHub, owners: &[String]) -> Self {
        let tokens = github_tokens(github, owners).await;
        let inherited_config = std::env::var("NIX_CONFIG").unwrap_or_default();
        let config = access_tokens_config(&tokens).map(|config| {
            // Through the environment rather than `--option` or a config file, so the tokens are
            // neither visible in the process list nor left on disk.
            if inherited_config.is_empty() {
                config
            } else {
                format!("{inherited_config}\n{config}")
            }
        });

        let mut secrets = secrets_in_nix_config(&inherited_config);
        secrets.extend(tokens.iter().map(|(_, token)| token.clone()));

        Self { config, secrets }
    }
}

/// How many tokens are minted at the same time.
const TOKEN_CONCURRENCY: usize = 4;

/// Mints a read-only token for each of `owners` the GitHub App is installed for, as
/// `(owner, token)` pairs. Public inputs need none, so owners without an installation, or whose
/// token can't be created, are skipped and nix is left to fetch their inputs anonymously.
///
/// GitHub treats owners case-insensitively, so spellings of the same owner share one token. Nix
/// still matches its keys case-sensitively, so each spelling gets its own pair.
async fn github_tokens(github: &GitHub, owners: &[String]) -> Vec<(String, String)> {
    let accounts = group_by_account(owners);

    // Collected first, so that the stream doesn't carry the closure's type: rustc can't prove
    // that closure general enough once it is part of an `async_trait` future.
    let lookups: Vec<_> = accounts
        .iter()
        .map(|spellings| mint_token(github, &spellings[0]))
        .collect();
    let minted: Vec<Option<String>> = stream::iter(lookups)
        .buffered(TOKEN_CONCURRENCY)
        .collect()
        .await;

    accounts
        .iter()
        .zip(minted)
        .filter_map(|(spellings, token)| Some((spellings, token?)))
        .flat_map(|(spellings, token)| {
            spellings
                .iter()
                .map(move |owner| (owner.clone(), token.clone()))
        })
        .collect()
}

/// Mints the token of one owner, or `None` (after a warning if it failed) when there is none.
async fn mint_token(github: &GitHub, owner: &str) -> Option<String> {
    match github.contents_read_token(owner).await {
        Ok(token) => token,
        Err(e) => {
            tracing::warn!("No GitHub token for flake inputs of '{}': {:#}", owner, e);
            None
        }
    }
}

/// Groups the spellings of `owners` that name the same account.
fn group_by_account(owners: &[String]) -> Vec<Vec<String>> {
    let mut accounts: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for owner in owners {
        let spellings = accounts.entry(owner.to_lowercase()).or_default();
        if !spellings.contains(owner) {
            spellings.push(owner.clone());
        }
    }
    accounts.into_values().collect()
}

/// The nix config line that makes nix authenticate to github.com with a token per owner, or
/// `None` without tokens. Nix picks the token whose key is the longest prefix of
/// `github.com/{owner}/{repo}`, and `extra-` appends to any `access-tokens` already configured
/// on the host instead of replacing them.
fn access_tokens_config(tokens: &[(String, String)]) -> Option<String> {
    if tokens.is_empty() {
        return None;
    }

    let entries: Vec<String> = tokens
        .iter()
        .map(|(owner, token)| format!("github.com/{owner}={token}"))
        .collect();
    Some(format!("extra-access-tokens = {}", entries.join(" ")))
}

/// The credentials in the `access-tokens` settings of a nix config, e.g. an inherited
/// `NIX_CONFIG`. Nix echoes a config line it can't parse, so these must not reach a log either.
/// This is deliberately generous: of every `access-tokens` entry, the part after the `=` is
/// taken (plus the part after a `type:` prefix such as `PAT:`), or the whole entry if it has
/// no `=`, so that a malformed line is covered too.
fn secrets_in_nix_config(config: &str) -> Vec<String> {
    let mut secrets = Vec::new();

    for line in config.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.strip_prefix("extra-").unwrap_or(key) != "access-tokens" {
            continue;
        }

        for entry in value.split_whitespace() {
            let secret = entry.split_once('=').map_or(entry, |(_, secret)| secret);
            secrets.push(secret.to_string());
            if let Some((_, bare)) = secret.rsplit_once(':') {
                secrets.push(bare.to_string());
            }
        }
    }

    secrets
}

/// Replaces the `secrets` in `text`, so that nix's output can be logged. The longest secrets go
/// first: when one is a prefix of another, replacing the short one first would leave the rest of
/// the long one behind.
pub(super) fn redact(text: &str, secrets: &[String]) -> String {
    let mut secrets: Vec<&str> = secrets
        .iter()
        .map(String::as_str)
        .filter(|secret| !secret.is_empty())
        .collect();
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));

    secrets
        .into_iter()
        .fold(text.to_string(), |text, secret| text.replace(secret, "***"))
}

/// The root inputs whose `original` ref matches an approved target, paired with that target.
fn matched_inputs<'a, 't>(
    lock: &'a FlakeLock,
    target_map: &HashMap<&str, &'t UpdateTarget>,
) -> Vec<(RootInput<'a>, &'t UpdateTarget)> {
    flake_lock::root_inputs(lock)
        .into_iter()
        .filter_map(|input| {
            let normalized = match &input {
                RootInput::Github(gh) => helpers::format_github_ref(gh.owner, gh.repo, gh.git_ref),
                RootInput::Git(g) => helpers::format_git_ref(g.url, g.git_ref),
                RootInput::Tarball(t) => helpers::format_tarball_ref(t.url),
            };
            let target = *target_map.get(normalized.as_str())?;
            Some((input, target))
        })
        .collect()
}

/// For each root input whose `original` ref matches an approved target, returns
/// `(local_alias, override_flake_ref)` pairs suitable for `nix flake lock --override-input`.
/// `override_flake_ref` pins to the target's exact approved SHA, not the ref's current tip.
fn override_args_for_targets(
    lock: &FlakeLock,
    target_map: &HashMap<&str, &UpdateTarget>,
) -> Vec<(String, String)> {
    let mut overrides: Vec<(String, String)> = matched_inputs(lock, target_map)
        .into_iter()
        .map(|(input, target)| {
            let override_ref = match &input {
                RootInput::Github(gh) => format!(
                    "github:{}/{}/{}",
                    gh.owner, gh.repo, target.target_version.version
                ),
                RootInput::Git(g) => {
                    format!("git+{}?rev={}", g.url, target.target_version.version)
                }
                RootInput::Tarball(_) => {
                    format!("tarball+{}", target.target_version.version)
                }
            };
            (input.alias().to_string(), override_ref)
        })
        .collect();

    overrides.sort();
    overrides
}

/// The `(owner, repo, rev)` of every github input that is overridden, which is where the
/// inputs of the new revision are declared.
fn github_override_sources(
    lock: &FlakeLock,
    target_map: &HashMap<&str, &UpdateTarget>,
) -> Vec<(String, String, String)> {
    matched_inputs(lock, target_map)
        .into_iter()
        .filter_map(|(input, target)| match input {
            RootInput::Github(gh)
                if flake_lock::is_valid_owner(gh.owner) && flake_lock::is_valid_repo(gh.repo) =>
            {
                Some((
                    gh.owner.to_string(),
                    gh.repo.to_string(),
                    target.target_version.version.clone(),
                ))
            }
            _ => None,
        })
        .collect()
}

/// The github owners in the `flake.lock` of each of `sources` at its revision. That lock pins
/// the whole input graph of the dependency, which nix reuses when it locks the override. The
/// lookup is best effort: a dependency without a lock, or one that can't be read, adds nothing.
async fn dependency_lock_owners(
    github: &GitHub,
    sources: &[(String, String, String)],
) -> Vec<String> {
    let lookups: Vec<_> = sources
        .iter()
        .map(|(owner, repo, rev)| dependency_owners(github, owner, repo, rev))
        .collect();

    stream::iter(lookups)
        .buffered(TOKEN_CONCURRENCY)
        .collect::<Vec<Vec<String>>>()
        .await
        .into_iter()
        .flatten()
        .collect()
}

async fn dependency_owners(github: &GitHub, owner: &str, repo: &str, rev: &str) -> Vec<String> {
    let content = match github
        .get_foreign_file(owner, repo, "flake.lock", Some(rev))
        .await
    {
        Ok(Some(content)) => content,
        Ok(None) => return Vec::new(),
        Err(e) => {
            tracing::warn!(
                "Failed to read flake.lock of {}/{}@{}: {:#}",
                owner,
                repo,
                rev,
                e
            );
            return Vec::new();
        }
    };

    match serde_json::from_str::<FlakeLock>(&content) {
        Ok(lock) => flake_lock::github_owners(&lock),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::{PackageInfo, RequirementVersion};

    fn make_target(name: &str, sha: &str) -> UpdateTarget {
        UpdateTarget {
            ecosystem: "nix-flake".to_string(),
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
    fn test_access_tokens_config_has_a_key_per_owner() {
        let tokens = vec![
            ("acme".to_string(), "ghs_one".to_string()),
            ("NixOS".to_string(), "ghs_two".to_string()),
        ];

        assert_eq!(
            access_tokens_config(&tokens).as_deref(),
            Some("extra-access-tokens = github.com/acme=ghs_one github.com/NixOS=ghs_two")
        );
    }

    #[test]
    fn test_group_by_account_merges_spellings_of_one_owner() {
        let owners = vec![
            "Acme".to_string(),
            "acme".to_string(),
            "NixOS".to_string(),
            "acme".to_string(),
        ];

        assert_eq!(
            group_by_account(&owners),
            vec![vec!["Acme", "acme"], vec!["NixOS"]]
        );
    }

    #[test]
    fn test_access_tokens_config_is_absent_without_tokens() {
        assert_eq!(access_tokens_config(&[]), None);
    }

    #[test]
    fn test_redact_hides_every_secret() {
        let secrets = vec!["ghs_one".to_string(), "ghs_two".to_string()];

        assert_eq!(
            redact("fetch ghs_one failed, ghs_two too, ghs_one", &secrets),
            "fetch *** failed, *** too, ***"
        );
    }

    #[test]
    fn test_redact_hides_a_secret_that_has_another_as_its_prefix() {
        let short_first = vec!["abc".to_string(), "abcdef".to_string()];
        let long_first = vec!["abcdef".to_string(), "abc".to_string()];

        assert_eq!(
            redact("token abcdef and abc", &short_first),
            "token *** and ***"
        );
        assert_eq!(
            redact("token abcdef and abc", &long_first),
            "token *** and ***"
        );
    }

    #[test]
    fn test_redact_ignores_empty_secrets() {
        assert_eq!(redact("unchanged", &[String::new()]), "unchanged");
    }

    #[test]
    fn test_secrets_in_nix_config_finds_access_tokens() {
        let config = "max-jobs = 4\n\
                      access-tokens = github.com=ghp_one gitlab.com=PAT:glpat_two\n\
                      extra-access-tokens = github.com/acme=ghs_three\n\
                      substituters = https://cache.nixos.org";

        assert_eq!(
            secrets_in_nix_config(config),
            vec!["ghp_one", "PAT:glpat_two", "glpat_two", "ghs_three"]
        );
    }

    #[test]
    fn test_secrets_in_nix_config_covers_malformed_entries() {
        // No `host=token` pairs, which is a syntax error that nix reports with the line.
        assert_eq!(
            secrets_in_nix_config("access-tokens = github.com ghp_one"),
            vec!["github.com", "ghp_one"]
        );
    }

    #[test]
    fn test_secrets_in_nix_config_is_empty_without_access_tokens() {
        assert!(secrets_in_nix_config("max-jobs = 4").is_empty());
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
    fn test_github_override_sources_are_the_overridden_github_inputs() {
        let content = r#"
        {
          "nodes": {
            "internal": {
              "locked": { "type": "github", "owner": "acme", "repo": "internal", "rev": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
              "original": { "type": "github", "owner": "acme", "repo": "internal", "ref": "main" }
            },
            "untouched": {
              "locked": { "type": "github", "owner": "acme", "repo": "untouched", "rev": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" },
              "original": { "type": "github", "owner": "acme", "repo": "untouched" }
            },
            "core": {
              "locked": { "type": "git", "rev": "cccccccccccccccccccccccccccccccccccccccc" },
              "original": { "type": "git", "url": "https://tangled.org/@tangled.org/core" }
            },
            "root": { "inputs": { "internal": "internal", "untouched": "untouched", "core": "core" } }
          },
          "root": "root"
        }
        "#;
        let lock: FlakeLock = serde_json::from_str(content).unwrap();

        let internal = make_target(
            "github:acme/internal/main",
            "dddddddddddddddddddddddddddddddddddddddd",
        );
        let core = make_target(
            "git+https://tangled.org/@tangled.org/core",
            "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        );
        let target_map: HashMap<&str, &UpdateTarget> = [
            (internal.name.as_str(), &internal),
            (core.name.as_str(), &core),
        ]
        .into();

        assert_eq!(
            github_override_sources(&lock, &target_map),
            vec![(
                "acme".to_string(),
                "internal".to_string(),
                "dddddddddddddddddddddddddddddddddddddddd".to_string()
            )]
        );
    }

    #[test]
    fn test_github_override_sources_skip_invalid_names() {
        let content = r#"
        {
          "nodes": {
            "bad": {
              "locked": { "type": "github", "owner": "acme", "repo": "..", "rev": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
              "original": { "type": "github", "owner": "acme", "repo": ".." }
            },
            "root": { "inputs": { "bad": "bad" } }
          },
          "root": "root"
        }
        "#;
        let lock: FlakeLock = serde_json::from_str(content).unwrap();

        let target = make_target("github:acme/..", "dddddddddddddddddddddddddddddddddddddddd");
        let target_map: HashMap<&str, &UpdateTarget> = [(target.name.as_str(), &target)].into();

        assert!(github_override_sources(&lock, &target_map).is_empty());
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
