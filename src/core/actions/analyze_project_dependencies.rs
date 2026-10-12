use anyhow::{Context, Result};
use futures::future;

use crate::core::{
    engine::{
        repository::{ProjectRepositorySnapshot, ProjectRepositoryView},
        DependencyUpdateOption, DiscoveredDependency,
    },
    job::LaneKey,
    message::Payload,
    services::Services,
};

#[derive(Clone)]
pub struct AnalyzedProjectDependency {
    pub discovered_dependency: DiscoveredDependency,
    pub dependency_update_options: DependencyUpdateOption,
}

impl AnalyzedProjectDependency {
    pub fn group_name(&self) -> String {
        crate::core::engine::groups::get_group(&self.dependency_update_options.package_info)
            .unwrap_or(self.discovered_dependency.purl.package_name())
    }
}

#[derive(Clone)]
pub struct AnalyzedProjectDependencies {
    pub analyzed_project_dependencies: Vec<AnalyzedProjectDependency>,
}

/// Scan the project at its default branch, and look up which of its dependencies are
/// outdated.
pub async fn run(
    services: &Services,
    lane: LaneKey,
    project_id: String,
) -> Result<Option<Payload>> {
    let repository = services.repository(&lane)?;
    let view = repository.view().await?;
    let snapshot = snapshot_at_default_branch(view.as_ref()).await?;

    // --- PHASE 1: SCAN ---
    // Run all scanners concurrently over the snapshot
    let scanners = services.scanners();
    let scan_futures = scanners
        .iter()
        .map(|scanner| scanner.discover_project_dependencies(snapshot.as_ref()));

    let mut discovered_dependencies: Vec<DiscoveredDependency> = future::try_join_all(scan_futures)
        .await
        .context("Failed to scan the project")?
        .into_iter()
        .flatten()
        .collect();

    // Global dedupe across all scanners
    discovered_dependencies.sort();
    discovered_dependencies.dedup();

    // --- PHASE 2: REGISTRY ---
    // Route queries dynamically based on the dependency's PURL
    let registry_router = services.registry_router();
    let registry_futures = discovered_dependencies.into_iter().map(|dep| async {
        let dependency_update_options = registry_router
            .query_dependency_update_options(&dep)
            .await?;

        Ok::<_, anyhow::Error>(AnalyzedProjectDependency {
            discovered_dependency: dep,
            dependency_update_options,
        })
    });

    let analyzed_project_dependencies = future::try_join_all(registry_futures)
        .await
        .context("Failed to query registries")?;

    Ok(Some(Payload::PersistAnalyzedProjectDependencies {
        project_id,
        scan_result: AnalyzedProjectDependencies {
            analyzed_project_dependencies,
        },
    }))
}

async fn snapshot_at_default_branch(
    view: &dyn ProjectRepositoryView,
) -> Result<Box<dyn ProjectRepositorySnapshot>> {
    let default_branch = view.get_default_branch().await?;
    let revision = view.get_revision(default_branch.as_str()).await?;
    Ok(view.snapshot(revision.as_str()))
}
