use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

use crate::core::engine::{repository::ProjectRepositorySnapshot, DiscoveredDependency, PURL};

#[derive(Deserialize)]
pub(crate) struct WorkspaceConfig {
    #[serde(rename = "minimumReleaseAge")]
    pub(crate) minimum_release_age: Option<i64>,
}

pub async fn run(repo: &dyn ProjectRepositorySnapshot) -> Result<Vec<DiscoveredDependency>> {
    let all_files = repo.list_files().await?;
    let mut pkg_json_paths = Vec::new();
    for file in all_files {
        if file.ends_with("package.json") {
            pkg_json_paths.push(file);
        }
    }

    if pkg_json_paths.is_empty() {
        return Ok(Vec::new());
    }

    let workspace_yaml = match repo.read_file("pnpm-workspace.yaml").await {
        Ok(yaml) => Some(yaml),
        Err(_) => None,
    };

    let mut minimum_release_age = None;
    if let Some(workspace_yaml) = workspace_yaml {
        if let Ok(config) = serde_yml::from_str::<WorkspaceConfig>(&workspace_yaml) {
            if let Some(age) = config.minimum_release_age {
                minimum_release_age = Some(chrono::Duration::minutes(age));
            }
        }
    }

    let mut all_deps = Vec::new();

    let mut locked_versions = std::collections::HashMap::new();
    if let Ok(lock_str) = repo.read_file("pnpm-lock.yaml").await {
        if let Ok(lock_val) = serde_yml::from_str::<serde_yml::Value>(&lock_str) {
            let mut extract_deps = |deps: &serde_yml::Value| {
                if let Some(map) = deps.as_mapping() {
                    for (k, v) in map {
                        if let Some(name) = k.as_str() {
                            if let Some(ver_str) = v.as_str() {
                                // pnpm lockfile v6 format: "1.2.3" or "1.2.3(peer...)"
                                locked_versions.insert(
                                    name.to_string(),
                                    ver_str.split('(').next().unwrap().to_string(),
                                );
                            } else if let Some(ver) = v.get("version").and_then(|v| v.as_str()) {
                                locked_versions.insert(
                                    name.to_string(),
                                    ver.split('(').next().unwrap().to_string(),
                                );
                            }
                        }
                    }
                }
            };

            if let Some(importers) = lock_val.get("importers").and_then(|i| i.as_mapping()) {
                for (_k, importer) in importers {
                    if let Some(deps) = importer.get("dependencies") {
                        extract_deps(deps);
                    }
                    if let Some(deps) = importer.get("devDependencies") {
                        extract_deps(deps);
                    }
                }
            } else {
                if let Some(deps) = lock_val.get("dependencies") {
                    extract_deps(deps);
                }
                if let Some(deps) = lock_val.get("devDependencies") {
                    extract_deps(deps);
                }
            }
        }
    }

    for pkg_json_path in pkg_json_paths {
        let pkg_json_opt = match repo.read_file(&pkg_json_path).await {
            Ok(json) => json,
            Err(_) => continue,
        };
        let pkg_json_str = pkg_json_opt;

        let pkg: Value = match serde_json::from_str(&pkg_json_str) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if let Some(deps) = pkg.get("dependencies").and_then(|d| d.as_object()) {
            for (name, req) in deps {
                if let Some(req_str) = req.as_str() {
                    let version = locked_versions.get(name).cloned();
                    let (namespace, parsed_name) = if name.starts_with('@') {
                        let parts: Vec<&str> = name.splitn(2, '/').collect();
                        if parts.len() == 2 {
                            (Some(parts[0].to_string()), parts[1].to_string())
                        } else {
                            (None, name.clone())
                        }
                    } else {
                        (None, name.clone())
                    };

                    all_deps.push(DiscoveredDependency {
                        purl: PURL {
                            ecosystem: "npm".to_string(),
                            namespace,
                            name: parsed_name,
                            subpath: None,
                            version,
                        },
                        requirement: req_str.to_string(),
                        minimum_release_age: minimum_release_age,
                    });
                }
            }
        }

        if let Some(deps) = pkg.get("devDependencies").and_then(|d| d.as_object()) {
            for (name, req) in deps {
                if let Some(req_str) = req.as_str() {
                    let version = locked_versions.get(name).cloned();
                    let (namespace, parsed_name) = if name.starts_with('@') {
                        let parts: Vec<&str> = name.splitn(2, '/').collect();
                        if parts.len() == 2 {
                            (Some(parts[0].to_string()), parts[1].to_string())
                        } else {
                            (None, name.clone())
                        }
                    } else {
                        (None, name.clone())
                    };

                    all_deps.push(DiscoveredDependency {
                        purl: PURL {
                            ecosystem: "npm".to_string(),
                            namespace,
                            name: parsed_name,
                            subpath: None,
                            version,
                        },
                        requirement: req_str.to_string(),
                        minimum_release_age: minimum_release_age,
                    });
                }
            }
        }
    }

    Ok(all_deps)
}
