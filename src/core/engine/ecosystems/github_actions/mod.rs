use anyhow::Result;
use async_trait::async_trait;

use crate::core::clients;
use crate::core::engine::ecosystems::{Registry, Scanner};
use crate::core::engine::repository::ProjectRepositorySnapshot;
use crate::core::engine::{DependencyUpdateOption, DiscoveredDependency};

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
}
