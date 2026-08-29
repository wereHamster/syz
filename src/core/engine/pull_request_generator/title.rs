use crate::core::engine::repository::ProjectRepositorySnapshot;
use crate::core::engine::UpdateTarget;

pub async fn generate_title(
    package_group: &str,
    ecosystem: &str,
    targets: &[UpdateTarget],
    is_major: bool,
    github: Option<&crate::core::clients::github::GitHub>,
    project_repo: &dyn ProjectRepositorySnapshot,
) -> String {
    let mut title = if targets.len() == 1 && ecosystem == "nix-flake" {
        let target = &targets[0];
        let sha = &target.target_version.version;
        let short_sha = &sha[..sha.len().min(7)];

        let display_name = crate::core::engine::ecosystems::nix_flake::internal::find_input_alias::run(
            project_repo,
            &target.name,
        )
        .await
        .unwrap_or_else(|| package_group.to_string());

        let date_suffix = match github {
            Some(gh) => match crate::core::engine::ecosystems::nix_flake::internal::helpers::parse_github_repo(&target.name) {
                Some(repo) => crate::core::engine::ecosystems::nix_flake::internal::helpers::get_commit_date(
                    gh, &repo.owner, &repo.repo, sha,
                )
                .await
                .map(|d| format!(" ({})", d.format("%Y-%m-%d")))
                .unwrap_or_default(),
                None => String::new(),
            },
            None => String::new(),
        };

        format!(
            "Update Nix Flake input `{}` to {}{}",
            display_name, short_sha, date_suffix
        )
    } else if targets.len() == 1 {
        let target = &targets[0];
        let mut clean_new = target.target_version.version.clone();

        if ecosystem == "github-actions" && clean_new.len() == 40 {
            if let Some(gh) = github {
                let action_repo = crate::core::engine::ecosystems::github_actions::internal::helpers::extract_repo(&target.name);
                if let Some(tag) = crate::core::engine::ecosystems::github_actions::internal::helpers::get_tag_for_sha(gh, &action_repo, &clean_new).await {
                    clean_new = tag;
                }
            }
        } else if ecosystem == "github-actions" {
            clean_new = target.target_version.requirement.clone();
        }

        format!("Update {} to {}", package_group, clean_new)
    } else {
        let first_new_req = &targets[0].target_version;
        let mut highest_new = &first_new_req.version;
        let mut lowest_new = &first_new_req.version;

        for t in targets {
            let current_ver_str = &t.target_version.version;
            let highest_ver_str = highest_new;
            let lowest_ver_str = lowest_new;

            if let (Ok(ver_new), Ok(ver_highest)) = (
                semver::Version::parse(current_ver_str),
                semver::Version::parse(highest_ver_str),
            ) {
                if ver_new > ver_highest {
                    highest_new = &t.target_version.version;
                }
            }

            if let (Ok(ver_new), Ok(ver_lowest)) = (
                semver::Version::parse(current_ver_str),
                semver::Version::parse(lowest_ver_str),
            ) {
                if ver_new < ver_lowest {
                    lowest_new = &t.target_version.version;
                }
            }
        }

        let mut clean_highest = highest_new.clone();
        let mut clean_lowest = lowest_new.clone();

        if ecosystem == "github-actions" {
            // For GitHub Actions, we actually want the requirement text if it isn't a SHA
            let highest_req = &targets
                .iter()
                .find(|t| t.target_version.version == *highest_new)
                .unwrap()
                .target_version
                .requirement;
            let lowest_req = &targets
                .iter()
                .find(|t| t.target_version.version == *lowest_new)
                .unwrap()
                .target_version
                .requirement;

            clean_highest = highest_req.clone();
            clean_lowest = lowest_req.clone();

            if let Some(gh) = github {
                let action_repo = crate::core::engine::ecosystems::github_actions::internal::helpers::extract_repo(package_group);
                if clean_highest.len() == 40 {
                    if let Some(tag) = crate::core::engine::ecosystems::github_actions::internal::helpers::get_tag_for_sha(gh, &action_repo, &clean_highest).await {
                        clean_highest = tag;
                    }
                }
                if clean_lowest.len() == 40 {
                    if let Some(tag) = crate::core::engine::ecosystems::github_actions::internal::helpers::get_tag_for_sha(gh, &action_repo, &clean_lowest).await {
                        clean_lowest = tag;
                    }
                }
            }
        }

        if clean_highest == clean_lowest {
            format!("Update {} to {}", package_group, clean_highest)
        } else {
            format!(
                "Update {} to {} ~ {}",
                package_group, clean_lowest, clean_highest
            )
        }
    };

    if is_major {
        title.push_str(" (major)");
    }

    title
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::{PackageInfo, RequirementVersion};

    struct MockSnapshot {
        files: std::collections::HashMap<String, String>,
    }

    impl MockSnapshot {
        fn empty() -> Self {
            Self {
                files: std::collections::HashMap::new(),
            }
        }

        fn with_file(path: &str, content: &str) -> Self {
            let mut files = std::collections::HashMap::new();
            files.insert(path.to_string(), content.to_string());
            Self { files }
        }
    }

    #[async_trait::async_trait]
    impl ProjectRepositorySnapshot for MockSnapshot {
        async fn list_files(&self) -> anyhow::Result<Vec<String>> {
            Ok(self.files.keys().cloned().collect())
        }

        async fn read_file(&self, path: &str) -> anyhow::Result<String> {
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("not found"))
        }
    }

    fn make_target(name: &str, current: &str, target: &str) -> UpdateTarget {
        UpdateTarget {
            name: name.to_string(),
            current_version: RequirementVersion {
                requirement: current.to_string(),
                version: current.to_string(),
            },
            target_version: RequirementVersion {
                requirement: target.to_string(),
                version: target.to_string(),
            },
            latest_version: target.to_string(),
            package_info: PackageInfo { repo_url: None },
            minimum_release_age: None,
        }
    }

    #[tokio::test]
    async fn test_generate_title_single_target() {
        let targets = vec![make_target("react", "17.0.0", "18.0.0")];
        let repo = MockSnapshot::empty();
        let title = generate_title("React", "npm", &targets, false, None, &repo).await;
        assert_eq!(title, "Update React to 18.0.0");

        let title_major = generate_title("React", "npm", &targets, true, None, &repo).await;
        assert_eq!(title_major, "Update React to 18.0.0 (major)");
    }

    #[tokio::test]
    async fn test_generate_title_nix_flake_single_target_resolves_alias() {
        let targets = vec![make_target(
            "github:NixOS/nixpkgs/nixpkgs-unstable",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "104240a772428cc2e20d8fd86c9ddbb886bbaff2",
        )];
        let flake_lock = r#"
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
        let repo = MockSnapshot::with_file("flake.lock", flake_lock);

        let title = generate_title(
            "github:NixOS/nixpkgs/nixpkgs-unstable",
            "nix-flake",
            &targets,
            false,
            None,
            &repo,
        )
        .await;
        assert_eq!(title, "Update Nix Flake input `nixpkgs` to 104240a");

        let title_major = generate_title(
            "github:NixOS/nixpkgs/nixpkgs-unstable",
            "nix-flake",
            &targets,
            true,
            None,
            &repo,
        )
        .await;
        assert_eq!(
            title_major,
            "Update Nix Flake input `nixpkgs` to 104240a (major)"
        );
    }

    #[tokio::test]
    async fn test_generate_title_nix_flake_falls_back_to_package_group_without_flake_lock() {
        let targets = vec![make_target(
            "github:NixOS/nixpkgs/nixpkgs-unstable",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "104240a772428cc2e20d8fd86c9ddbb886bbaff2",
        )];
        let repo = MockSnapshot::empty();

        let title = generate_title(
            "github:NixOS/nixpkgs/nixpkgs-unstable",
            "nix-flake",
            &targets,
            false,
            None,
            &repo,
        )
        .await;
        assert_eq!(
            title,
            "Update Nix Flake input `github:NixOS/nixpkgs/nixpkgs-unstable` to 104240a"
        );
    }

    #[tokio::test]
    async fn test_generate_title_multiple_targets_same_version() {
        let targets = vec![
            make_target("react", "17.0.0", "18.0.0"),
            make_target("react-dom", "17.0.0", "18.0.0"),
        ];
        let repo = MockSnapshot::empty();
        let title = generate_title("React", "npm", &targets, false, None, &repo).await;
        assert_eq!(title, "Update React to 18.0.0");
    }

    #[tokio::test]
    async fn test_generate_title_multiple_targets_different_versions() {
        let targets = vec![
            make_target("babel-core", "7.0.0", "7.1.0"),
            make_target("babel-cli", "7.0.0", "7.2.5"),
            make_target("babel-preset-env", "7.0.0", "7.0.5"),
        ];
        let repo = MockSnapshot::empty();
        let title = generate_title("Babel", "npm", &targets, false, None, &repo).await;
        assert_eq!(title, "Update Babel to 7.0.5 ~ 7.2.5");
    }
}
