pub mod analyze_project_dependencies;
pub mod process_bump;
pub mod update_transitive_dependencies;
pub mod update_vulnerable_dependencies;

use crate::core::application::Handle;
use crate::core::database::{pk, SweepKind};
use crate::core::message::Payload;

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
