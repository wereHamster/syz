use anyhow::Result;

use crate::core::application::Application;

pub async fn run(app: &Application, project_id: String) -> Result<()> {
    tracing::info!("AnalyzeProjectDependencies {}", project_id);

    let view = app.project_repository_view(&project_id).await?;
    let default_branch = view.get_default_branch().await?;
    let revision = view.get_revision(default_branch.as_str()).await?;
    let snapshot = view.snapshot(revision.as_str());

    let files = snapshot.list_files().await?;
    for file in files {
        tracing::info!("file: {}", file);
    }

    // Initialize ecosystem scanners and apply each to the snapshot to discover
    // project dependencies. Run the discovery in parallel.
    //
    // Once all project dependencies are known, query the ecosystem for dependency update
    // options.
    //
    // Finally write the result into the database.

    Ok(())
}
