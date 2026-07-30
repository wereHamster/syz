use anyhow::{Context, Result};
use async_trait::async_trait;
use std::fs;
use std::process::Command;
use toml_edit::{DocumentMut, Item};

use crate::core::clients;
use crate::core::engine::ecosystems::{Patcher, Registry, Scanner};
use crate::core::engine::repository::{FileModification, FileState, ProjectRepositorySnapshot};
use crate::core::engine::{DependencyUpdateOption, DiscoveredDependency, UpdateTarget};

pub mod internal;

pub struct CargoScanner;

#[async_trait]
impl Scanner for CargoScanner {
    async fn discover_project_dependencies(
        &self,
        repo: &dyn ProjectRepositorySnapshot,
    ) -> Result<Vec<DiscoveredDependency>> {
        internal::discover_project_dependencies::run(repo).await
    }
}

pub struct CargoRegistry {
    crates_client: clients::crates::Crates,
}

impl CargoRegistry {
    pub fn new(crates_client: clients::crates::Crates) -> Self {
        Self { crates_client }
    }
}

#[async_trait]
impl Registry for CargoRegistry {
    async fn query_dependency_update_options(
        &self,
        dependency: &DiscoveredDependency,
    ) -> Result<DependencyUpdateOption> {
        internal::query_dependency_update_options::run(self.crates_client.clone(), dependency).await
    }

    async fn fetch_package_info(&self, name: &str) -> Result<crate::core::engine::PackageInfo> {
        self.crates_client.get_package_info(name).await
    }

    async fn fetch_release_history(
        &self,
        name: &str,
        current_version: &str,
        target_version: &str,
    ) -> Result<Vec<crate::core::engine::Release>> {
        self.crates_client
            .get_release_history(name, current_version, target_version)
            .await
    }
}

pub struct CargoPatcher;

impl CargoPatcher {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CargoPatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Patcher for CargoPatcher {
    fn updated_requirement(&self, old_req: &str, target_version: &str) -> Option<String> {
        let prefix = if old_req.starts_with('^') {
            "^"
        } else if old_req.starts_with('~') {
            "~"
        } else if old_req.starts_with('=') {
            "="
        } else {
            ""
        };
        let new_req = format!("{}{}", prefix, target_version);
        if old_req != new_req {
            Some(new_req)
        } else {
            None
        }
    }

    async fn apply_updates(
        &self,
        repo: &dyn ProjectRepositorySnapshot,
        temp_dir: &std::path::Path,
        targets: &[UpdateTarget],
    ) -> Result<Vec<FileModification>> {
        let toml_str = repo
            .read_file("Cargo.toml")
            .await
            .context("Missing Cargo.toml")?;

        let lockfile = repo.read_file("Cargo.lock").await.ok();

        let mut doc = toml_str
            .parse::<DocumentMut>()
            .context("Invalid Cargo.toml")?;

        for target in targets {
            for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
                if let Some(Item::Table(table)) = doc.get_mut(key) {
                    if let Some(val) = table.get_mut(&target.name) {
                        if let Some(s) = val.as_value_mut() {
                            if s.is_str() {
                                *s = target.target_version.requirement.clone().into();
                            } else if let Some(t) = s.as_inline_table_mut() {
                                if t.contains_key("version") {
                                    t.insert(
                                        "version",
                                        target.target_version.requirement.clone().into(),
                                    );
                                }
                            }
                        } else if let Some(t) = val.as_table_mut() {
                            if t.contains_key("version") {
                                t.insert(
                                    "version",
                                    Item::Value(target.target_version.requirement.clone().into()),
                                );
                            }
                        }
                    }
                }
            }
        }

        let updated_toml_str = doc.to_string();
        fs::write(temp_dir.join("Cargo.toml"), &updated_toml_str)?;

        if let Some(ref l) = lockfile {
            fs::write(temp_dir.join("Cargo.lock"), l)?;
        }

        for src_file in ["src/lib.rs", "src/main.rs"] {
            if let Ok(content) = repo.read_file(src_file).await {
                let src_path = std::path::Path::new(src_file);
                let dest_path = temp_dir.join(src_path);
                if let Some(parent) = dest_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(dest_path, &content)?;
            }
        }

        let temp_dir_owned = temp_dir.to_path_buf();
        let status = tokio::task::spawn_blocking(move || {
            Command::new("cargo")
                .arg("generate-lockfile")
                .current_dir(&temp_dir_owned)
                .status()
        })
        .await
        .map_err(|e| anyhow::anyhow!("Task join error: {}", e))??;
        if !status.success() {
            tracing::warn!("cargo generate-lockfile failed, continuing anyway...");
        }

        let updated_toml = fs::read_to_string(temp_dir.join("Cargo.toml"))?;
        let mut tree_items = vec![FileModification {
            path: "Cargo.toml".to_string(),
            state: FileState::Write(updated_toml),
        }];

        if let Ok(updated_lock) = fs::read_to_string(temp_dir.join("Cargo.lock")) {
            let old_lock = lockfile.unwrap_or_default();
            if updated_lock != old_lock {
                tree_items.push(FileModification {
                    path: "Cargo.lock".to_string(),
                    state: FileState::Write(updated_lock),
                });
            }
        }

        Ok(tree_items)
    }
}
