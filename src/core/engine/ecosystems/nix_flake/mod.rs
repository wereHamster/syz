use anyhow::Result;
use async_trait::async_trait;

use crate::core::clients;
use crate::core::engine::ecosystems::{Patcher, Registry, Scanner};
use crate::core::engine::repository::{FileModification, ProjectRepositorySnapshot};
use crate::core::engine::{DependencyUpdateOption, DiscoveredDependency, UpdateTarget};

pub mod internal;

pub struct NixFlakeScanner;

#[async_trait]
impl Scanner for NixFlakeScanner {
    async fn discover_project_dependencies(
        &self,
        repo: &dyn ProjectRepositorySnapshot,
    ) -> Result<Vec<DiscoveredDependency>> {
        internal::discover_project_dependencies::run(repo).await
    }
}

pub struct NixFlakeRegistry {
    github_client: clients::github::GitHub,
}

impl NixFlakeRegistry {
    pub fn new(github_client: clients::github::GitHub) -> Self {
        Self { github_client }
    }
}

#[async_trait]
impl Registry for NixFlakeRegistry {
    async fn query_dependency_update_options(
        &self,
        dependency: &DiscoveredDependency,
    ) -> Result<DependencyUpdateOption> {
        internal::query_dependency_update_options::run(self.github_client.clone(), dependency)
            .await
    }

    async fn fetch_package_info(&self, name: &str) -> Result<crate::core::engine::PackageInfo> {
        let repo_url = internal::helpers::parse_github_repo(name)
            .map(|repo| format!("https://github.com/{}/{}", repo.owner, repo.repo));

        Ok(crate::core::engine::PackageInfo { repo_url })
    }
}

pub struct NixFlakePatcher;

#[async_trait]
impl Patcher for NixFlakePatcher {
    async fn apply_updates(
        &self,
        _snapshot: &dyn ProjectRepositorySnapshot,
        _temp_dir: &std::path::Path,
        _targets: &[UpdateTarget],
    ) -> Result<Vec<FileModification>> {
        Ok(Vec::new())
    }
}
