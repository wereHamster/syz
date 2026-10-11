use super::{report_sweep, DefaultBranch};
use crate::core::application::Application;
use crate::core::database::SweepKind;
use anyhow::Result;
use tempfile::TempDir;
use tracing::instrument::WithSubscriber;
use tracing::Instrument;

pub async fn run(app: &Application, project_id: String) -> Result<()> {
    let repository = app.project_platform_repository(&project_id).await?;
    let patchers = app.patchers();

    let pr_generator = app.audit_pull_request_generator();
    let handle = app.handle();
    let audit_permits = app.audit_permits();

    let span = tracing::Span::current();

    tokio::spawn(async move {
        // Held until the audit is done. Waiting for it here, in the worker, keeps the
        // application loop free to process messages in the meantime.
        let Ok(_permit) = audit_permits.acquire().await else {
            return;
        };

        let DefaultBranch {
            name: default_branch,
            revision: base_revision,
            snapshot,
            mutator,
        } = match DefaultBranch::resolve(repository.as_ref()).await {
            Ok(branch) => branch,
            Err(e) => {
                tracing::error!("Failed to resolve the default branch of {}: {:#}", project_id, e);
                return;
            }
        };

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
                    // Without a trustworthy result for this ecosystem, neither opening nor
                    // closing the audit pull request would be correct.
                    tracing::error!(
                        "Failed to update vulnerable dependencies for {}, aborting the audit: {}",
                        ecosystem_name,
                        e
                    );
                    return;
                }
            }
        }

        let kind = SweepKind::Audit;
        let branch_name = kind.branch_name();

        // Failing to close a pull request isn't tracked: the record is dropped regardless.
        if !has_changes {
            tracing::info!("No fixable security vulnerabilities found.");
            let _ = mutator
                .close_pull_request(branch_name, &default_branch)
                .await;
            report_sweep(&handle, &project_id, kind, None).await;
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
                    Ok(url) => {
                        tracing::info!("Created vulnerable update PR: {}", url);
                        report_sweep(&handle, &project_id, kind, Some(url)).await;
                    }
                    Err(e) => tracing::error!("Failed to create PR: {}", e),
                }
            }
            Ok(None) => {
                tracing::info!("Modifications resulted in no changes relative to base branch. Cleaning up empty PRs.");
                let _ = mutator
                    .close_pull_request(branch_name, &default_branch)
                    .await;
                report_sweep(&handle, &project_id, kind, None).await;
            }
            Err(e) => {
                tracing::error!("Failed to commit changes: {}", e);
            }
        }
    }
    .instrument(span)
    .with_current_subscriber());

    Ok(())
}
