use super::process_bump::generate_branch_name;
use super::DefaultBranch;
use crate::core::application::Application;
use anyhow::Result;
use std::collections::HashSet;
use tracing::instrument::WithSubscriber;
use tracing::Instrument;

/// Close the pull requests opened from the given branches of the project, and delete the
/// branches. Used for bumps that no longer exist, so nothing is left to track the pull
/// requests.
pub async fn run(app: &Application, project_id: String, branch_names: Vec<String>) -> Result<()> {
    // Different bump names can map to the same branch (and so share a pull request), so
    // leave alone any branch that a surviving bump still uses.
    let live_branches: HashSet<String> = app
        .store()
        .list_bumps()
        .await?
        .into_iter()
        .filter(|bump| bump.project_id == project_id)
        .map(|bump| generate_branch_name(&bump.name, bump.major))
        .collect();
    let branch_names: Vec<String> = branch_names
        .into_iter()
        .filter(|branch_name| !live_branches.contains(branch_name))
        .collect();
    if branch_names.is_empty() {
        return Ok(());
    }

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
