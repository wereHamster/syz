use anyhow::Result;
use regex::Regex;

use crate::core::engine::{repository::ProjectRepositorySnapshot, DiscoveredDependency, PURL};

pub async fn run(repo: &dyn ProjectRepositorySnapshot) -> Result<Vec<DiscoveredDependency>> {
    let files = repo.list_files().await?;
    let mut deps = Vec::new();
    let re = Regex::new(r"^(?P<indent>[ \t]*-?[ \t]*uses:\s+)(?P<action>[a-zA-Z0-9_.-]+/[a-zA-Z0-9_./-]+)@(?P<ref>[^#\s]+)(?:[ \t]+#[ \t]*(?P<comment>v?[\d\.]+.*))?").unwrap();

    for path in files {
        if path.starts_with(".github/workflows/")
            && (path.ends_with(".yml") || path.ends_with(".yaml"))
        {
            if let Ok(content) = repo.read_file(&path).await {
                for line in content.lines() {
                    if let Some(caps) = re.captures(line) {
                        let action = caps.name("action").unwrap().as_str().to_string();
                        let action_ref = caps.name("ref").unwrap().as_str().to_string();
                        let comment = caps.name("comment").map(|m| m.as_str().to_string());

                        // Split owner/repo/subpath
                        let parts: Vec<&str> = action.splitn(3, '/').collect();
                        if parts.len() < 2 {
                            continue;
                        }

                        let namespace = Some(parts[0].to_string());
                        let name = parts[1].to_string();
                        let subpath = if parts.len() == 3 {
                            Some(parts[2].to_string())
                        } else {
                            None
                        };

                        let requirement = if let Some(c) = comment {
                            c
                        } else {
                            action_ref.clone()
                        };

                        deps.push(DiscoveredDependency {
                            purl: PURL {
                                ecosystem: "github-actions".to_string(),
                                namespace,
                                name,
                                subpath,
                                version: Some(action_ref),
                            },
                            requirement,
                            minimum_release_age: Some(chrono::Duration::days(15)),
                        });
                    }
                }
            }
        }
    }

    Ok(deps)
}
