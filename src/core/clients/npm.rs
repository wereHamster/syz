use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use semver::Version;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize, Clone)]
pub struct RegistryResponse {
    #[allow(dead_code)]
    pub name: String,
    #[serde(rename = "dist-tags")]
    pub dist_tags: HashMap<String, String>,
    pub time: HashMap<String, String>,
    pub repository: Option<serde_json::Value>,
}

pub struct VersionData {
    pub target_minor: Option<String>,
    pub target_major: Option<String>,
    pub head_minor: Option<String>,
    pub head_major: Option<String>,
    pub repo_url: Option<String>,
}

use crate::core::{
    http_agent::HttpAgent,
    version_resolver::{resolve_updates, AvailableRelease},
};

#[derive(Clone)]
pub struct Npm {
    agent: HttpAgent,
}

impl Npm {
    pub fn new(agent: HttpAgent) -> Self {
        Self { agent }
    }

    async fn get_registry_response(&self, package: &str) -> Result<RegistryResponse> {
        let url = format!("https://registry.npmjs.org/{}", package);
        let response: RegistryResponse = self.agent.json(&url).await?;
        Ok(response)
    }

    pub async fn get_versions(
        &self,
        package: &str,
        current_version: Option<&str>,
        minimum_release_age: Option<chrono::Duration>,
    ) -> Result<VersionData> {
        let response = self.get_registry_response(package).await?;

        let repo_url = response.repository.and_then(|repo| {
            let raw_url = if let Some(url) = repo.as_str() {
                Some(url.to_string())
            } else if let Some(obj) = repo.as_object() {
                obj.get("url")
                    .and_then(|u| u.as_str())
                    .map(|s| s.to_string())
            } else {
                None
            };
            raw_url.map(|u| clean_repo_url(&u))
        });

        let current_version = match current_version {
            Some(v) => v,
            None => {
                return Ok(VersionData {
                    target_minor: None,
                    target_major: None,
                    head_minor: None,
                    head_major: None,
                    repo_url,
                })
            }
        };

        let latest_absolute = response
            .dist_tags
            .get("latest")
            .cloned()
            .context("No 'latest' tag found in registry response")?;

        let latest_absolute_ver =
            Version::parse(&latest_absolute).context("Failed to parse latest version as semver")?;

        let current_ver =
            Version::parse(current_version).context("Failed to parse current version")?;

        let mut available_releases = Vec::new();

        for (version_str, time_str) in &response.time {
            // Skip metadata keys in the time object
            if version_str == "modified" || version_str == "created" || version_str == "unpublished"
            {
                continue;
            }

            let parsed_time = match DateTime::parse_from_rfc3339(time_str) {
                Ok(t) => t.with_timezone(&Utc),
                Err(_) => continue, // Skip if time is unparseable
            };

            if let Ok(ver) = Version::parse(version_str) {
                if package == "@types/node" && ver.major % 2 != 0 && ver.major != current_ver.major
                {
                    continue;
                }

                // Ensure we only look at versions that are <= the latest absolute version
                // to prevent picking up newer versions from other tags (e.g. beta, next)
                if ver <= latest_absolute_ver && ver.pre.is_empty() {
                    available_releases.push(AvailableRelease {
                        version: ver,
                        published_at: parsed_time,
                    });
                }
            }
        }

        let resolution = resolve_updates(&current_ver, &available_releases, minimum_release_age);

        Ok(VersionData {
            target_minor: resolution.minor.target.map(|v| v.to_string()),
            target_major: resolution.major.target.map(|v| v.to_string()),
            head_minor: resolution.minor.head.map(|v| v.to_string()),
            head_major: resolution.major.head.map(|v| v.to_string()),
            repo_url,
        })
    }
}

fn clean_repo_url(url: &str) -> String {
    let mut clean_url = url.to_string();
    if clean_url.starts_with("git+") {
        clean_url = clean_url[4..].to_string();
    }
    if clean_url.starts_with("git://") {
        clean_url = clean_url.replace("git://", "https://");
    }
    if clean_url.starts_with("ssh://git@") {
        clean_url = clean_url.replace("ssh://git@", "https://");
    }
    if clean_url.starts_with("git@github.com:") {
        clean_url = clean_url.replace("git@github.com:", "https://github.com/");
    }
    if clean_url.ends_with(".git") {
        clean_url = clean_url[..clean_url.len() - 4].to_string();
    }

    // Extract base github.com owner/repo and discard paths/fragments/query
    if clean_url.contains("github.com/") {
        let parts: Vec<&str> = clean_url.split("github.com/").collect();
        if parts.len() == 2 {
            let mut sub_parts = parts[1].split(|c| c == '/' || c == '#' || c == '?');
            let owner = sub_parts.next().unwrap_or("");
            let repo = sub_parts.next().unwrap_or("");
            if !owner.is_empty() && !repo.is_empty() {
                let mut base = format!("{}github.com/{}/{}", parts[0], owner, repo);
                if base.ends_with(".git") {
                    base = base[..base.len() - 4].to_string();
                }
                clean_url = base;
            }
        }
    }

    clean_url
}
