use crate::core::application::Application;
use anyhow::Result;
use tempfile::TempDir;

pub async fn run(app: &Application, project_id: String) -> Result<()> {
    let project = app.store().project(&project_id).await?;
    let patchers = app.patchers();

    let view = app.project_repository_view(&project.id).await?;
    let default_branch = view.get_default_branch().await?;
    let base_revision = view.get_revision(&default_branch).await?;
    let mutator = app.project_repository_mutator(&project.id).await?;
    let snapshot = view.snapshot(&base_revision);

    let pr_generator = app.transitive_pull_request_generator();

    tokio::spawn(async move {
        let mut all_modifications = Vec::new();
        let mut all_summaries = Vec::new();

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
                    if !result.modifications.is_empty() {
                        all_modifications.extend(result.modifications);

                        let body = match pr_generator
                            .generate_pull_request_body(&result.summary)
                            .await
                        {
                            Ok(b) => b,
                            Err(e) => {
                                tracing::error!("Failed to generate transitive PR body: {}", e);
                                String::from("Failed to generate PR body.")
                            }
                        };

                        all_summaries.push(format!("### {}\n\n{}", ecosystem_name, body));
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::error!(
                        "Failed to update transitive dependencies for {}: {}",
                        ecosystem_name,
                        e
                    );
                }
            }
        }

        let branch_name = "syz/transitive".to_string();

        if all_modifications.is_empty() {
            let _ = mutator
                .close_pull_request(&branch_name, &default_branch)
                .await;
            return;
        }

        let title = "Update transitive dependencies".to_string();
        let combined_summary = all_summaries.join("\n\n");

        let commit_res = mutator
            .commit_changes(&base_revision, &branch_name, &title, all_modifications)
            .await;

        match commit_res {
            Ok(Some(_sha)) => {
                match mutator
                    .create_pull_request(&title, &branch_name, &default_branch, &combined_summary)
                    .await
                {
                    Ok(url) => tracing::info!("Created transitive update PR: {}", url),
                    Err(e) => tracing::error!("Failed to create transitive PR: {}", e),
                }
            }
            Ok(None) => {
                tracing::info!("Modifications resulted in no changes relative to base branch. Cleaning up empty PRs.");
                let _ = mutator
                    .close_pull_request(&branch_name, &default_branch)
                    .await;
            }
            Err(e) => {
                tracing::error!("Failed to commit changes for transitive updates: {}", e);
            }
        }
    });

    Ok(())
}
