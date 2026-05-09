use anyhow::Result;

use crate::core::application::Application;
use crate::core::clients::github::GitHub;
use crate::core::engine::repository::ProjectRepositoryView;

pub async fn run(app: &Application, project_id: String) -> Result<()> {
    tracing::info!("AnalyzeProjectDependencies {}", project_id);

    let query = app.query();

    let project = query.project(project_id).await?;

    tracing::info!("Got project {}:{}", project.platform, project.repository);

    match project.platform.as_str() {
        "github" => {
            let github = GitHub::new().await?;

            let parts: Vec<&str> = project.repository.split('/').collect();
            if parts.len() != 2 {
                anyhow::bail!("Repository must be in the format owner/repo");
            }
            let owner = parts[0].to_string();
            let repo = parts[1].to_string();

            let project_repository_view = github.project_repository_view(owner, repo).await?;
            let default_branch = project_repository_view.get_default_branch().await?;
            let revision = project_repository_view
                .get_revision(default_branch.as_str())
                .await?;
            let snapshot = project_repository_view.snapshot(revision.as_str());

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
        }

        _ => {
            tracing::warn!("Unsupported project platform: {}", project.platform);
        }
    }

    Ok(())
}
