use anyhow::Result;
use async_trait::async_trait;

use crate::core::engine::ecosystems::Ecosystem;
use crate::core::engine::repository::ProjectRepositorySnapshot;
use crate::core::engine::DiscoveredDependency;

pub mod internal;

pub struct Npm {}

impl Npm {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl Ecosystem for Npm {
    async fn discover_project_dependencies(
        &self,
        repo: &dyn ProjectRepositorySnapshot,
    ) -> Result<Vec<DiscoveredDependency>> {
        internal::discover_project_dependencies::run(repo).await
    }
}
