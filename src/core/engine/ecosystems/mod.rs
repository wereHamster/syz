use anyhow::Result;
use async_trait::async_trait;

use crate::core::engine::{repository::ProjectRepositorySnapshot, DiscoveredDependency};

pub mod npm;

#[async_trait]
pub trait Ecosystem: Send + Sync {
    /// Scans a project repository to discover dependencies.
    async fn discover_project_dependencies(
        &self,
        repo: &dyn ProjectRepositorySnapshot,
    ) -> Result<Vec<DiscoveredDependency>>;
}
