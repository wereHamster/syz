use super::DefaultBranch;
use crate::core::application::Application;
use anyhow::Result;
use tracing::instrument::WithSubscriber;
use tracing::Instrument;

/// Close the pull requests opened from the given branches of the project, and delete the
/// branches. Used for bumps that no longer exist, so nothing is left to track the pull
/// requests.
pub async fn run(app: &Application, project_id: String, branch_names: Vec<String>) -> Result<()> {
    let repository = app.project_platform_repository(&project_id).await?;

    let span = tracing::Span::current();

    tokio::spawn(
        async move {
            let DefaultBranch {
                name: default_branch,
                mutator,
                ..
            } = match DefaultBranch::resolve(repository.as_ref()).await {
                Ok(branch) => branch,
                Err(e) => {
                    tracing::error!(
                        "Failed to resolve the default branch of {}: {:#}",
                        project_id,
                        e
                    );
                    return;
                }
            };

            for branch_name in branch_names {
                match mutator
                    .close_pull_request(&branch_name, &default_branch)
                    .await
                {
                    Ok(()) => tracing::info!("Closed the pull request of {}", branch_name),
                    Err(e) => tracing::error!(
                        "Failed to close the pull request of {}: {:#}",
                        branch_name,
                        e
                    ),
                }
            }
        }
        .instrument(span)
        .with_current_subscriber(),
    );

    Ok(())
}
