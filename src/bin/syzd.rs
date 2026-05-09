use anyhow::Result;
use syz::core::{database::pk, message::Payload};
use tracing_subscriber::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing_subscriber();

    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    tracing::info!("starting...");

    let application = syz::core::application::Application::new().await?;
    let handle = application.start();

    let query = handle.query();
    let projects = query.list_projects().await?;

    for project in projects {
        tracing::info!("project {}", project.id.clone());

        handle
            .send(
                pk().into(),
                Payload::AnalyzeProjectDependencies {
                    project_id: project.id.clone(),
                },
            )
            .await?;
    }

    tokio::time::sleep(std::time::Duration::from_secs(300)).await;

    Ok(())
}

pub fn init_tracing_subscriber() {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    // If we detect that stdout/stderr is connected to journald, use the
    // journald-specific layer.
    //
    // If connecting to journald fails, fall through to the fmt subscriber.
    if std::env::var("JOURNAL_STREAM").is_ok() {
        if let Ok(journald_layer) = tracing_journald::layer() {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(journald_layer)
                .init();
            return;
        }
    }

    tracing_subscriber::fmt().with_env_filter(env_filter).init();
}
