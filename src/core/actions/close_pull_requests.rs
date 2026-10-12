use super::process_bump::generate_branch_name;
use super::DefaultBranch;
use crate::core::application::Application;
use crate::core::engine::repository::ProjectRepositoryMutator;
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

            close_unused(
                &store,
                &project_id,
                mutator.as_ref(),
                &default_branch,
                branch_names,
            )
            .await;
        }
        .instrument(span)
        .with_current_subscriber(),
    );

    Ok(())
}

/// Close the pull request of each branch that none of the project's bumps uses.
async fn close_unused(
    store: &Store,
    project_id: &str,
    mutator: &dyn ProjectRepositoryMutator,
    default_branch: &str,
    branch_names: Vec<String>,
) {
    for branch_name in branch_names {
        // Checked right before closing rather than up front: resolving the default
        // branch is slow, and a rescan may have brought the bump back meanwhile.
        match live_branches(store, project_id).await {
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
            .close_pull_request(&branch_name, default_branch)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::database::Database;
    use crate::core::engine::repository::FileModification;
    use crate::core::platform::Platform;
    use async_trait::async_trait;
    use std::sync::Mutex;
    use turso::params;

    #[derive(Default)]
    struct RecordingMutator {
        closed: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl ProjectRepositoryMutator for RecordingMutator {
        async fn commit_changes(
            &self,
            _base_revision: &str,
            _branch_name: &str,
            _commit_message: &str,
            _modifications: Vec<FileModification>,
        ) -> Result<Option<String>> {
            unimplemented!()
        }

        async fn create_pull_request(
            &self,
            _title: &str,
            _branch_name: &str,
            _base_branch: &str,
            _body: &str,
        ) -> Result<String> {
            unimplemented!()
        }

        async fn close_pull_request(&self, branch_name: &str, _base_branch: &str) -> Result<()> {
            self.closed.lock().unwrap().push(branch_name.to_string());
            Ok(())
        }
    }

    #[tokio::test]
    async fn closes_obsolete_branches_but_keeps_one_a_surviving_bump_collides_with() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(Database::open(dir.path()).await.unwrap());
        let project = store
            .add_project(Platform::GitHub, "owner/repo".to_string())
            .await
            .unwrap();

        // "Foo@Bar" and "foo---bar" map to the same branch; only the latter survives.
        assert_eq!(
            generate_branch_name("Foo@Bar", false),
            generate_branch_name("foo---bar", false)
        );
        let conn = store.database().conn().unwrap();
        conn.execute(
            "INSERT INTO bump (id, project_id, name, major, approved) VALUES (?, ?, ?, 0, 0)",
            params!["live", project.id.as_str(), "foo---bar"],
        )
        .await
        .unwrap();

        let mutator = RecordingMutator::default();
        close_unused(
            &store,
            &project.id,
            &mutator,
            "main",
            vec![
                generate_branch_name("Foo@Bar", false),
                generate_branch_name("serde", false),
            ],
        )
        .await;

        assert_eq!(
            *mutator.closed.lock().unwrap(),
            vec![generate_branch_name("serde", false)]
        );
    }
}
