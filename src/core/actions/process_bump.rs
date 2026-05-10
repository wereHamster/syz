use crate::core::application::Application;
use crate::core::message::Payload;
use anyhow::Result;

pub async fn run(app: &Application, bump_id: String) -> Result<()> {
    let handle = app.handle();

    // Spawn a background task
    tokio::spawn(async move {
        // ... (This is where the actual bump processing will happen) ...
        // We'll wire up the mutator, tempdir, and patcher in a subsequent step.
        // For now we just finish and persist the mock result.

        let result_payload = Payload::PersistBumpResult {
            bump_id,
            pull_request_url: None, // No URL yet
        };

        if let Err(e) = handle
            .send(crate::core::database::pk(), result_payload)
            .await
        {
            tracing::error!("Failed to send PersistBumpResult: {}", e);
        }
    });

    Ok(())
}
