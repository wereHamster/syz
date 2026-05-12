use anyhow::Result;
use async_trait::async_trait;

use crate::core::clients;
use crate::core::engine::ecosystems::{Patcher, Registry, Scanner};
use crate::core::engine::repository::{FileModification, ProjectRepositorySnapshot};
use crate::core::engine::{DependencyUpdateOption, DiscoveredDependency, UpdateTarget};

pub mod internal;

pub struct GitHubScanner;

#[async_trait]
impl Scanner for GitHubScanner {
    async fn discover_project_dependencies(
        &self,
        repo: &dyn ProjectRepositorySnapshot,
    ) -> Result<Vec<DiscoveredDependency>> {
        internal::discover_project_dependencies::run(repo).await
    }
}

pub struct GitHubRegistry {
    github_client: clients::github::GitHub,
}

impl GitHubRegistry {
    pub fn new(github_client: clients::github::GitHub) -> Self {
        Self { github_client }
    }
}

#[async_trait]
impl Registry for GitHubRegistry {
    async fn query_dependency_update_options(
        &self,
        dependency: &DiscoveredDependency,
    ) -> Result<DependencyUpdateOption> {
        internal::query_dependency_update_options::run(self.github_client.clone(), dependency).await
    }

    async fn fetch_package_info(&self, name: &str) -> Result<crate::core::engine::PackageInfo> {
        self.github_client.get_package_info(name).await
    }

    async fn fetch_release_history(
        &self,
        name: &str,
        current_version: &str,
        target_version: &str,
    ) -> Result<Vec<crate::core::engine::Release>> {
        self.github_client
            .get_release_history(name, current_version, target_version)
            .await
    }
}

pub struct GitHubPatcher {
    github_client: clients::github::GitHub,
}

impl GitHubPatcher {
    pub fn new(github_client: clients::github::GitHub) -> Self {
        Self { github_client }
    }
}

#[async_trait]
impl Patcher for GitHubPatcher {
    fn updated_requirement(&self, old_req: &str, target_version: &str) -> Option<String> {
        if target_version.len() == 40 {
            return Some(target_version.to_string());
        }

        let prefix = if old_req.starts_with('v') { "v" } else { "" };
        let new_req = format!("{}{}", prefix, target_version.trim_start_matches('v'));
        if old_req != new_req {
            Some(new_req)
        } else {
            None
        }
    }

    async fn apply_updates(
        &self,
        snapshot: &dyn ProjectRepositorySnapshot,
        _temp_dir: &std::path::Path,
        targets: &[UpdateTarget],
    ) -> Result<Vec<FileModification>> {
        let files = snapshot.list_files().await?;
        let mut tree_items = Vec::new();

        let re = regex::Regex::new(r"^(?P<indent>[ \t]*-?[ \t]*uses:\s+)(?P<action>[a-zA-Z0-9_.-]+/[a-zA-Z0-9_./-]+)@(?P<ref>[^#\s]+)(?:[ \t]+#[ \t]*(?P<comment>v?[\d\.]+.*))?").unwrap();

        let mut target_map: std::collections::HashMap<String, &UpdateTarget> =
            std::collections::HashMap::new();
        for t in targets {
            target_map.insert(t.name.clone(), t);
        }

        for path in files {
            if path.starts_with(".github/workflows/")
                && (path.ends_with(".yml") || path.ends_with(".yaml"))
            {
                if let Ok(content) = snapshot.read_file(&path).await {
                    let mut updated_lines = Vec::new();
                    let mut file_changed = false;

                    for line in content.lines() {
                        if let Some(caps) = re.captures(line) {
                            let action = caps.name("action").unwrap().as_str().to_string();

                            if let Some(target) = target_map.get(&action) {
                                file_changed = true;
                                let indent = caps.name("indent").unwrap().as_str();
                                let comment = caps.name("comment");

                                let commit_sha = target.target_version.requirement.clone();
                                let action_repo = internal::helpers::extract_repo(&action);

                                let pretty_tag = if commit_sha.len() == 40 {
                                    internal::helpers::get_tag_for_sha(
                                        &self.github_client,
                                        &action_repo,
                                        &commit_sha,
                                    )
                                    .await
                                } else {
                                    None
                                };

                                if comment.is_some() {
                                    if let Some(tag) = pretty_tag {
                                        updated_lines.push(format!(
                                            "{}{}@{} # {}",
                                            indent, action, commit_sha, tag
                                        ));
                                    } else {
                                        updated_lines
                                            .push(format!("{}{}@{}", indent, action, commit_sha));
                                    }
                                } else {
                                    updated_lines
                                        .push(format!("{}{}@{}", indent, action, commit_sha));
                                }
                                continue;
                            }
                        }
                        updated_lines.push(line.to_string());
                    }

                    if file_changed {
                        let new_content = updated_lines.join("\n") + "\n";
                        tree_items.push(FileModification {
                            path: path.to_string(),
                            state: crate::core::engine::repository::FileState::Write(new_content),
                        });
                    }
                }
            }
        }

        Ok(tree_items)
    }
}
