use anyhow::Result;
use futures::future;

use crate::core::{
    application::Application,
    engine::{DependencyUpdateOption, DiscoveredDependency},
};

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

pub struct AnalyzedProjectDependencies {
    pub analyzed_project_pependencies: Vec<AnalyzedProjectDependency>,
}

pub async fn run(app: &Application, project_id: String) -> Result<()> {
    let snapshot = app.project_repository_snapshot(&project_id).await?;

    let scanners = app.ecosystems();
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
        let results = future::try_join_all(discovered_dependencies.into_iter().map(|dep| async {
            let dependency_update_options = scanner.query_dependency_update_options(&dep).await?;

            Ok::<_, anyhow::Error>(AnalyzedProjectDependency {
                discovered_dependency: dep,
                dependency_update_options,
            })
        }))
        .await?;

        Ok::<_, anyhow::Error>(results)
    });

    let results = future::try_join_all(futures).await?;
    let analyzed_project_pependencies = results.into_iter().flatten().collect();

    app.persist_analyzed_project_pependencies(
        &project_id,
        AnalyzedProjectDependencies {
            analyzed_project_pependencies,
        },
    )
    .await?;

    Ok(())
}
