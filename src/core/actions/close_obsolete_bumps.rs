use super::process_bump::generate_branch_name;
use super::DefaultBranch;
use crate::core::application::Application;
use crate::core::database::Bump;
use anyhow::Result;
use tracing::instrument::WithSubscriber;
use tracing::Instrument;

/// Close the pull requests of bumps that a rescan found to be obsolete and removed.
pub async fn run(app: &Application, project_id: String, bumps: Vec<Bump>) -> Result<()> {
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

            for bump in bumps {
                let branch_name = generate_branch_name(&bump.name, bump.major);
                match mutator
                    .close_pull_request(&branch_name, &default_branch)
                    .await
                {
                    Ok(()) => tracing::info!(
                        "Closed the pull request of obsolete bump {} ({})",
                        bump.name,
                        bump.url.as_deref().unwrap_or_default()
                    ),
                    Err(e) => tracing::error!(
                        "Failed to close the pull request of obsolete bump {}: {:#}",
                        bump.name,
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
