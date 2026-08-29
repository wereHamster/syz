use anyhow::Result;

use crate::core::{
    clients,
    engine::{DependencyUpdateOption, DiscoveredDependency, PackageInfo, ProposedBump, UpdateType},
};

use super::helpers;

pub async fn run(
    github_client: clients::github::GitHub,
    dependency: &DiscoveredDependency,
) -> Result<DependencyUpdateOption> {
    let reference = if dependency.requirement.is_empty() {
        None
    } else {
        Some(dependency.requirement.as_str())
    };

    if let Some(repo) = helpers::parse_git_url(&dependency.purl.name) {
        let latest_sha = helpers::get_latest_git_commit_sha(&repo.url, reference).await;

        let bumps = match latest_sha {
            Some(sha) if Some(sha.as_str()) != dependency.purl.version.as_deref() => {
                vec![ProposedBump {
                    target_version: sha.clone(),
                    head_version: sha,
                    is_major: false,
                    update_type: UpdateType::Minor,
                }]
            }
            _ => Vec::new(),
        };

        return Ok(DependencyUpdateOption {
            package_info: PackageInfo {
                repo_url: Some(repo.url),
            },
            bumps,
        });
    }

    let repo = match helpers::parse_github_repo(&dependency.purl.name) {
        Some(r) => r,
        None => {
            return Ok(DependencyUpdateOption {
                package_info: PackageInfo { repo_url: None },
                bumps: Vec::new(),
            })
        }
    };

    let repo_url = Some(format!("https://github.com/{}/{}", repo.owner, repo.repo));

    let latest_sha =
        helpers::get_latest_commit_sha(&github_client, &repo.owner, &repo.repo, reference).await;

    let bumps = match latest_sha {
        Some(sha) if Some(sha.as_str()) != dependency.purl.version.as_deref() => {
            vec![ProposedBump {
                target_version: sha.clone(),
                head_version: sha,
                is_major: false,
                update_type: UpdateType::Minor,
            }]
        }
        _ => Vec::new(),
    };

    Ok(DependencyUpdateOption {
        package_info: PackageInfo { repo_url },
        bumps,
    })
}
