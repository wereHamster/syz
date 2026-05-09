use anyhow::Result;

use crate::core::application::Application;

pub async fn run(app: &Application, project_id: String) -> Result<()> {
    tracing::info!("AnalyzeProjectDependencies {}", project_id);

    let query = app.query();

    let project = query.project(project_id).await?;

    tracing::info!("Got project {}:{}", project.platform, project.repository);

    Ok(())
}
