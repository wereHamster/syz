use anyhow::Result;

use crate::core::{
    clients,
    engine::{DependencyUpdateOption, DiscoveredDependency, PackageInfo, ProposedBump},
};

pub async fn run(
    crates_client: clients::crates::Crates,
    dependency: &DiscoveredDependency,
) -> Result<DependencyUpdateOption> {
    let req_to_pass = dependency
        .purl
        .version
        .clone()
        .unwrap_or_else(|| dependency.requirement.clone());

    let versions = crates_client
        .get_versions(&dependency.purl.name, &req_to_pass)
        .await?;

    let mut bumps = Vec::new();

    if let Some(minor_ver) = versions.target_minor {
        bumps.push(ProposedBump {
            target_version: minor_ver,
            head_version: versions.head_minor.unwrap_or_else(|| "0.0.0".to_string()),
            is_major: false,
            update_type: crate::core::engine::UpdateType::Minor,
        });
    }

    if let Some(major_ver) = versions.target_major {
        bumps.push(ProposedBump {
            target_version: major_ver,
            head_version: versions.head_major.unwrap_or_else(|| "0.0.0".to_string()),
            is_major: true,
            update_type: crate::core::engine::UpdateType::Major,
        });
    }

    Ok(DependencyUpdateOption {
        package_info: PackageInfo {
            repo_url: versions.repo_url,
        },
        bumps,
    })
}
