use anyhow::{Context, Result};
use toml_edit::{DocumentMut, Item};

use crate::core::engine::{DiscoveredDependency, PURL, repository::ProjectRepositorySnapshot};

pub async fn run(repo: &dyn ProjectRepositorySnapshot) -> Result<Vec<DiscoveredDependency>> {
    let toml_opt = match repo.read_file("Cargo.toml").await {
        Ok(toml) => toml,
        Err(_) => return Ok(Vec::new()),
    };
    let toml_str = toml_opt;

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

    Ok(deps)
}
