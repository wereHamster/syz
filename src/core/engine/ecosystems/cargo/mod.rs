use anyhow::Result;
use async_trait::async_trait;

use crate::core::clients;
use crate::core::engine::ecosystems::{Registry, Scanner};
use crate::core::engine::repository::ProjectRepositorySnapshot;
use crate::core::engine::{DependencyUpdateOption, DiscoveredDependency};

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
}
