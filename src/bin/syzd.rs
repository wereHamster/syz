use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::prelude::*;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Directory where syzd keeps its database and auth token
    #[arg(long, env = "SYZD_DATA_DIR", default_value_os_t = default_data_dir())]
    data_dir: PathBuf,
}

/// Under systemd, the state directory (`StateDirectory=`) is exported as
/// `$STATE_DIRECTORY`. Otherwise, use a `data` subfolder of the working
/// directory, which suits local development.
fn default_data_dir() -> PathBuf {
    match std::env::var_os("STATE_DIRECTORY") {
        // With multiple state directories the variable is colon-separated.
        Some(dirs) => std::env::split_paths(&dirs)
            .next()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or_else(|| PathBuf::from("data")),
        None => PathBuf::from("data"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    init_tracing_subscriber();

    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    tracing::info!("starting...");

    tracing::info!("using data directory {}", args.data_dir.display());

    let application = syz::core::application::Application::new(&args.data_dir).await?;
    let handle = application.start();

    let token = match std::env::var("SYZD_AUTH_TOKEN") {
        Ok(token) if !token.is_empty() => token,
        _ => syz::auth_token::load_or_create(&syz::auth_token::server_path(&args.data_dir))?,
    };
    let github_webhook_secret =
        std::env::var("GITHUB_WEBHOOK_SECRET").expect("GITHUB_WEBHOOK_SECRET must be set");
    syz::server::start(handle, token, github_webhook_secret).await?;

    tracing::info!("exiting...");

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
