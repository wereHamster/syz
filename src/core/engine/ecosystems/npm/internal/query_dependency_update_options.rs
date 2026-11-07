use anyhow::Result;
use nodejs_semver::{Range, Version};

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

    let current = current_version(dependency);
    let mut bumps = Vec::new();

    if let Some(minor_ver) = versions.target_minor {
        let update_type = get_update_type(current.as_ref(), &minor_ver);
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
        let update_type = get_update_type(current.as_ref(), &major_ver);
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

/// The version the project is on: the locked version, or the lowest version the requirement
/// allows if that is newer.
///
/// The requirement alone isn't enough: published packages keep their ranges loose, so the lowest
/// version they allow stays behind the locked one, and may consist of alternatives like `^6 || ^7`.
///
/// Returns `None` for requirements that aren't semver ranges, e.g. `latest` or git URLs.
fn current_version(dependency: &DiscoveredDependency) -> Option<Version> {
    let floor = Range::parse(&dependency.requirement).ok()?.min_version();
    let locked = dependency
        .purl
        .version
        .as_deref()
        .and_then(|version| Version::parse(version).ok());
    floor.into_iter().chain(locked).max()
}

fn get_update_type(current: Option<&Version>, latest_allowed: &str) -> UpdateType {
    let (Some(current), Ok(latest_allowed)) = (current, Version::parse(latest_allowed)) else {
        return UpdateType::Unknown;
    };

    if latest_allowed <= *current {
        UpdateType::UpToDate
    } else if latest_allowed.major() > current.major()
        || (current.major() == 0 && latest_allowed.minor() > current.minor())
        || (current.major() == 0
            && current.minor() == 0
            && latest_allowed.patch() > current.patch())
    {
        UpdateType::Major
    } else if latest_allowed.minor() > current.minor() {
        UpdateType::Minor
    } else {
        UpdateType::Patch
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::PURL;

    fn dependency(requirement: &str, locked: Option<&str>) -> DiscoveredDependency {
        DiscoveredDependency {
            purl: PURL {
                ecosystem: "npm".to_string(),
                namespace: None,
                name: "wonka".to_string(),
                subpath: None,
                version: locked.map(str::to_string),
            },
            requirement: requirement.to_string(),
            minimum_release_age: None,
        }
    }

    fn update_type(requirement: &str, locked: Option<&str>, target: &str) -> UpdateType {
        get_update_type(
            current_version(&dependency(requirement, locked)).as_ref(),
            target,
        )
    }

    #[test]
    fn classifies_widened_ranges_by_the_locked_version() {
        assert_eq!(
            update_type("^6 || ^7", Some("7.0.0"), "7.1.0"),
            UpdateType::Minor
        );
        assert_eq!(
            update_type("^6 || ^7", Some("7.0.0"), "8.0.0"),
            UpdateType::Major
        );
        assert_eq!(
            update_type("^6 || ^7", Some("7.1.0"), "7.1.0"),
            UpdateType::UpToDate
        );
    }

    #[test]
    fn treats_a_kept_range_as_up_to_date_once_the_lockfile_has_the_target() {
        assert_eq!(
            update_type("^5", Some("5.1.0"), "5.1.0"),
            UpdateType::UpToDate
        );
        assert_eq!(update_type("^5", Some("5.0.0"), "5.1.0"), UpdateType::Minor);
    }

    #[test]
    fn compares_against_the_floor_when_it_is_ahead_of_the_lockfile() {
        assert_eq!(
            update_type("^1.4.0", Some("1.2.0"), "1.4.2"),
            UpdateType::Patch
        );
        assert_eq!(update_type("^1.4.0", None, "1.5.0"), UpdateType::Minor);
    }

    #[test]
    fn treats_0x_minor_releases_as_major() {
        assert_eq!(
            update_type("^0.19.1", Some("0.19.1"), "0.20.0"),
            UpdateType::Major
        );
        assert_eq!(
            update_type("^0.0.3", Some("0.0.3"), "0.0.4"),
            UpdateType::Major
        );
    }

    #[test]
    fn skips_requirements_that_are_not_ranges() {
        assert_eq!(
            update_type("latest", Some("1.0.0"), "2.0.0"),
            UpdateType::Unknown
        );
    }
}
