use anyhow::Result;
use semver::Version;

use crate::core::{
    clients,
    engine::{DependencyUpdateOption, DiscoveredDependency, PackageInfo, ProposedBump, UpdateType},
};

pub async fn run(
    npm_client: clients::npm::Npm,
    dependency: &DiscoveredDependency,
) -> Result<DependencyUpdateOption> {
    let full_name = match &dependency.purl.namespace {
        Some(ns) => format!("{}/{}", ns, dependency.purl.name),
        None => dependency.purl.name.clone(),
    };

    let versions = npm_client
        .get_versions(
            &full_name,
            dependency.purl.version.as_deref(),
            dependency.minimum_release_age,
        )
        .await?;

    let mut bumps = Vec::new();

    if let Some(minor_ver) = versions.target_minor {
        let update_type = get_update_type(&dependency.requirement, &minor_ver);
        if update_type != UpdateType::UpToDate && update_type != UpdateType::Unknown {
            bumps.push(ProposedBump {
                target_version: minor_ver,
                head_version: versions.head_minor.unwrap_or_else(|| "0.0.0".to_string()),
                is_major: false,
                update_type,
            });
        }
    }

    if let Some(major_ver) = versions.target_major {
        let update_type = get_update_type(&dependency.requirement, &major_ver);
        if update_type != UpdateType::UpToDate && update_type != UpdateType::Unknown {
            // Note: npm special cases 0.x.x versions
            // Even if it's considered a "major" bump in terms of NPM's config,
            // we should mark it properly. The get_update_type function handles the 0.x rules.
            let is_major = update_type == UpdateType::Major;
            bumps.push(ProposedBump {
                target_version: major_ver,
                head_version: versions.head_major.unwrap_or_else(|| "0.0.0".to_string()),
                is_major,
                update_type,
            });
        }
    }

    Ok(DependencyUpdateOption {
        package_info: PackageInfo {
            repo_url: versions.repo_url,
        },
        bumps,
    })
}

fn pad_version(v: &str) -> String {
    let mut parts: Vec<&str> = v.split('.').collect();
    while parts.len() < 3 {
        parts.push("0");
    }
    parts.join(".")
}

fn get_update_type(current_req: &str, latest_allowed: &str) -> UpdateType {
    let latest_allowed_str =
        pad_version(latest_allowed.trim_start_matches(&['^', '~', '=', 'v'][..]));
    let latest_allowed_ver = match Version::parse(&latest_allowed_str) {
        Ok(v) => v,
        Err(_) => return UpdateType::Unknown,
    };

    let current_ver_str = pad_version(current_req.trim_start_matches(&['^', '~', '=', 'v'][..]));
    let current_ver = match Version::parse(&current_ver_str) {
        Ok(v) => v,
        Err(_) => return UpdateType::Unknown,
    };

    if latest_allowed_ver > current_ver {
        if (latest_allowed_ver.major > current_ver.major)
            || (current_ver.major == 0 && latest_allowed_ver.minor > current_ver.minor)
            || (current_ver.major == 0
                && current_ver.minor == 0
                && latest_allowed_ver.patch > current_ver.patch)
        {
            UpdateType::Major
        } else if latest_allowed_ver.minor > current_ver.minor {
            UpdateType::Minor
        } else {
            UpdateType::Patch
        }
    } else {
        UpdateType::UpToDate
    }
}
