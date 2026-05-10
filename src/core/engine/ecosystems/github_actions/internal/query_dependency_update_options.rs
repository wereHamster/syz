use std::collections::HashMap;

use anyhow::Result;
use serde::Deserialize;

use crate::core::{
    clients::{self, github::GitHub},
    engine::{
        DependencyUpdateOption, DiscoveredDependency, PackageInfo, ProposedBump,
        RequirementVersion, VersionData,
    },
    version_resolver::{self, AvailableRelease},
};

pub async fn run(
    github_client: clients::github::GitHub,
    dependency: &DiscoveredDependency,
) -> Result<DependencyUpdateOption> {
    let full_name = match &dependency.purl.namespace {
        Some(ns) => format!("{}/{}", ns, dependency.purl.name),
        None => dependency.purl.name.clone(),
    };

    let repo_name = extract_repo(&full_name);

    let versions = get_versions_internal(
        &github_client,
        &full_name,
        &dependency.requirement,
        dependency.minimum_release_age.clone(),
    )
    .await?;

    let mut target_minor = None;
    if let Some(minor) = versions.target_minor {
        let clean_base = minor.trim_start_matches('v');
        let v_tag = format!("v{}", clean_base);
        let no_v_tag = clean_base.to_string();

        let mut sha_opt = get_sha_for_tag(&github_client, &repo_name, &v_tag).await;
        if sha_opt.is_none() {
            sha_opt = get_sha_for_tag(&github_client, &repo_name, &no_v_tag).await;
        }

        if let Some(sha) = sha_opt {
            target_minor = Some(RequirementVersion {
                requirement: minor.clone(),
                version: sha,
            });
        } else {
            let prefix = if dependency.requirement.starts_with('v') && !minor.starts_with('v') {
                "v"
            } else {
                ""
            };
            target_minor = Some(RequirementVersion {
                requirement: minor.clone(),
                version: format!("{}{}", prefix, minor),
            });
        }
    }

    let mut target_major = None;
    if let Some(major) = versions.target_major {
        let clean_base = major.trim_start_matches('v');
        let v_tag = format!("v{}", clean_base);
        let no_v_tag = clean_base.to_string();

        let mut sha_opt = get_sha_for_tag(&github_client, &repo_name, &v_tag).await;
        if sha_opt.is_none() {
            sha_opt = get_sha_for_tag(&github_client, &repo_name, &no_v_tag).await;
        }

        if let Some(sha) = sha_opt {
            target_major = Some(RequirementVersion {
                requirement: major.clone(),
                version: sha,
            });
        } else {
            let prefix = if dependency.requirement.starts_with('v') && !major.starts_with('v') {
                "v"
            } else {
                ""
            };
            target_major = Some(RequirementVersion {
                requirement: major.clone(),
                version: format!("{}{}", prefix, major),
            });
        }
    }

    let latest_minor_base = versions.head_minor.unwrap_or_else(|| "0.0.0".to_string());

    let mut latest_minor = latest_minor_base.clone();
    if latest_minor_base != "0.0.0" {
        let clean_latest = latest_minor_base.trim_start_matches('v');
        let v_tag = format!("v{}", clean_latest);
        let no_v_tag = clean_latest.to_string();

        let mut sha_opt = get_sha_for_tag(&github_client, &repo_name, &v_tag).await;
        if sha_opt.is_none() {
            sha_opt = get_sha_for_tag(&github_client, &repo_name, &no_v_tag).await;
        }

        if let Some(sha) = sha_opt {
            latest_minor = sha;
        }
    }

    let latest_major_base = versions.head_major.unwrap_or_else(|| "0.0.0".to_string());

    let mut latest_major = latest_major_base.clone();
    if latest_major_base != "0.0.0" {
        let clean_latest = latest_major_base.trim_start_matches('v');
        let v_tag = format!("v{}", clean_latest);
        let no_v_tag = clean_latest.to_string();

        let mut sha_opt = get_sha_for_tag(&github_client, &repo_name, &v_tag).await;
        if sha_opt.is_none() {
            sha_opt = get_sha_for_tag(&github_client, &repo_name, &no_v_tag).await;
        }

        if let Some(sha) = sha_opt {
            latest_major = sha;
        }
    }

    // tracing::info!(
    //     "Dependency Update Option {} minor:{} major:{}",
    //     dependency.name,
    //     latest_minor,
    //     latest_major
    // );

    // if let Some(ref t) = target_minor {
    //     tracing::info!("target minor req:{} version:{}", t.requirement, t.version);
    // }
    // if let Some(ref t) = target_major {
    //     tracing::info!("target major req:{} version:{}", t.requirement, t.version);
    // }

    let mut bumps = Vec::new();

    if let Some(minor) = target_minor {
        bumps.push(ProposedBump {
            target_version: minor.version,
            head_version: latest_minor,
            is_major: false,
            update_type: crate::core::engine::UpdateType::Minor,
        });
    }

    if let Some(major) = target_major {
        bumps.push(ProposedBump {
            target_version: major.version,
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

fn extract_repo(action: &str) -> String {
    let parts: Vec<&str> = action.splitn(3, '/').collect();
    if parts.len() >= 2 {
        format!("{}/{}", parts[0], parts[1])
    } else {
        action.to_string()
    }
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct GithubRelease {
    tag_name: String,
    published_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Deserialize)]
struct GithubTag {
    object: GithubTagObject,
}

#[derive(Deserialize)]
struct GithubTagObject {
    sha: String,
    #[serde(rename = "type")]
    object_type: String,
}

async fn get_sha_for_tag(github_client: &GitHub, action_repo: &str, tag: &str) -> Option<String> {
    let route = format!("/repos/{}/git/ref/tags/{}", action_repo, tag);
    if let Ok(res) = github_client.get_json(&route).await {
        if let Ok(tag_data) = serde_json::from_value::<GithubTag>(res) {
            let sha = if tag_data.object.object_type == "tag" {
                let tag_route = format!("/repos/{}/git/tags/{}", action_repo, tag_data.object.sha);
                if let Ok(tag_res) = github_client.get_json(&tag_route).await {
                    if let Some(obj) = tag_res.get("object").and_then(|o| o.as_object()) {
                        obj.get("sha")
                            .and_then(|s| s.as_str())
                            .unwrap_or(&tag_data.object.sha)
                            .to_string()
                    } else {
                        tag_data.object.sha
                    }
                } else {
                    tag_data.object.sha
                }
            } else {
                tag_data.object.sha
            };
            return Some(sha);
        }
    }
    None
}

async fn get_versions_internal(
    github_client: &GitHub,
    name: &str,
    req: &str,
    minimum_release_age: Option<chrono::Duration>,
) -> Result<VersionData> {
    let repo_name = extract_repo(name);
    let route = format!("/repos/{}/releases", repo_name);
    let response = match github_client.get_json(&route).await {
        Ok(res) => res,
        Err(_) => {
            return Ok(VersionData {
                repo_url: None,
                head_major: None,
                head_minor: None,
                target_major: None,
                target_minor: None,
            })
        }
    };

    let releases: Vec<GithubRelease> = serde_json::from_value(response)?;

    let mut heads_per_major: HashMap<u64, (semver::Version, String)> = HashMap::new();

    for rel in &releases {
        // tracing::info!("release {} {}", name, rel.tag_name);
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
    // tracing::info!("clean_req {}", clean_req);
    let current_ver = if let Some(v) = coerce_version(clean_req) {
        v
    } else {
        return Ok(VersionData {
            repo_url: Some(format!("https://github.com/{}", repo_name)),
            head_major: None,
            head_minor: None,
            target_major: None,
            target_minor: None,
        });
    };

    let _target_minor: Option<(semver::Version, String)> = None;
    let _target_major: Option<(semver::Version, String)> = None;

    let _min_age = minimum_release_age.unwrap_or(chrono::Duration::zero());
    let now = chrono::Utc::now();

    let mut available_releases = Vec::new();

    for rel in &releases {
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
                    coerce_version(ver_str) == Some(ver.clone())
                })
                .map(|r| r.tag_name.clone())
        })
    };

    let target_minor = get_tag_for_ver(resolution.minor.target);
    let target_major = get_tag_for_ver(resolution.major.target);
    let head_minor = get_tag_for_ver(resolution.minor.head);
    let head_major = get_tag_for_ver(resolution.major.head);

    Ok(VersionData {
        repo_url: Some(format!("https://github.com/{}", repo_name)),
        target_minor,
        target_major,
        head_minor,
        head_major,
    })
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
