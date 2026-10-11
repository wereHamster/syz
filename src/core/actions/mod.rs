pub mod analyze_project_dependencies;
pub mod process_bump;
pub mod update_transitive_dependencies;
pub mod update_vulnerable_dependencies;

use crate::core::application::Handle;
use crate::core::database::{pk, SweepKind};
use crate::core::engine::repository::{ProjectRepositoryMutator, ProjectRepositorySnapshot};
use crate::core::message::Payload;
use crate::core::platform::ProjectPlatformRepository;
use anyhow::Result;

/// A project's default branch resolved to a revision, with what is needed to read and
/// write against it.
///
/// Resolving it talks to the hosting platform, so only call it from a background worker.
struct DefaultBranch {
    name: String,
    revision: String,
    snapshot: Box<dyn ProjectRepositorySnapshot>,
    mutator: Box<dyn ProjectRepositoryMutator>,
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
}

/// Tell the main loop which pull request (if any) is open for the project's sweep of
/// the given kind.
async fn report_sweep(
    handle: &Handle,
    project_id: &str,
    kind: SweepKind,
    pull_request_url: Option<String>,
) {
    let payload = Payload::PersistSweep {
        project_id: project_id.to_string(),
        kind,
        pull_request_url,
    };
    if let Err(e) = handle.send(pk(), payload).await {
        tracing::error!("Failed to send PersistSweep: {}", e);
    }
}
