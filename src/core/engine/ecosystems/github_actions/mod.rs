use anyhow::Result;
use async_trait::async_trait;

use crate::core::clients;
use crate::core::engine::ecosystems::Ecosystem;
use crate::core::engine::repository::ProjectRepositorySnapshot;
use crate::core::engine::{DependencyUpdateOption, DiscoveredDependency};

pub mod internal;

pub struct GitHub {
    github_client: clients::github::GitHub,
}

impl GitHub {
    pub fn new(github_client: clients::github::GitHub) -> Self {
        Self { github_client }
    }
}

#[async_trait]
impl Ecosystem for GitHub {
    async fn discover_project_dependencies(
        &self,
        repo: &dyn ProjectRepositorySnapshot,
    ) -> Result<Vec<DiscoveredDependency>> {
        internal::discover_project_dependencies::run(repo).await
    }

    async fn query_dependency_update_options(
        &self,
        dependency: &DiscoveredDependency,
    ) -> Result<DependencyUpdateOption> {
        internal::query_dependency_update_options::run(self.github_client.clone(), dependency).await
    }
}
