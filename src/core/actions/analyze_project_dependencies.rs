use anyhow::Result;
use futures::future;
use tracing::instrument::WithSubscriber;

use crate::core::{
    application::Application,
    database::pk,
    engine::{DependencyUpdateOption, DiscoveredDependency},
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

pub async fn run(app: &Application, project_id: String) -> Result<()> {
    let snapshot = app.project_repository_snapshot(&project_id).await?;
    let scanners = app.ecosystems();
    let handle = app.handle();

    tokio::spawn(
        async move {
            let futures = scanners.iter().map(|scanner| async {
                let mut discovered_dependencies = scanner
                    .discover_project_dependencies(snapshot.as_ref())
                    .await?;

                // We do not require that 'discover_project_dependencies' returns unique dependencies.
                // It is very likely that there are duplicates. That happens quite often in monorepos
                // where multiple packages have the same dependency. To not waste time querying
                // dependency update options, make the deps unique.
                discovered_dependencies.sort();
                discovered_dependencies.dedup();

                // For each of the discovered dependencies, see if it can be updated. This requires
                // contacting the ecosystem package registry, and lots of custom code to determine
                // what the version candidates are.
                let results =
                    future::try_join_all(discovered_dependencies.into_iter().map(|dep| async {
                        let dependency_update_options =
                            scanner.query_dependency_update_options(&dep).await?;

                        Ok::<_, anyhow::Error>(AnalyzedProjectDependency {
                            discovered_dependency: dep,
                            dependency_update_options,
                        })
                    }))
                    .await?;

                Ok::<_, anyhow::Error>(results)
            });

            match future::try_join_all(futures).await {
                Ok(results) => {
                    let analyzed_project_dependencies = results.into_iter().flatten().collect();
                    let payload =
                        crate::core::message::Payload::PersistAnalyzedProjectDependencies {
                            project_id: project_id.clone(),
                            scan_result: AnalyzedProjectDependencies {
                                analyzed_project_dependencies,
                            },
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
                    tracing::error!("Failed to analyze project {}: {:#}", project_id, e);
                }
            }
        }
        .with_current_subscriber(),
    );

    Ok(())
}
