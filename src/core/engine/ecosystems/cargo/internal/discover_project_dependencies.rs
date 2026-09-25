use anyhow::{Context, Result};
use toml_edit::{DocumentMut, Item};

use crate::core::engine::{repository::ProjectRepositorySnapshot, DiscoveredDependency, PURL};

use super::workspace;

pub async fn run(repo: &dyn ProjectRepositorySnapshot) -> Result<Vec<DiscoveredDependency>> {
    let toml_str = match repo.read_file("Cargo.toml").await {
        Ok(toml) => toml,
        Err(_) => return Ok(Vec::new()),
    };

    let doc = toml_str
        .parse::<DocumentMut>()
        .context("Invalid Cargo.toml")?;
    let mut deps = Vec::new();

    let mut locked_versions = std::collections::HashMap::new();
    if let Ok(lock_str) = repo.read_file("Cargo.lock").await {
        if let Ok(lock_doc) = lock_str.parse::<DocumentMut>() {
            if let Some(Item::ArrayOfTables(packages)) = lock_doc.get("package") {
                for pkg in packages.iter() {
                    if let (Some(name), Some(version)) = (
                        pkg.get("name").and_then(|n| n.as_str()),
                        pkg.get("version").and_then(|v| v.as_str()),
                    ) {
                        locked_versions.insert(name.to_string(), version.to_string());
                    }
                }
            }
        }
    }

    collect_deps(&doc, &locked_versions, &mut deps);

    for member in workspace::resolve_members(&doc, repo).await? {
        let member_toml = match repo.read_file(&member.manifest_path).await {
            Ok(content) => content,
            Err(_) => {
                tracing::warn!(
                    "Skipping cargo workspace member with missing manifest: {}",
                    member.manifest_path
                );
                continue;
            }
        };
        let member_doc = match member_toml.parse::<DocumentMut>() {
            Ok(doc) => doc,
            Err(_) => {
                tracing::warn!(
                    "Skipping cargo workspace member with invalid manifest: {}",
                    member.manifest_path
                );
                continue;
            }
        };
        collect_deps(&member_doc, &locked_versions, &mut deps);
    }

    Ok(deps)
}

/// Walks the `dependencies`/`dev-dependencies`/`build-dependencies` tables of
/// a parsed manifest and appends any resolvable entries to `deps`. Entries
/// using inherited `dep.workspace = true` syntax have no `version` key and
/// are skipped, same as before.
fn collect_deps(
    doc: &DocumentMut,
    locked_versions: &std::collections::HashMap<String, String>,
    deps: &mut Vec<DiscoveredDependency>,
) {
    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(Item::Table(table)) = doc.get(key) {
            for (name, value) in table.iter() {
                let req = if let Some(s) = value.as_str() {
                    s.to_string()
                } else if let Some(inline_table) = value.as_inline_table() {
                    if let Some(v) = inline_table.get("version").and_then(|v| v.as_str()) {
                        v.to_string()
                    } else {
                        continue;
                    }
                } else if let Some(table) = value.as_table() {
                    if let Some(v) = table.get("version").and_then(|v| v.as_str()) {
                        v.to_string()
                    } else {
                        continue;
                    }
                } else {
                    continue;
                };
                let version = locked_versions.get(name).cloned();
                deps.push(DiscoveredDependency {
                    purl: PURL {
                        ecosystem: "cargo".to_string(),
                        namespace: None,
                        name: name.to_string(),
                        subpath: None,
                        version,
                    },
                    requirement: req,
                    minimum_release_age: None,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::repository::test_support::MockSnapshot;

    #[tokio::test]
    async fn single_package_repo_discovers_root_dependencies() {
        let repo = MockSnapshot::with_files(&[(
            "Cargo.toml",
            "[package]\nname = \"foo\"\n\n[dependencies]\nserde = \"1.0.0\"\n",
        )]);
        let deps = run(&repo).await.unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].purl.name, "serde");
    }

    #[tokio::test]
    async fn workspace_member_only_dependency_is_discovered() {
        let repo = MockSnapshot::with_files(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/tdsm\"]\n\n[package]\nname = \"syz\"\n",
            ),
            (
                "crates/tdsm/Cargo.toml",
                "[package]\nname = \"tdsm\"\n\n[dependencies]\nsqlparser = \"0.62.0\"\n",
            ),
        ]);
        let deps = run(&repo).await.unwrap();
        assert!(deps.iter().any(|d| d.purl.name == "sqlparser"));
    }
}
