use crate::core::engine::VersionData;
use crate::core::http_agent::HttpAgent;
use anyhow::{Context, Result};
use semver::{Version, VersionReq};
use serde::Deserialize;

#[derive(Deserialize)]
struct CrateIndexLine {
    vers: String,
    yanked: bool,
    pubtime: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Clone)]
pub struct Crates {
    http_agent: HttpAgent,
}

#[derive(serde::Deserialize)]
struct CratesIoResponse {
    #[serde(rename = "crate")]
    crate_data: CrateData,
    versions: Vec<CrateVersion>,
}

#[derive(serde::Deserialize)]
struct CrateData {
    repository: Option<String>,
}

#[derive(serde::Deserialize)]
struct CrateVersion {
    num: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

impl Crates {
    pub fn new(http_agent: HttpAgent) -> Self {
        Self { http_agent }
    }

    fn get_crates_io_url(name: &str) -> String {
        let lower_name = name.to_lowercase();
        match lower_name.len() {
            0 => "https://index.crates.io/invalid".to_string(),
            1 => format!("https://index.crates.io/1/{}", lower_name),
            2 => format!("https://index.crates.io/2/{}", lower_name),
            3 => format!(
                "https://index.crates.io/3/{}/{}",
                &lower_name[0..1],
                lower_name
            ),
            _ => format!(
                "https://index.crates.io/{}/{}/{}",
                &lower_name[0..2],
                &lower_name[2..4],
                lower_name
            ),
        }
    }

    async fn get_index_versions(&self, package: &str) -> Result<Vec<IndexVersion>> {
        let url = Self::get_crates_io_url(package);
        let body = self.http_agent.text(&url).await?;
        let versions = parse_index(&body);
        if versions.is_empty() {
            anyhow::bail!("No non-yanked versions found for {}", package);
        }
        Ok(versions)
    }

    pub async fn get_release_history(
        &self,
        name: &str,
        clean_current: &str,
        clean_new: &str,
    ) -> Result<Vec<crate::core::engine::Release>> {
        let url = format!("https://crates.io/api/v1/crates/{}", name);
        let data: CratesIoResponse = self.http_agent.json(&url).await?;

        let mut history = Vec::new();
        let target_ver =
            semver::Version::parse(clean_new).unwrap_or_else(|_| semver::Version::new(0, 0, 0));
        let current_ver =
            semver::Version::parse(clean_current).unwrap_or_else(|_| semver::Version::new(0, 0, 0));

        for v in data.versions {
            if let Ok(parsed) = semver::Version::parse(&v.num) {
                if parsed >= current_ver && parsed <= target_ver {
                    history.push(crate::core::engine::Release {
                        version: v.num,
                        publish_time: v.created_at,
                    });
                }
            }
        }

        history.sort_by_key(|r| std::cmp::Reverse(r.publish_time));

        Ok(history)
    }

    pub async fn get_package_info(&self, name: &str) -> Result<crate::core::engine::PackageInfo> {
        let url = format!("https://crates.io/api/v1/crates/{}", name);
        let data: CratesIoResponse = self.http_agent.json(&url).await?;

        Ok(crate::core::engine::PackageInfo {
            repo_url: data.crate_data.repository,
        })
    }

