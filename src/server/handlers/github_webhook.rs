use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::Value;
use sha2::Sha256;

use super::AppState;
use crate::core::{
    database::{pk, SweepKind},
    message::Payload,
    platform::Platform,
};

pub async fn post(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, (StatusCode, String)> {
    let signature = headers
        .get("X-Hub-Signature-256")
        .and_then(|v| v.to_str().ok())
        .ok_or((
            StatusCode::UNAUTHORIZED,
            "Missing X-Hub-Signature-256 header".to_string(),
        ))?;

    verify_signature(&state.github_webhook_secret, signature, &body)?;

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

/// Verify the `X-Hub-Signature-256` header against the HMAC-SHA256 of the raw
/// request body, as described in
/// <https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries>.
fn verify_signature(
    secret: &str,
    signature: &str,
    body: &[u8],
) -> Result<(), (StatusCode, String)> {
    let invalid = || (StatusCode::UNAUTHORIZED, "Invalid signature".to_string());

    let digest = signature.strip_prefix("sha256=").ok_or_else(invalid)?;
    let digest = hex::decode(digest).map_err(|_| invalid())?;

    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC-SHA256 accepts keys of any size");
    mac.update(body);
    mac.verify_slice(&digest).map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test vector from the GitHub documentation.
    const SECRET: &str = "It's a Secret to Everybody";
    const BODY: &[u8] = b"Hello, World!";
    const SIGNATURE: &str =
        "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";

    #[test]
    fn accepts_a_valid_signature() {
        assert!(verify_signature(SECRET, SIGNATURE, BODY).is_ok());
    }

    #[test]
    fn rejects_a_tampered_body() {
        assert!(verify_signature(SECRET, SIGNATURE, b"Goodbye, World!").is_err());
    }

    #[test]
    fn rejects_a_wrong_secret() {
        assert!(verify_signature("nope", SIGNATURE, BODY).is_err());
    }

    #[test]
    fn rejects_a_malformed_signature() {
        assert!(verify_signature(SECRET, "not-a-signature", BODY).is_err());
        assert!(verify_signature(SECRET, "sha256=zzzz", BODY).is_err());
    }
}

async fn handle_push(state: AppState, payload: Value) -> Result<StatusCode, (StatusCode, String)> {
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
    // With trigger_bumps, the approved bumps are published after it completes.
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

    // 2. Sweeps don't depend on the analysis, so update the open ones right away.
    state
        .handle
        .send(
            pk(),
            Payload::UpdateSweeps {
                project_id: project.id.clone(),
            },
        )
        .await
        .map_err(|e| {
            tracing::error!("Failed to send UpdateSweeps message: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to schedule sweep updates".to_string(),
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

    // Audit and transitive pull requests are dropped whether they were merged or just
    // closed; the next run opens a new one if there is still something to do. Whether the
    // closed pull request is the one the sweep currently tracks is decided by the
    // application, so that it is ordered with the sweep updates that are already queued.
    let is_sweep = payload
        .pointer("/pull_request/head/ref")
        .and_then(|v| v.as_str())
        .and_then(SweepKind::from_branch_name)
        .is_some();

    if is_sweep {
        tracing::info!(
            "Pull request {} closed (merged: {}), scheduling SweepPullRequestClosed",
            pr_url,
            merged
        );
        state
            .handle
            .send(
                pk(),
                Payload::SweepPullRequestClosed {
                    pull_request_url: pr_url.to_string(),
                },
            )
            .await
            .map_err(|e| {
                tracing::error!("Failed to send SweepPullRequestClosed message: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to schedule SweepPullRequestClosed".to_string(),
                )
            })?;

        return Ok(StatusCode::OK);
    }

    // Which bump the pull request belongs to is decided by the application, so that it is
    // ordered with the bump results that are already queued.
    tracing::info!(
        "Pull request {} closed (merged: {}), scheduling BumpPullRequestClosed",
        pr_url,
        merged
    );
    state
        .handle
        .send(
            pk(),
            Payload::BumpPullRequestClosed {
                pull_request_url: pr_url.to_string(),
                merged,
            },
        )
        .await
        .map_err(|e| {
            tracing::error!("Failed to send BumpPullRequestClosed message: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to schedule BumpPullRequestClosed".to_string(),
            )
        })?;

    Ok(StatusCode::OK)
}
