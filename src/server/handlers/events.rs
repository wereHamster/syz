use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::sse::{Event as SseEvent, Sse},
};
use futures::stream::{self, Stream};
use std::convert::Infallible;
use tokio::sync::broadcast::error::RecvError;

use super::AppState;

pub async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, (StatusCode, String)> {
    let auth = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or((StatusCode::UNAUTHORIZED, "Missing authorization header".to_string()))?;

    let token = auth
        .strip_prefix("Bearer ")
        .ok_or((StatusCode::UNAUTHORIZED, "Invalid authorization format".to_string()))?;

    if token != state.token {
        return Err((StatusCode::UNAUTHORIZED, "Invalid token".to_string()));
    }

    let rx = state.handle.subscribe();

    let stream = stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let sse_event = SseEvent::default()
                        .json_data(&event)
                        .unwrap_or_else(|_| SseEvent::default().data("serialization error"));
                    return Some((Ok(sse_event), rx));
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return None,
            }
        }
    });

    Ok(Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default()))
}
