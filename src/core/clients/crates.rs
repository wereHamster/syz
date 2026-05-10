use crate::core::engine::VersionData;
use crate::core::http_agent::HttpAgent;
use anyhow::Result;
use semver::{Version, VersionReq};
use serde::Deserialize;

#[derive(Deserialize)]
struct CrateIndexLine {
    vers: String,
    yanked: bool,
}

#[derive(Clone)]
pub struct Crates {
    http_agent: HttpAgent,
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

    async fn get_index_versions(&self, package: &str) -> Result<Vec<Version>> {
        let url = Self::get_crates_io_url(package);

        let body = self.http_agent.text(&url).await?;
        let mut versions = Vec::new();

        for line in body.lines() {
            if let Ok(entry) = serde_json::from_str::<CrateIndexLine>(line) {
                if !entry.yanked {
                    if let Ok(v) = Version::parse(&entry.vers) {
                        versions.push(v);
                    }
                }
            }
        }

        if versions.is_empty() {
            anyhow::bail!("No non-yanked versions found for {}", package);
        }

        versions.sort();
        Ok(versions)
    }

    pub async fn get_versions(&self, name: &str, req: &str) -> Result<VersionData> {
        let versions = self.get_index_versions(name).await?;
        let head_version = versions
            .last()
            .unwrap()
            .to_string()
            .split('+')
            .next()
            .unwrap()
            .to_string();

        let current_req = VersionReq::parse(req)?;

        let mut target_minor = None;
        let mut target_major = None;

        let max_satisfying = versions.iter().filter(|v| current_req.matches(v)).max();

        if let Some(max_sat) = max_satisfying {
            let sat_req =
                VersionReq::parse(&format!("^{}", max_sat)).unwrap_or(current_req.clone());
            let max_compatible = versions.iter().filter(|v| sat_req.matches(v)).max();

            if let Some(mc) = max_compatible {
                if mc > max_sat {
                    target_minor = Some(mc.to_string().split('+').next().unwrap().to_string());
                }
            }

            let major_candidates: Vec<_> = versions
                .iter()
                .filter(|v| !sat_req.matches(v) && *v > max_sat)
                .collect();
            if let Some(mc) = major_candidates.last() {
                target_major = Some(mc.to_string().split('+').next().unwrap().to_string());
            }
        } else {
            target_major = Some(head_version.clone());
        }

        Ok(VersionData {
            repo_url: None,
            head_major: Some(head_version.clone()),
            head_minor: Some(head_version.clone()),
            target_major,
            target_minor,
        })
    }
}
