use super::process_bump::generate_branch_name;
use super::DefaultBranch;
use crate::core::application::Application;
use crate::core::store::Store;
use anyhow::Result;
use std::collections::HashSet;
use tracing::instrument::WithSubscriber;
use tracing::Instrument;

/// Close the pull requests opened from the given branches of the project, and delete the
/// branches. Used for bumps that no longer exist, so nothing is left to track the pull
/// requests.
pub async fn run(app: &Application, project_id: String, branch_names: Vec<String>) -> Result<()> {
    let repository = app.project_platform_repository(&project_id).await?;
    let store = app.store().clone();

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
                // Checked right before closing rather than up front: resolving the default
                // branch is slow, and a rescan may have brought the bump back meanwhile.
                match live_branches(&store, &project_id).await {
                    Ok(live) if live.contains(&branch_name) => {
                        tracing::info!(
                            "Keeping the pull request of {}, a bump uses it",
                            branch_name
                        );
                        continue;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::error!(
                            "Failed to look up the bumps of {}, keeping {}: {:#}",
                            project_id,
                            branch_name,
                            e
                        );
                        continue;
                    }
                }

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

/// The branches of the project's existing bumps. Different bump names can map to the same
/// branch, and so share a pull request.
async fn live_branches(store: &Store, project_id: &str) -> Result<HashSet<String>> {
    Ok(store
        .list_bumps()
        .await?
        .into_iter()
        .filter(|bump| bump.project_id == project_id)
        .map(|bump| generate_branch_name(&bump.name, bump.major))
        .collect())
}
