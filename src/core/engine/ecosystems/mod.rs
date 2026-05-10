use anyhow::Result;
use async_trait::async_trait;

use crate::core::engine::{
    repository::ProjectRepositorySnapshot, DependencyUpdateOption, DiscoveredDependency, UpdateTarget,
};

pub mod cargo;
pub mod github_actions;
pub mod npm;
pub mod registry_router;

#[async_trait]
pub trait Scanner: Send + Sync {
    /// Scans a project repository to discover dependencies.
    async fn discover_project_dependencies(
        &self,
        repo: &dyn ProjectRepositorySnapshot,
    ) -> Result<Vec<DiscoveredDependency>>;
}

#[async_trait]
pub trait Registry: Send + Sync {
    /// Computes options how the given dependency can be updated.
    ///
    /// The result includes information about different versions (minor and
    /// major) that we can update the dependency to.
    async fn query_dependency_update_options(
        &self,
        dependency: &DiscoveredDependency,
    ) -> Result<DependencyUpdateOption>;
}

#[async_trait]
pub trait Patcher: Send + Sync {
    /// Computes the new version requirement string if an update is needed.
    /// Returns `Some(new_requirement)` if the dependency should be updated,
    /// or `None` if it is already satisfied / equivalent.
    fn updated_requirement(&self, old_req: &str, target_version: &str) -> Option<String> {
        let new_req = target_version.to_string();
        if old_req != new_req {
            Some(new_req)
        } else {
            None
        }
    }

    /// Materializes necessary files from `snapshot` into `temp_dir`, runs ecosystem-specific 
    /// update tools (e.g., `cargo update`, `npm install`), and computes the differences.
    async fn apply_updates(
        &self,
        snapshot: &dyn ProjectRepositorySnapshot,
        temp_dir: &std::path::Path,
        targets: &[UpdateTarget],
    ) -> Result<Vec<crate::core::engine::repository::FileModification>>;
}
