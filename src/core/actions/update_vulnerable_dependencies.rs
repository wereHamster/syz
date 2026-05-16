use crate::core::application::Application;
use anyhow::Result;
use tempfile::TempDir;
use tracing::instrument::WithSubscriber;

pub async fn run(app: &Application, project_id: String) -> Result<()> {
    let project = app.store().project(&project_id).await?;
    let patchers = app.patchers();

    let view = app.project_repository_view(&project.id).await?;
    let default_branch = view.get_default_branch().await?;
    let base_revision = view.get_revision(&default_branch).await?;
    let mutator = app.project_repository_mutator(&project.id).await?;
    let snapshot = view.snapshot(&base_revision);

    let pr_generator = app.audit_pull_request_generator();

    tokio::spawn(async move {
        let mut all_modifications = Vec::new();
        let mut combined_pr_body = String::new();
        let mut has_changes = false;

        for (ecosystem_name, patcher) in patchers {
            let temp_dir_result = TempDir::new();
            if let Err(e) = temp_dir_result {
                tracing::error!(
                    "Failed to create temporary directory for {}: {}",
                    ecosystem_name,
                    e
                );
                continue;
            }
            let temp_dir = temp_dir_result.unwrap();

            match patcher
                .update_vulnerable_dependencies(snapshot.as_ref(), temp_dir.path())
                .await
            {
                Ok(Some(result)) => {
                    if !result.modifications.is_empty() {
                        all_modifications.extend(result.modifications);
                        if has_changes {
                            combined_pr_body.push_str("\n\n---\n\n");
                        }

                        let body = match pr_generator
                            .generate_pull_request_body(&result.summary)
                            .await
                        {
                            Ok(b) => b,
                            Err(e) => {
                                tracing::error!("Failed to generate audit PR body: {}", e);
                                String::from("Failed to generate PR body.")
                            }
                        };
                        combined_pr_body.push_str(&body);
                        has_changes = true;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::error!(
                        "Failed to update vulnerable dependencies for {}: {}",
                        ecosystem_name,
                        e
                    );
                }
            }
        }

        let branch_name = "syz/audit";

        if !has_changes {
            tracing::info!("No fixable security vulnerabilities found.");
            let _ = mutator
                .close_pull_request(branch_name, &default_branch)
                .await;
            return;
        }

        let title = "Update dependencies to fix security vulnerabilities";

        let commit_res = mutator
            .commit_changes(&base_revision, branch_name, title, all_modifications)
            .await;

        match commit_res {
            Ok(Some(_sha)) => {
                match mutator
                    .create_pull_request(title, branch_name, &default_branch, &combined_pr_body)
                    .await
                {
                    Ok(url) => tracing::info!("Created vulnerable update PR: {}", url),
                    Err(e) => tracing::error!("Failed to create PR: {}", e),
                }
            }
            Ok(None) => {
                tracing::info!("Modifications resulted in no changes relative to base branch. Cleaning up empty PRs.");
                let _ = mutator
                    .close_pull_request(branch_name, &default_branch)
                    .await;
            }
            Err(e) => {
                tracing::error!("Failed to commit changes: {}", e);
            }
        }
    }.with_current_subscriber());

    Ok(())
}
