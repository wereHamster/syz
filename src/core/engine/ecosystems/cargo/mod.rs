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
        let root_toml_str = repo
            .read_file("Cargo.toml")
            .await
            .context("Missing Cargo.toml")?;
        let root_doc = root_toml_str
            .parse::<DocumentMut>()
            .context("Invalid Cargo.toml")?;

        let lockfile = repo.read_file("Cargo.lock").await.ok();

        let members = internal::workspace::resolve_members(&root_doc, repo).await?;

        // (path, dir, doc) for the root manifest plus every workspace member.
        let mut manifests: Vec<(String, String, DocumentMut)> =
            vec![("Cargo.toml".to_string(), String::new(), root_doc)];

        for member in &members {
            match repo.read_file(&member.manifest_path).await {
                Ok(content) => match content.parse::<DocumentMut>() {
                    Ok(doc) => {
                        manifests.push((member.manifest_path.clone(), member.dir.clone(), doc))
                    }
                    Err(_) => tracing::warn!(
                        "Skipping cargo workspace member with invalid manifest: {}",
                        member.manifest_path
                    ),
                },
                Err(_) => tracing::warn!(
                    "Skipping cargo workspace member with missing manifest: {}",
                    member.manifest_path
                ),
            }
        }

        let mut changed_paths = std::collections::HashSet::new();

        for (path, _dir, doc) in &mut manifests {
            let before = doc.to_string();
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
                                        Item::Value(
                                            target.target_version.requirement.clone().into(),
                                        ),
                                    );
                                }
                            }
                        }
                    }
                }
            }
            if doc.to_string() != before {
                changed_paths.insert(path.clone());
            }
        }

        for (path, dir, doc) in &manifests {
            let dest_path = temp_dir.join(path);
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(dest_path, doc.to_string())?;

            for src_file in ["src/lib.rs", "src/main.rs"] {
                let repo_src_path = if dir.is_empty() {
                    src_file.to_string()
                } else {
                    format!("{dir}/{src_file}")
                };
                if let Ok(content) = repo.read_file(&repo_src_path).await {
                    let dest_src_path = temp_dir.join(std::path::Path::new(&repo_src_path));
                    if let Some(parent) = dest_src_path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(dest_src_path, &content)?;
                }
            }
        }

        if let Some(ref l) = lockfile {
            fs::write(temp_dir.join("Cargo.lock"), l)?;
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
            anyhow::bail!("cargo generate-lockfile failed in {:?}", temp_dir);
        }

        // The root manifest is always included, matching prior behavior; workspace
        // member manifests are only included in the diff when actually changed, so
        // an update doesn't touch every crate's Cargo.toml in the PR.
        let mut tree_items = Vec::new();
        for (idx, (path, _dir, _doc)) in manifests.iter().enumerate() {
            if idx == 0 || changed_paths.contains(path) {
                let updated = fs::read_to_string(temp_dir.join(path))?;
                tree_items.push(FileModification {
                    path: path.clone(),
                    state: FileState::Write(updated),
                });
            }
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::repository::test_support::MockSnapshot;
    use crate::core::engine::{PackageInfo, RequirementVersion};

    fn make_target(name: &str, current: &str, target: &str) -> UpdateTarget {
        UpdateTarget {
            name: name.to_string(),
            current_version: RequirementVersion {
                requirement: current.to_string(),
                version: current.to_string(),
            },
            target_version: RequirementVersion {
                requirement: target.to_string(),
                version: target.to_string(),
            },
            latest_version: target.to_string(),
            package_info: PackageInfo { repo_url: None },
            minimum_release_age: None,
        }
    }

    #[tokio::test]
    async fn single_package_repo_generates_lockfile_and_includes_root_manifest() {
        let repo = MockSnapshot::with_files(&[
            (
                "Cargo.toml",
                "[package]\nname = \"foo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", ""),
        ]);
        let temp_dir = tempfile::tempdir().unwrap();

        let patcher = CargoPatcher::new();
        let result = patcher
            .apply_updates(&repo, temp_dir.path(), &[])
            .await
            .unwrap();

        assert!(result.iter().any(|m| m.path == "Cargo.toml"));
        // No Cargo.lock existed before, so the freshly generated one is new
        // content and is included in the diff too.
        assert!(result.iter().any(|m| m.path == "Cargo.lock"));
        assert!(temp_dir.path().join("Cargo.lock").exists());
    }

    #[tokio::test]
    async fn workspace_member_only_dependency_is_bumped_and_lockfile_regenerated() {
        let repo = MockSnapshot::with_files(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/tdsm\", \"crates/base\"]\n",
            ),
            (
                "crates/base/Cargo.toml",
                "[package]\nname = \"base\"\nversion = \"0.2.0\"\nedition = \"2021\"\n",
            ),
            ("crates/base/src/lib.rs", ""),
            (
                "crates/tdsm/Cargo.toml",
                "[package]\nname = \"tdsm\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nbase = { path = \"../base\", version = \"0.1.0\" }\n",
            ),
            ("crates/tdsm/src/lib.rs", ""),
        ]);
        let temp_dir = tempfile::tempdir().unwrap();

        let patcher = CargoPatcher::new();
        let targets = vec![make_target("base", "0.1.0", "0.2.0")];
        let result = patcher
            .apply_updates(&repo, temp_dir.path(), &targets)
            .await
            .unwrap();

        let tdsm_toml = result
            .iter()
            .find(|m| m.path == "crates/tdsm/Cargo.toml")
            .expect("crates/tdsm/Cargo.toml should be in the diff");
        match &tdsm_toml.state {
            FileState::Write(content) => assert!(content.contains("0.2.0")),
            FileState::Delete => panic!("expected a Write"),
        }

        // The root manifest declares no dependencies, so it isn't touched by
        // this bump, but is still included per existing root-manifest behavior.
        assert!(result.iter().any(|m| m.path == "Cargo.toml"));

        let lock_modification = result
            .iter()
            .find(|m| m.path == "Cargo.lock")
            .expect("Cargo.lock should be regenerated and included in the diff");
        match &lock_modification.state {
            FileState::Write(content) => assert!(content.contains("name = \"base\"")),
            FileState::Delete => panic!("expected a Write"),
        }
    }

    #[tokio::test]
    async fn missing_workspace_member_manifest_fails_the_update_instead_of_dropping_the_lockfile() {
        let repo = MockSnapshot::with_files(&[(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/ghost\"]\n",
        )]);
        let temp_dir = tempfile::tempdir().unwrap();

        let patcher = CargoPatcher::new();
        let result = patcher.apply_updates(&repo, temp_dir.path(), &[]).await;

        assert!(
            result.is_err(),
            "a broken workspace must fail the update, not silently produce a Cargo.toml-only diff"
        );
    }
}
