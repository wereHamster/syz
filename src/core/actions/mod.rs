//! The jobs that the dispatcher runs. They talk to the network, package managers and git,
//! may read the store, and report back to the main loop with an outcome message.

pub mod analyze_project_dependencies;
pub mod close_pull_requests;
pub mod process_bump;
pub mod prune_branches;
pub mod update_transitive_dependencies;
pub mod update_vulnerable_dependencies;

use crate::core::engine::repository::{
    FileModification, ProjectRepositoryMutator, ProjectRepositorySnapshot,
};
use crate::core::job::PublishOutcome;
use crate::core::platform::ProjectPlatformRepository;
use anyhow::{Context, Result};
use std::path::Path;
use tempfile::TempDir;

/// Creates a scratch directory inside `work_dir`, which is created if it doesn't exist yet.
fn create_temp_dir(work_dir: &Path) -> Result<TempDir> {
    std::fs::create_dir_all(work_dir)
        .with_context(|| format!("Failed to create work directory {}", work_dir.display()))?;
    TempDir::new_in(work_dir)
        .with_context(|| format!("Failed to create a directory in {}", work_dir.display()))
}

/// A project's default branch resolved to a revision, with what is needed to read and
/// write against it.
///
/// Resolving it talks to the hosting platform, so only call it from a job.
struct DefaultBranch {
    name: String,
    revision: String,
    snapshot: Box<dyn ProjectRepositorySnapshot>,
    mutator: Box<dyn ProjectRepositoryMutator>,
}

/// A change to propose in a pull request.
struct Change {
    title: String,
    body: String,
    modifications: Vec<FileModification>,
}

impl DefaultBranch {
    async fn resolve(repository: &dyn ProjectPlatformRepository) -> Result<Self> {
        let view = repository.view().await?;
        let name = view.get_default_branch().await?;
        let revision = view.get_revision(&name).await?;
        let snapshot = view.snapshot(&revision);
        let mutator = repository.mutator().await?;

        Ok(Self {
            name,
            revision,
            snapshot,
            mutator,
        })
    }

