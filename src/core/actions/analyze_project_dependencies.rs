use anyhow::Result;
use futures::future;
use tracing::instrument::WithSubscriber;
use tracing::Instrument;

use crate::{
    core::{
        application::Application,
        database::pk,
        engine::{
            repository::{ProjectRepositorySnapshot, ProjectRepositoryView},
            DependencyUpdateOption, DiscoveredDependency,
        },
    },
    tui::views,
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

pub async fn run(app: &Application, project_id: String, trigger_bumps: bool) -> Result<()> {
    let repository = app.project_platform_repository(&project_id).await?;

    let scanners = app.scanners();
    let registry_router = app.registry_router();
    let handle = app.handle();

    let span = tracing::Span::current();

    tokio::spawn(
        async move {
            let snapshot = match repository.view().await {
                Ok(view) => match snapshot_at_default_branch(view.as_ref()).await {
                    Ok(snapshot) => snapshot,
                    Err(e) => {
                        tracing::error!("Failed to get project repository snapshot: {}", e);
                        return;
                    }
                },
                Err(e) => {
                    tracing::error!("Failed to get project repository view: {}", e);
                    return;
                }
            };

            // --- PHASE 1: SCAN ---
            // Run all scanners concurrently over the snapshot
            let scan_futures = scanners.iter().map(|scanner| {
                scanner.discover_project_dependencies(snapshot.as_ref())
            });

            match future::try_join_all(scan_futures).await {
                Ok(results) => {
                    let mut discovered_dependencies: Vec<DiscoveredDependency> =
                        results.into_iter().flatten().collect();

                    // Global dedupe across all scanners
                    discovered_dependencies.sort();
                    discovered_dependencies.dedup();

                    // --- PHASE 2: REGISTRY ---
                    // Route queries dynamically based on the dependency's PURL
                    let registry_futures = discovered_dependencies.into_iter().map(|dep| async {
                        let dependency_update_options = registry_router
                            .query_dependency_update_options(&dep)
                            .await?;

                        Ok::<_, anyhow::Error>(AnalyzedProjectDependency {
                            discovered_dependency: dep,
                            dependency_update_options,
                        })
                    });

                    match future::try_join_all(registry_futures).await {
                        Ok(analyzed_project_dependencies) => {
                            let payload =
                                crate::core::message::Payload::PersistAnalyzedProjectDependencies {
                                    project_id: project_id.clone(),
                                    scan_result: AnalyzedProjectDependencies {
                                        analyzed_project_dependencies,
                                    },
                                    trigger_bumps,
                                };

                            // We send it to the application mailbox using an ad-hoc message_id
                            if let Err(e) = handle.send(pk(), payload).await {
                                tracing::error!(
                                    "Failed to send PersistAnalyzedProjectDependencies message: {:#}",
                                    e
                                );
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to query registries for project {}: {:#}", project_id, e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to scan project {}: {:#}", project_id, e);
                }
            }
        }
        .instrument(span)
        .with_current_subscriber()
    );

    Ok(())
}

async fn snapshot_at_default_branch(
    view: &dyn ProjectRepositoryView,
) -> Result<Box<dyn ProjectRepositorySnapshot>> {
    let default_branch = view.get_default_branch().await?;
    let revision = view.get_revision(default_branch.as_str()).await?;
    Ok(view.snapshot(revision.as_str()))
}
