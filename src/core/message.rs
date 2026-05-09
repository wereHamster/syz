use anyhow::Result;

use super::application::Application;

#[derive(Clone)]
pub struct Message {
    /// A client-defined ID of the message.
    ///
    /// Events generated as a consequence of this message may carry this ID. This allows
    /// applicatino clients to correlate events (output) to messages (input).
    pub message_id: String,

    pub payload: Payload,
}

#[derive(Clone)]
pub enum Payload {
    /// Scan the project source code, identify which dependencies it has, check which
    /// dependencies are outdated, and update the local database with the information.
    AnalyzeProjectDependencies { project_id: String },
}

impl Payload {
    pub async fn execute(&self, app: &Application) -> Result<()> {
        match self {
            Payload::AnalyzeProjectDependencies { project_id } => {
                super::actions::analyze_project_dependencies::run(app, project_id.clone()).await
            }
        }
    }
}
