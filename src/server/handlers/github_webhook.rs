use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;

use super::AppState;
use crate::core::{database::pk, message::Payload};

#[derive(Deserialize)]
pub struct GithubPushPayload {
    #[serde(rename = "ref")]
    ref_name: String,
    repository: GithubRepository,
}

#[derive(Deserialize)]
pub struct GithubRepository {
    html_url: String,
}

pub async fn post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<GithubPushPayload>,
) -> Result<StatusCode, (StatusCode, String)> {
    let event = headers
        .get("X-GitHub-Event")
        .and_then(|v| v.to_str().ok())
        .ok_or((
            StatusCode::BAD_REQUEST,
            "Missing X-GitHub-Event header".to_string(),
        ))?;

    if event != "push" {
        return Ok(StatusCode::OK);
    }

    if payload.ref_name != "refs/heads/main" {
        return Ok(StatusCode::OK);
    }

    let repo_url = &payload.repository.html_url;
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
        repo_url.clone()
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
        .find(|p| p.platform == "github" && p.repository == db_repo_name);

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
