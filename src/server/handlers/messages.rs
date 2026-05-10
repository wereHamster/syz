use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use serde_json::json;

use crate::core::{database::pk, message::Payload};
use super::AppState;

pub async fn post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Payload>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let auth = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or((StatusCode::UNAUTHORIZED, "Missing authorization header".to_string()))?;

    let token = auth.strip_prefix("Bearer ").ok_or((StatusCode::UNAUTHORIZED, "Invalid authorization format".to_string()))?;

    if token != state.token {
        return Err((StatusCode::UNAUTHORIZED, "Invalid token".to_string()));
    }

    let message_id = pk();
    state
        .handle
        .send(message_id.clone(), payload)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "message_id": message_id })))
}
