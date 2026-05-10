use crate::core::application::Application;
use anyhow::Result;
use tempfile::TempDir;

pub async fn run(app: &Application, project_id: String) -> Result<()> {
    let project = app.query().project(&project_id).await?;
    let patchers = app.patchers();

    let view = app.project_repository_view(&project.id).await?;
    let default_branch = view.get_default_branch().await?;
    let base_revision = view.get_revision(&default_branch).await?;
    let mutator = app.project_repository_mutator(&project.id).await?;
    let snapshot = view.snapshot(&base_revision);

    tokio::spawn(async move {
        for (ecosystem_name, patcher) in patchers {
            let temp_dir_result = TempDir::new();
            if let Err(e) = temp_dir_result {
                tracing::error!("Failed to create temporary directory: {}", e);
                continue;
            }
            let temp_dir = temp_dir_result.unwrap();

            match patcher
                .update_transitive_dependencies(snapshot.as_ref(), temp_dir.path())
                .await
            {
                Ok(Some(result)) => {
                    if result.modifications.is_empty() {
                        continue;
                    }

                    let branch_name = format!("syz/transitive-{}", ecosystem_name);
                    let title = format!("Update transitive dependencies ({})", ecosystem_name);

                    let commit_res = mutator
                        .commit_changes(&base_revision, &branch_name, &title, result.modifications)
                        .await;

                    match commit_res {
                        Ok(Some(_sha)) => {
                            match mutator
                                .create_pull_request(
                                    &title,
                                    &branch_name,
                                    &default_branch,
                                    &result.summary,
                                )
                                .await
                            {
                                Ok(url) => tracing::info!("Created transitive update PR: {}", url),
                                Err(e) => tracing::error!(
                                    "Failed to create PR for {}: {}",
                                    ecosystem_name,
                                    e
                                ),
                            }
                        }
                        Ok(None) => {
                            tracing::info!("Modifications resulted in no changes relative to base branch. Cleaning up empty PRs.");
                            let _ = mutator
                                .close_pull_request(&branch_name, &default_branch)
                                .await;
                        }
                        Err(e) => {
                            tracing::error!(
                                "Failed to commit changes for {}: {}",
                                ecosystem_name,
                                e
                            );
                        }
                    }
                }
                Ok(None) => {
                    // Check if we need to close an empty PR if it existed
                    let branch_name = format!("syz/transitive-{}", ecosystem_name);
                    let _ = mutator
                        .close_pull_request(&branch_name, &default_branch)
                        .await;
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to update transitive dependencies for {}: {}",
                        ecosystem_name,
                        e
                    );
                }
            }
        }
    });

    Ok(())
}