    /// Propose the change in a pull request from `branch_name`, which is reset onto this
    /// revision. Without a change, or if the default branch already has it, the branch's pull
    /// request (if any) is closed instead.
    async fn publish(&self, branch_name: &str, change: Option<Change>) -> Result<PublishOutcome> {
        if let Some(Change {
            title,
            body,
            modifications,
        }) = change.filter(|change| !change.modifications.is_empty())
        {
            let commit = self
                .mutator
                .commit_changes(&self.revision, branch_name, &title, modifications)
                .await
                .with_context(|| format!("Failed to commit to {}", branch_name))?;

            if commit.is_some() {
                let url = self
                    .mutator
                    .create_pull_request(&title, branch_name, &self.name, &body)
                    .await
                    .with_context(|| {
                        format!("Failed to create the pull request of {}", branch_name)
                    })?;

                tracing::info!("Published {} as {}", branch_name, url);
                return Ok(PublishOutcome::Opened { url });
            }

            tracing::info!("{} has no changes relative to {}", branch_name, self.name);
        }

        self.mutator
            .close_pull_request(branch_name, &self.name)
            .await
            .with_context(|| format!("Failed to close the pull request of {}", branch_name))?;

        tracing::info!(
            "Nothing to publish on {}, its pull request is closed",
            branch_name
        );
        Ok(PublishOutcome::Closed)
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::core::engine::repository::{FileModification, ProjectRepositoryMutator};
    use anyhow::Result;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    /// Records the calls made to it, in a log that its clones share. `commit_changes` reports
    /// a commit unless `unchanged`, and closing the branches in `fail_closing` fails.
    #[derive(Default, Clone)]
    pub struct RecordingMutator {
        pub unchanged: bool,
        pub fail_closing: Vec<String>,
        pub calls: Arc<Mutex<Vec<String>>>,
    }

    impl RecordingMutator {
        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ProjectRepositoryMutator for RecordingMutator {
        async fn commit_changes(
            &self,
            base_revision: &str,
            branch_name: &str,
            _commit_message: &str,
            _modifications: Vec<FileModification>,
        ) -> Result<Option<String>> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("commit {} onto {}", branch_name, base_revision));
            Ok((!self.unchanged).then(|| "sha".to_string()))
        }

        async fn create_pull_request(
            &self,
            _title: &str,
            branch_name: &str,
            base_branch: &str,
            _body: &str,
        ) -> Result<String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("open {} against {}", branch_name, base_branch));
            Ok(format!(
                "https://github.com/owner/repo/pull/{}",
                branch_name
            ))
        }

        async fn close_pull_request(&self, branch_name: &str, base_branch: &str) -> Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("close {} against {}", branch_name, base_branch));
            if self.fail_closing.iter().any(|branch| branch == branch_name) {
                anyhow::bail!("refused");
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::RecordingMutator;
    use super::*;
    use crate::core::engine::repository::test_support::MockSnapshot;
    use crate::core::engine::repository::FileState;

    fn default_branch(mutator: RecordingMutator) -> (DefaultBranch, RecordingMutator) {
        let branch = DefaultBranch {
            name: "main".to_string(),
            revision: "rev".to_string(),
            snapshot: Box::new(MockSnapshot::empty()),
            mutator: Box::new(mutator.clone()),
        };
        (branch, mutator)
    }

    fn change(modifications: usize) -> Change {
        Change {
            title: "Update".to_string(),
            body: "Body".to_string(),
            modifications: (0..modifications)
                .map(|i| FileModification {
                    path: format!("file{}", i),
                    state: FileState::Write(String::new()),
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn publishes_a_change_as_a_pull_request() {
        let (branch, mutator) = default_branch(RecordingMutator::default());

        let outcome = branch.publish("syz/x", Some(change(1))).await.unwrap();

        assert_eq!(
            outcome,
            PublishOutcome::Opened {
                url: "https://github.com/owner/repo/pull/syz/x".to_string()
            }
        );
        assert_eq!(
            mutator.calls(),
            vec!["commit syz/x onto rev", "open syz/x against main"]
        );
    }

    #[tokio::test]
    async fn closes_the_pull_request_without_a_change() {
        let (branch, mutator) = default_branch(RecordingMutator::default());

        let outcome = branch.publish("syz/x", None).await.unwrap();

        assert_eq!(outcome, PublishOutcome::Closed);
        assert_eq!(mutator.calls(), vec!["close syz/x against main"]);
    }

    #[tokio::test]
    async fn closes_the_pull_request_of_an_empty_change() {
        let (branch, mutator) = default_branch(RecordingMutator::default());

        let outcome = branch.publish("syz/x", Some(change(0))).await.unwrap();

        assert_eq!(outcome, PublishOutcome::Closed);
        assert_eq!(mutator.calls(), vec!["close syz/x against main"]);
    }

    #[tokio::test]
    async fn closes_the_pull_request_when_the_default_branch_has_the_change() {
        let (branch, mutator) = default_branch(RecordingMutator {
            unchanged: true,
            ..RecordingMutator::default()
        });

        let outcome = branch.publish("syz/x", Some(change(1))).await.unwrap();

        assert_eq!(outcome, PublishOutcome::Closed);
        assert_eq!(
            mutator.calls(),
            vec!["commit syz/x onto rev", "close syz/x against main"]
        );
    }

    #[tokio::test]
    async fn fails_when_the_pull_request_cannot_be_closed() {
        let (branch, _) = default_branch(RecordingMutator {
            fail_closing: vec!["syz/x".to_string()],
            ..RecordingMutator::default()
        });

        let err = branch.publish("syz/x", None).await.unwrap_err();

        assert!(format!("{:#}", err).contains("syz/x"), "{err:#}");
    }
}
