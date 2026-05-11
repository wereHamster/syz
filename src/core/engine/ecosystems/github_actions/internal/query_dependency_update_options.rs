use std::collections::HashMap;

use anyhow::Result;
use serde::Deserialize;

use crate::core::{
    clients::{self, github::GitHub},
    engine::{
        DependencyUpdateOption, DiscoveredDependency, PackageInfo, ProposedBump, VersionData,
    },
    version_resolver::{self, AvailableRelease},
};

use super::helpers;

#[derive(Deserialize)]
#[allow(dead_code)]
struct GithubRelease {
    tag_name: String,
    published_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub async fn run(
    github_client: clients::github::GitHub,
    dependency: &DiscoveredDependency,
) -> Result<DependencyUpdateOption> {
    let full_name = match &dependency.purl.namespace {
        Some(ns) => format!("{}/{}", ns, dependency.purl.name),
        None => dependency.purl.name.clone(),
    };

    let repo_name = helpers::extract_repo(&full_name);

    let versions = match fetch_releases(&github_client, &repo_name).await {
        Some(releases) => compute_versions(
            &repo_name,
            &releases,
            &dependency.requirement,
            dependency.minimum_release_age,
            chrono::Utc::now(),
        ),
        None => VersionData {
            repo_url: None,
            head_major: None,
            head_minor: None,
            target_major: None,
            target_minor: None,
        },
    };

    let latest_minor = versions.head_minor.unwrap_or_else(|| "0.0.0".to_string());
    let latest_major = versions.head_major.unwrap_or_else(|| "0.0.0".to_string());

    let mut bumps = Vec::new();

    if let Some(minor) = versions.target_minor {
        bumps.push(ProposedBump {
            target_version: minor,
            head_version: latest_minor,
            is_major: false,
            update_type: crate::core::engine::UpdateType::Minor,
        });
    }

    if let Some(major) = versions.target_major {
        bumps.push(ProposedBump {
            target_version: major,
            head_version: latest_major,
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

async fn fetch_releases(github_client: &GitHub, repo_name: &str) -> Option<Vec<GithubRelease>> {
    let route = format!("/repos/{}/releases", repo_name);
    let response = match github_client.get_json(&route).await {
        Ok(res) => res,
        Err(_) => return None,
    };

    serde_json::from_value(response).ok()
}

fn compute_versions(
    repo_name: &str,
    releases: &[GithubRelease],
    req: &str,
    minimum_release_age: Option<chrono::Duration>,
    now: chrono::DateTime<chrono::Utc>,
) -> VersionData {
    let mut heads_per_major: HashMap<u64, (semver::Version, String)> = HashMap::new();

    for rel in releases {
        let ver_str = rel.tag_name.trim_start_matches('v');
        if let Some(ver) = coerce_version(ver_str) {
            if ver.pre.is_empty() {
                let entry = heads_per_major
                    .entry(ver.major)
                    .or_insert_with(|| (ver.clone(), rel.tag_name.clone()));
                if ver > entry.0 {
                    *entry = (ver, rel.tag_name.clone());
                }
            }
        }
    }

    let clean_req = req.trim_start_matches('v');
    let current_ver = if let Some(v) = coerce_version(clean_req) {
        v
    } else {
        return VersionData {
            repo_url: Some(format!("https://github.com/{}", repo_name)),
            head_major: None,
            head_minor: None,
            target_major: None,
            target_minor: None,
        };
    };

    let _target_minor: Option<(semver::Version, String)> = None;
    let _target_major: Option<(semver::Version, String)> = None;

    let _min_age = minimum_release_age.unwrap_or(chrono::Duration::zero());

    let mut available_releases = Vec::new();

    for rel in releases {
        let parsed_time = rel.published_at.unwrap_or(now);

        let ver_str = rel.tag_name.trim_start_matches('v');
        if let Some(ver) = coerce_version(ver_str) {
            if ver.pre.is_empty() {
                available_releases.push(AvailableRelease {
                    version: ver,
                    published_at: parsed_time,
                });
            }
        }
    }

    let resolution =
        version_resolver::resolve_updates(&current_ver, &available_releases, minimum_release_age);

    let get_tag_for_ver = |opt_ver: Option<semver::Version>| -> Option<String> {
        opt_ver.and_then(|ver| {
            releases
                .iter()
                .find(|r| {
                    let ver_str = r.tag_name.trim_start_matches('v');
                    coerce_version(ver_str).as_ref() == Some(&ver)
                })
                .map(|r| r.tag_name.clone())
        })
    };

    let target_minor = get_tag_for_ver(resolution.minor.target);
    let target_major = get_tag_for_ver(resolution.major.target);
    let head_minor = get_tag_for_ver(resolution.minor.head);
    let head_major = get_tag_for_ver(resolution.major.head);

    VersionData {
        repo_url: Some(format!("https://github.com/{}", repo_name)),
        target_minor,
        target_major,
        head_minor,
        head_major,
    }
}

fn coerce_version(v: &str) -> Option<semver::Version> {
    if let Ok(ver) = semver::Version::parse(v) {
        return Some(ver);
    }

    let (base, rest) = if let Some(idx) = v.find(|c| c == '-' || c == '+') {
        (&v[..idx], &v[idx..])
    } else {
        (v, "")
    };

    let parts: Vec<&str> = base.split('.').collect();
    if parts.len() == 1 {
        let new_v = format!("{}.0.0{}", base, rest);
        semver::Version::parse(&new_v).ok()
    } else if parts.len() == 2 {
        let new_v = format!("{}.0{}", base, rest);
        semver::Version::parse(&new_v).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn test_compute_versions() {
        let now = Utc.with_ymd_and_hms(2023, 1, 1, 0, 0, 0).unwrap();
        let releases = vec![
            GithubRelease {
                tag_name: "v1.0.0".to_string(),
                published_at: Some(Utc.with_ymd_and_hms(2022, 1, 1, 0, 0, 0).unwrap()),
            },
            GithubRelease {
                tag_name: "v1.1.0".to_string(),
                published_at: Some(Utc.with_ymd_and_hms(2022, 6, 1, 0, 0, 0).unwrap()),
            },
            GithubRelease {
                tag_name: "v2.0.0".to_string(),
                published_at: Some(Utc.with_ymd_and_hms(2022, 12, 1, 0, 0, 0).unwrap()),
            },
        ];

        let data = compute_versions("actions/checkout", &releases, "v1.0.0", None, now);

        assert_eq!(
            data.repo_url.unwrap(),
            "https://github.com/actions/checkout"
        );
        assert_eq!(data.head_minor.unwrap(), "v1.1.0");
        assert_eq!(data.target_minor.unwrap(), "v1.1.0");
        assert_eq!(data.head_major.unwrap(), "v2.0.0");
        assert_eq!(data.target_major.unwrap(), "v2.0.0");
    }

    #[test]
    fn test_coerce_version() {
        assert_eq!(coerce_version("1").unwrap(), semver::Version::new(1, 0, 0));
        assert_eq!(
            coerce_version("1.2").unwrap(),
            semver::Version::new(1, 2, 0)
        );
        assert_eq!(
            coerce_version("1.2.3").unwrap(),
            semver::Version::new(1, 2, 3)
        );
    }
}
