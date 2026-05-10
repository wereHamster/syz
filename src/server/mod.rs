pub mod handlers;

use anyhow::Result;
use axum::{routing::get, Router};
use tokio::net::TcpListener;

pub async fn start() -> Result<()> {
    let app = Router::new().route("/", get(handlers::root::ascii_art));

    let addr = "0.0.0.0:7790";
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("HTTP server listening on {}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}
