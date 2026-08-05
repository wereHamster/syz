use anyhow::Result;
use async_trait::async_trait;

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

pub struct NixFlakeRegistry;

#[async_trait]
impl Registry for NixFlakeRegistry {
    async fn query_dependency_update_options(
        &self,
        dependency: &DiscoveredDependency,
    ) -> Result<DependencyUpdateOption> {
        internal::query_dependency_update_options::run(dependency).await
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
