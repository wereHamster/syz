use anyhow::Result;

use crate::core::application::Application;

pub async fn run(app: &Application, project_id: String) -> Result<()> {
    tracing::info!("AnalyzeProjectDependencies {}", project_id);

    let query = app.query();

    let project = query.project(project_id).await?;

    tracing::info!("Got project {}:{}", project.platform, project.repository);

    // project.platform is either "github" or "tangled".
    //
    // Need to construct a ProjectRepositoryView, get the revision of the default branch,
    // then construct a ProjectRepositorySnapshot.
    //
    // Then initialize ecosystem scanners and apply each to the snapshot to discover
    // project dependencies. Run the discovery in parallel.
    //
    // Once all project dependencies are known, query the ecosystem for dependency update
    // options.
    //
    // Finally write the result into the database.

    Ok(())
}
