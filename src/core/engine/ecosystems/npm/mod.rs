use anyhow::Result;
use async_trait::async_trait;

use crate::core::clients;
use crate::core::engine::ecosystems::{Registry, Scanner};
use crate::core::engine::repository::ProjectRepositorySnapshot;
use crate::core::engine::{DependencyUpdateOption, DiscoveredDependency};

pub mod internal;

use crate::core::engine::{repository::FileModification, UpdateTarget};
use std::fs;
use std::process::Command;

pub struct NpmScanner;

#[async_trait]
impl Scanner for NpmScanner {
    async fn discover_project_dependencies(
        &self,
        repo: &dyn ProjectRepositorySnapshot,
    ) -> Result<Vec<DiscoveredDependency>> {
        internal::discover_project_dependencies::run(repo).await
    }
}

pub struct NpmRegistry {
    npm_client: clients::npm::Npm,
}

impl NpmRegistry {
    pub fn new(npm_client: clients::npm::Npm) -> Self {
        Self { npm_client }
    }
}

#[async_trait]
impl Registry for NpmRegistry {
    async fn query_dependency_update_options(
        &self,
        dependency: &DiscoveredDependency,
    ) -> Result<DependencyUpdateOption> {
        internal::query_dependency_update_options::run(self.npm_client.clone(), dependency).await
    }

    async fn fetch_package_info(&self, name: &str) -> Result<crate::core::engine::PackageInfo> {
        self.npm_client.get_package_info(name).await
    }

    async fn fetch_release_history(
        &self,
        name: &str,
        current_version: &str,
        target_version: &str,
    ) -> Result<Vec<crate::core::engine::Release>> {
        self.npm_client
            .get_release_history(name, current_version, target_version)
            .await
    }
}

pub struct NpmPatcher;

#[async_trait]
impl crate::core::engine::ecosystems::Patcher for NpmPatcher {
    fn updated_requirement(&self, old_req: &str, target_version: &str) -> Option<String> {
        let prefix = if old_req.starts_with('^') {
            "^"
        } else if old_req.starts_with('~') {
            "~"
        } else if old_req.starts_with('=') {
            "="
        } else if old_req.starts_with('v') {
            "v"
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
        snapshot: &dyn ProjectRepositorySnapshot,
        temp_dir: &std::path::Path,
        targets: &[UpdateTarget],
    ) -> Result<Vec<FileModification>> {
        let workspace = snapshot.read_file("pnpm-workspace.yaml").await.ok();
        let lockfile = snapshot.read_file("pnpm-lock.yaml").await.ok();

        tracing::info!("Fetching repository tree for NPM updates...");
        let files = snapshot.list_files().await?;

        let mut pkg_jsons_to_fetch = Vec::new();
        for path in files {
            if path.ends_with("package.json") {
                pkg_jsons_to_fetch.push(path.to_string());
            }
        }

        let mut tree_items = Vec::new();

        for path in &pkg_jsons_to_fetch {
            if let Ok(content) = snapshot.read_file(path).await {
                let mut pkg_json: serde_json::Value = match serde_json::from_str(&content) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let mut was_updated = false;

                for target in targets {
                    for key in ["dependencies", "devDependencies", "peerDependencies"] {
                        if let Some(deps) = pkg_json.get_mut(key).and_then(|d| d.as_object_mut()) {
                            if deps.contains_key(&target.name) {
                                deps.insert(
                                    target.name.to_string(),
                                    serde_json::Value::String(
                                        target.target_version.requirement.clone(),
                                    ),
                                );
                                was_updated = true;
                            }
                        }
                    }
                }

                let full_path = temp_dir.join(path);
                if let Some(parent) = full_path.parent() {
                    fs::create_dir_all(parent)?;
                }

                if was_updated {
                    let updated_pkg_json_str = serde_json::to_string_pretty(&pkg_json)? + "\n";
                    fs::write(&full_path, &updated_pkg_json_str)?;
                    tree_items.push(FileModification {
                        path: path.clone(),
                        state: crate::core::engine::repository::FileState::Write(
                            updated_pkg_json_str,
                        ),
                    });
                } else {
                    fs::write(&full_path, content)?;
                }
            }
        }

        if let Some(ref w) = workspace {
            fs::write(temp_dir.join("pnpm-workspace.yaml"), w)?;
        }
        if let Some(ref l) = lockfile {
            fs::write(temp_dir.join("pnpm-lock.yaml"), l)?;
        }

        tracing::info!("Running pnpm install in temp directory to update lockfile...");
        let mut cmd = Command::new("pnpm");
        cmd.arg("install")
            .arg("--lockfile-only")
            .arg("--ignore-scripts");

        if workspace.is_some() {
            cmd.arg("--recursive");
        }

        let status = cmd.current_dir(temp_dir).status()?;

        if !status.success() {
            tracing::info!("Fallback: Running without --lockfile-only...");
            let mut fallback_cmd = Command::new("pnpm");
            fallback_cmd.arg("install").arg("--ignore-scripts");
            if workspace.is_some() {
                fallback_cmd.arg("--recursive");
            }

            let fallback_status = fallback_cmd.current_dir(temp_dir).status()?;

            if !fallback_status.success() {
                anyhow::bail!("pnpm install failed");
            }
        }

        tracing::info!("Running pnpm dedupe...");
        let mut dedupe_cmd = Command::new("pnpm");
        dedupe_cmd.arg("dedupe").arg("--ignore-scripts");
        let dedupe_status = dedupe_cmd.current_dir(temp_dir).status()?;
        if !dedupe_status.success() {
            tracing::warn!("pnpm dedupe failed, continuing anyway...");
        }

        let updated_lock = fs::read_to_string(temp_dir.join("pnpm-lock.yaml")).ok();

        if let Some(lock) = updated_lock {
            let old_lock = lockfile.unwrap_or_default();
            if lock != old_lock {
                tree_items.push(FileModification {
                    path: "pnpm-lock.yaml".to_string(),
                    state: crate::core::engine::repository::FileState::Write(lock),
                });
            }
        }

        Ok(tree_items)
    }
}
