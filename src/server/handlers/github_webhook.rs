use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use serde_json::Value;

use super::AppState;
use crate::core::{database::pk, message::Payload, platform::Platform};

pub async fn post(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, (StatusCode, String)> {
    let event = headers
        .get("X-GitHub-Event")
        .and_then(|v| v.to_str().ok())
        .ok_or((
            StatusCode::BAD_REQUEST,
            "Missing X-GitHub-Event header".to_string(),
        ))?
        .to_string();

    let payload: Value = serde_json::from_slice(&body).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Failed to parse webhook payload: {}", e),
        )
    })?;

    match event.as_str() {
        "push" => handle_push(state, payload).await,
        "pull_request" => handle_pull_request(state, payload).await,
        _ => Ok(StatusCode::OK),
    }
}

async fn handle_push(
    state: AppState,
    payload: Value,
) -> Result<StatusCode, (StatusCode, String)> {
    let ref_name = payload.get("ref").and_then(|v| v.as_str()).unwrap_or("");
    if ref_name != "refs/heads/main" {
        return Ok(StatusCode::OK);
    }

    let repo_url = payload
        .pointer("/repository/html_url")
        .and_then(|v| v.as_str())
        .ok_or((
            StatusCode::BAD_REQUEST,
            "Missing repository.html_url".to_string(),
        ))?;
    tracing::info!("GitHub webhook: main branch updated in {}", repo_url);

    // Parse owner/repo from URL (e.g., https://github.com/owner/repo -> owner/repo)
    let repo_parts: Vec<&str> = repo_url.trim_end_matches('/').split('/').collect();
    let db_repo_name = if repo_parts.len() >= 2 {
        format!(
            "{}/{}",
            repo_parts[repo_parts.len() - 2],
            repo_parts[repo_parts.len() - 1]
        )
    } else {
        repo_url.to_string()
    };

    // Find the project corresponding to the repository
    let projects = state.handle.store().list_projects().await.map_err(|e| {
        tracing::error!("Failed to list projects: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to list projects".to_string(),
        )
    })?;

    let project = projects
        .into_iter()
        .find(|p| p.platform == Platform::GitHub && p.repository == db_repo_name);

    let project = match project {
        Some(p) => p,
        None => {
            tracing::info!(
                "No project found for repository {} (expected {} in DB)",
                repo_url,
                db_repo_name
            );
            return Ok(StatusCode::OK);
        }
    };

    tracing::info!("Found project {} for repository {}", project.id, repo_url);

    // 1. Run AnalyzeProjectDependencies to ensure the database is up to date.
    // This will also trigger ProcessBump for any approved bumps after it completes.
    tracing::info!(
        "Scheduling AnalyzeProjectDependencies for project {}",
        project.id
    );
    state
        .handle
        .send(
            pk(),
            Payload::AnalyzeProjectDependencies {
                project_id: project.id.clone(),
                trigger_bumps: true,
            },
        )
        .await
        .map_err(|e| {
            tracing::error!("Failed to send AnalyzeProjectDependencies message: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to schedule analysis".to_string(),
            )
        })?;

    Ok(StatusCode::OK)
}

async fn handle_pull_request(
    state: AppState,
    payload: Value,
) -> Result<StatusCode, (StatusCode, String)> {
    let action = payload.get("action").and_then(|v| v.as_str()).unwrap_or("");
    if action != "closed" {
        return Ok(StatusCode::OK);
    }

    let pr_url = payload
        .pointer("/pull_request/html_url")
        .and_then(|v| v.as_str())
        .ok_or((
            StatusCode::BAD_REQUEST,
            "Missing pull_request.html_url".to_string(),
        ))?;
    let merged = payload
        .pointer("/pull_request/merged")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let bump = state
        .handle
        .store()
        .bump_by_url(pr_url)
        .await
        .map_err(|e| {
            tracing::error!("Failed to look up bump by url: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to look up bump".to_string(),
            )
        })?;

    let bump = match bump {
        Some(b) => b,
        None => {
            tracing::info!("No bump found for pull request {}", pr_url);
            return Ok(StatusCode::OK);
        }
    };

    let (payload, description) = if merged {
        (Payload::RemoveBump { bump_id: bump.id }, "RemoveBump")
    } else {
        (Payload::ClearBumpUrl { bump_id: bump.id }, "ClearBumpUrl")
    };

    tracing::info!(
        "Pull request {} closed (merged: {}), scheduling {}",
        pr_url,
        merged,
        description
    );
    state.handle.send(pk(), payload).await.map_err(|e| {
        tracing::error!("Failed to send {} message: {}", description, e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to schedule {}", description),
        )
    })?;

    Ok(StatusCode::OK)
}
