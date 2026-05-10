use anyhow::Result;
use async_trait::async_trait;

use crate::core::engine::{
    repository::ProjectRepositorySnapshot, DependencyUpdateOption, DiscoveredDependency,
};

pub mod cargo;
pub mod github_actions;
pub mod npm;

#[async_trait]
pub trait Ecosystem: Send + Sync {
    /// Scans a project repository to discover dependencies.
    async fn discover_project_dependencies(
        &self,
        repo: &dyn ProjectRepositorySnapshot,
    ) -> Result<Vec<DiscoveredDependency>>;

    /// Computes options how the given dependency can be updated.
    ///
    /// The result includes information about different versions (minor and
    /// major) that we can update the dependency to.
    async fn query_dependency_update_options(
        &self,
        dependency: &DiscoveredDependency,
    ) -> Result<DependencyUpdateOption>;
}
