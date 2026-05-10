use anyhow::Result;
use async_trait::async_trait;

use crate::core::clients;
use crate::core::engine::ecosystems::{Registry, Scanner};
use crate::core::engine::repository::ProjectRepositorySnapshot;
use crate::core::engine::{DependencyUpdateOption, DiscoveredDependency};

pub mod internal;

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
}
