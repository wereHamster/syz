use anyhow::Result;
use futures::future;

use crate::core::application::Application;

pub async fn run(app: &Application, project_id: String) -> Result<()> {
    tracing::info!("AnalyzeProjectDependencies {}", project_id);

    let view = app.project_repository_view(&project_id).await?;
    let default_branch = view.get_default_branch().await?;
    let revision = view.get_revision(default_branch.as_str()).await?;
    let snapshot = view.snapshot(revision.as_str());

    let scanners = app.ecosystems();
    let futures = scanners
        .iter()
        .map(|scanner| scanner.discover_project_dependencies(snapshot.as_ref()));

    let results = future::try_join_all(futures).await?;
    let deps: Vec<_> = results.into_iter().flatten().collect();

    for dep in deps {
        tracing::info!("Dep {}", dep.purl);
    }

    // Once all project dependencies are known, query the ecosystem for dependency update
    // options.
    //
    // Finally write the result into the database.

    Ok(())
}