    /// Picks the update targets for `req`. Versions published less than `minimum_release_age`
    /// ago are not proposed as targets but still count as the head.
    pub async fn get_versions(
        &self,
        name: &str,
        req: &str,
        minimum_release_age: Option<chrono::Duration>,
    ) -> Result<VersionData> {
        let versions = self.get_index_versions(name).await?;
        select_versions(&versions, req, minimum_release_age, chrono::Utc::now())
    }
}

struct IndexVersion {
    version: Version,
    /// Absent for versions published before crates.io recorded publish times.
    pubtime: Option<chrono::DateTime<chrono::Utc>>,
}

/// The non-yanked stable versions in a sparse index file, in ascending order.
fn parse_index(body: &str) -> Vec<IndexVersion> {
    let mut versions: Vec<IndexVersion> = body
        .lines()
        .filter_map(|line| serde_json::from_str::<CrateIndexLine>(line).ok())
        .filter(|entry| !entry.yanked)
        .filter_map(|entry| {
            let version = Version::parse(&entry.vers).ok()?;
            version.pre.is_empty().then_some(IndexVersion {
                version,
                pubtime: entry.pubtime,
            })
        })
        .collect();
    versions.sort_by(|a, b| a.version.cmp(&b.version));
    versions
}

fn select_versions(
    versions: &[IndexVersion],
    req: &str,
    minimum_release_age: Option<chrono::Duration>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<VersionData> {
    let format = |v: &Version| v.to_string().split('+').next().unwrap().to_string();

    let head_version = versions
        .last()
        .map(|v| format(&v.version))
        .context("No versions available")?;

    // Like Cargo, versions without a publish time are never held back.
    let is_mature = |v: &IndexVersion| match (minimum_release_age, v.pubtime) {
        (Some(min_age), Some(pubtime)) => now - pubtime >= min_age,
        _ => true,
    };
    let mature: Vec<&Version> = versions
        .iter()
        .filter(|v| is_mature(v))
        .map(|v| &v.version)
        .collect();

    let current_req = VersionReq::parse(req)?;

    let mut target_minor = None;
    let mut target_major = None;

    let max_satisfying = versions
        .iter()
        .map(|v| &v.version)
        .filter(|v| current_req.matches(v))
        .max();

    if let Some(max_sat) = max_satisfying {
        let sat_req = VersionReq::parse(&format!("^{}", max_sat)).unwrap_or(current_req.clone());

        if let Some(mc) = mature.iter().filter(|v| sat_req.matches(v)).max() {
            if *mc > max_sat {
                target_minor = Some(format(mc));
            }
        }

        if let Some(mc) = mature
            .iter()
            .filter(|v| !sat_req.matches(v) && **v > max_sat)
            .max()
        {
            target_major = Some(format(mc));
        }
    } else {
        target_major = mature.last().map(|v| format(v));
    }

    Ok(VersionData {
        repo_url: None,
        head_major: Some(head_version.clone()),
        head_minor: Some(head_version),
        target_major,
        target_minor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(entries: &[(&str, Option<&str>)]) -> Vec<IndexVersion> {
        let body: Vec<String> = entries
            .iter()
            .map(|(vers, pubtime)| {
                serde_json::json!({ "name": "foo", "vers": vers, "yanked": false, "pubtime": pubtime })
                    .to_string()
            })
            .collect();
        parse_index(&body.join("\n"))
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        "2026-11-13T12:00:00Z".parse().unwrap()
    }

    #[test]
    fn young_versions_are_not_targets_but_are_the_head() {
        let versions = index(&[
            ("1.0.0", Some("2026-01-01T00:00:00Z")),
            ("1.1.0", Some("2026-11-01T00:00:00Z")),
            ("1.2.0", Some("2026-11-12T00:00:00Z")),
            ("2.0.0", Some("2026-11-01T00:00:00Z")),
            ("3.0.0", Some("2026-11-13T00:00:00Z")),
        ]);
        let data =
            select_versions(&versions, "=1.0.0", Some(chrono::Duration::days(3)), now()).unwrap();
        assert_eq!(data.target_minor.as_deref(), Some("1.1.0"));
        assert_eq!(data.target_major.as_deref(), Some("2.0.0"));
        assert_eq!(data.head_major.as_deref(), Some("3.0.0"));
    }

    #[test]
    fn without_minimum_the_newest_versions_are_targets() {
        let versions = index(&[
            ("1.0.0", Some("2026-01-01T00:00:00Z")),
            ("1.2.0", Some("2026-11-13T00:00:00Z")),
            ("3.0.0", Some("2026-11-13T00:00:00Z")),
        ]);
        let data = select_versions(&versions, "=1.0.0", None, now()).unwrap();
        assert_eq!(data.target_minor.as_deref(), Some("1.2.0"));
        assert_eq!(data.target_major.as_deref(), Some("3.0.0"));
    }

    #[test]
    fn versions_without_pubtime_are_never_held_back() {
        let versions = index(&[("1.0.0", None), ("1.1.0", None)]);
        let data =
            select_versions(&versions, "=1.0.0", Some(chrono::Duration::days(3)), now()).unwrap();
        assert_eq!(data.target_minor.as_deref(), Some("1.1.0"));
    }
}
