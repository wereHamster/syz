pub mod handlers;

use anyhow::Result;
use axum::{routing::get, Router};
use tokio::net::TcpListener;

pub async fn start(handle: crate::core::application::Handle, token: String) -> Result<()> {
    let app_state = handlers::AppState { handle, token };

    let app = Router::new()
        .route("/", get(handlers::root::ascii_art))
        .route("/messages", axum::routing::post(handlers::messages::post))
        .route("/events", axum::routing::get(handlers::events::get))
        .with_state(app_state);

    let addr = "0.0.0.0:7790";
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("HTTP server listening on {}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}
