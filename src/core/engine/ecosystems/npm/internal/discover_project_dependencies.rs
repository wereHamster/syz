use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

use crate::core::engine::{repository::ProjectRepositorySnapshot, DiscoveredDependency, PURL};

#[derive(Deserialize)]
pub(crate) struct WorkspaceConfig {
    #[serde(rename = "minimumReleaseAge")]
    pub(crate) minimum_release_age: Option<i64>,
}

/// pnpm's default `minimumReleaseAge` (in minutes) since v11.
const PNPM_DEFAULT_MINIMUM_RELEASE_AGE_MINUTES: i64 = 1440;

/// The `minimumReleaseAge` pnpm enforces for a `pnpm-workspace.yaml`, falling back to pnpm's
/// default when the setting (or the file) is missing.
pub(crate) fn effective_minimum_release_age(workspace: Option<&str>) -> chrono::Duration {
    let minutes = workspace
        .and_then(|w| noyalib::from_str::<WorkspaceConfig>(w).ok())
        .and_then(|config| config.minimum_release_age)
        .unwrap_or(PNPM_DEFAULT_MINIMUM_RELEASE_AGE_MINUTES);
    chrono::Duration::minutes(minutes)
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

    let workspace_yaml = repo.read_file("pnpm-workspace.yaml").await.ok();

    let minimum_release_age = Some(effective_minimum_release_age(workspace_yaml.as_deref()));

    let mut all_deps = Vec::new();

    let mut locked_versions = std::collections::HashMap::new();
    if let Ok(lock_str) = repo.read_file("pnpm-lock.yaml").await {
        // Since pnpm 11 the lockfile can hold several YAML documents: one for pnpm's own
        // environment (`packageManagerDependencies`), then the project's lockfile.
        let documents = noyalib::load_all(&lock_str)
            .map(|docs| docs.filter_map(Result::ok).collect::<Vec<_>>())
            .unwrap_or_default();
        for lock_val in documents {
            let mut extract_deps = |deps: &noyalib::Value| {
                if let Some(map) = deps.as_mapping() {
                    for (k, v) in map {
                        {
                            let name = k;
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
                    if req_str.starts_with("workspace:") {
                        continue;
                    }
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
                        minimum_release_age,
                    });
                }
            }
        }

        if let Some(deps) = pkg.get("devDependencies").and_then(|d| d.as_object()) {
            for (name, req) in deps {
                if let Some(req_str) = req.as_str() {
                    if req_str.starts_with("workspace:") {
                        continue;
                    }
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
                        minimum_release_age,
                    });
                }
            }
        }
    }

    Ok(all_deps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::repository::test_support::MockSnapshot;

    const PACKAGE_JSON: &str = r#"{
  "dependencies": { "react": "^19.0.0" },
  "devDependencies": { "@types/react": "^19.0.0" }
}"#;

    fn locked_versions(deps: &[DiscoveredDependency]) -> Vec<(String, Option<String>)> {
        let mut versions: Vec<_> = deps
            .iter()
            .map(|d| {
                let name = match &d.purl.namespace {
                    Some(ns) => format!("{}/{}", ns, d.purl.name),
                    None => d.purl.name.clone(),
                };
                (name, d.purl.version.clone())
            })
            .collect();
        versions.sort();
        versions
    }

    #[tokio::test]
    async fn reads_locked_versions_from_single_document_lockfile() {
        let lockfile = "lockfileVersion: '9.0'

importers:

  .:
    dependencies:
      react:
        specifier: ^19.0.0
        version: 19.3.0
    devDependencies:
      '@types/react':
        specifier: ^19.0.0
        version: 19.3.0
";
        let repo = MockSnapshot::with_files(&[
            ("package.json", PACKAGE_JSON),
            ("pnpm-lock.yaml", lockfile),
        ]);

        let deps = run(&repo).await.unwrap();

        assert_eq!(
            locked_versions(&deps),
            vec![
                ("@types/react".to_string(), Some("19.3.0".to_string())),
                ("react".to_string(), Some("19.3.0".to_string())),
            ]
        );
    }

    /// pnpm 11+ prepends a document for its own environment to the lockfile.
    #[tokio::test]
    async fn reads_locked_versions_from_multi_document_lockfile() {
        let lockfile = "---
lockfileVersion: '9.0'

importers:

  .:
    configDependencies: {}
    packageManagerDependencies:
      pnpm:
        specifier: 12.0.0
        version: 12.0.0

packages:

  pnpm@12.0.0:
    resolution: {integrity: sha512-AAAA}

---
lockfileVersion: '9.0'

settings:
  autoInstallPeers: true
  excludeLinksFromLockfile: false

importers:

  .:
    dependencies:
      react:
        specifier: ^19.0.0
        version: 19.3.0
    devDependencies:
      '@types/react':
        specifier: ^19.0.0
        version: 19.3.0(react@19.3.0)
";
        let repo = MockSnapshot::with_files(&[
            ("package.json", PACKAGE_JSON),
            ("pnpm-lock.yaml", lockfile),
        ]);

        let deps = run(&repo).await.unwrap();

        assert_eq!(
            locked_versions(&deps),
            vec![
                ("@types/react".to_string(), Some("19.3.0".to_string())),
                ("react".to_string(), Some("19.3.0".to_string())),
            ]
        );
    }
}
