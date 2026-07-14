use crate::core::application::Application;
use anyhow::Result;
use tempfile::TempDir;
use tracing::instrument::WithSubscriber;
use tracing::Instrument;

pub async fn run(app: &Application, project_id: String) -> Result<()> {
    let project = app.store().project(&project_id).await?;
    let patchers = app.patchers();

    let view = app.project_repository_view(&project.id).await?;
    let default_branch = view.get_default_branch().await?;
    let base_revision = view.get_revision(&default_branch).await?;
    let mutator = app.project_repository_mutator(&project.id).await?;
    let snapshot = view.snapshot(&base_revision);

    let pr_generator = app.transitive_pull_request_generator();
    let advisory_resolver = app.advisory_resolver();

    let span = tracing::Span::current();

    tokio::spawn(async move {
        let mut all_modifications = Vec::new();
        let mut all_summaries = Vec::new();

        for (ecosystem_name, patcher) in patchers {
            let temp_dir_result = TempDir::new();
            if let Err(e) = temp_dir_result {
                tracing::error!("Failed to create temporary directory: {}", e);
                continue;
            }
            let temp_dir = temp_dir_result.unwrap();

            match patcher
                .update_transitive_dependencies(snapshot.as_ref(), temp_dir.path())
                .await
            {
                Ok(Some(mut result)) => {
                    if !result.modifications.is_empty() {
                        all_modifications.extend(result.modifications);

                        // Resolve advisories for bumped packages
                        resolve_advisories_for_bumps(
                            advisory_resolver.as_ref(),
                            ecosystem_name,
                            &mut result.summary,
                        )
                        .await;

                        let body = match pr_generator
                            .generate_pull_request_body(&result.summary)
                            .await
                        {
                            Ok(b) => b,
                            Err(e) => {
                                tracing::error!("Failed to generate transitive PR body: {}", e);
                                String::from("Failed to generate PR body.")
                            }
                        };

                        all_summaries.push(format!("### {}\n\n{}", ecosystem_name, body));
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::error!(
                        "Failed to update transitive dependencies for {}: {}",
                        ecosystem_name,
                        e
                    );
                }
            }
        }

        let branch_name = "syz/transitive".to_string();

        if all_modifications.is_empty() {
            let _ = mutator
                .close_pull_request(&branch_name, &default_branch)
                .await;
            return;
        }

        let title = "Update transitive dependencies".to_string();
        let combined_summary = all_summaries.join("\n\n");

        let commit_res = mutator
            .commit_changes(&base_revision, &branch_name, &title, all_modifications)
            .await;

        match commit_res {
            Ok(Some(_sha)) => {
                match mutator
                    .create_pull_request(&title, &branch_name, &default_branch, &combined_summary)
                    .await
                {
                    Ok(url) => tracing::info!("Created transitive update PR: {}", url),
                    Err(e) => tracing::error!("Failed to create transitive PR: {}", e),
                }
            }
            Ok(None) => {
                tracing::info!("Modifications resulted in no changes relative to base branch. Cleaning up empty PRs.");
                let _ = mutator
                    .close_pull_request(&branch_name, &default_branch)
                    .await;
            }
            Err(e) => {
                tracing::error!("Failed to commit changes for transitive updates: {}", e);
            }
        }
    }
    .instrument(span)
    .with_current_subscriber());

    Ok(())
}

/// Parse a version bump description like `` `1.2.3` -> `2.0.0` `` or
/// `` `1.2.3, 1.2.4` -> `2.0.0` `` and return (before_vers, after_vers).
fn parse_bump_description(desc: &str) -> (Vec<String>, Vec<String>) {
    let stripped = desc.trim_matches('`');
    let mut parts = stripped.splitn(2, " -> ");
    let before_str = parts.next().unwrap_or("").trim().trim_matches('`');
    let after_str = parts.next().unwrap_or("").trim().trim_matches('`');

    let before_vers: Vec<String> = before_str
        .split(", ")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let after_vers: Vec<String> = after_str
        .split(", ")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    (before_vers, after_vers)
}

/// Resolve security advisories for each bumped module in the summary and
/// populate `resolved_advisories`.
async fn resolve_advisories_for_bumps(
    resolver: &dyn crate::core::engine::advisories::AdvisoryResolver,
    ecosystem: &str,
    summary: &mut crate::core::engine::TransitiveUpdateSummary,
) {
    for (module, desc) in summary.major_bumps.iter().chain(summary.minor_bumps.iter()) {
        let (before_vers, after_vers) = parse_bump_description(desc);
        let current_version = before_vers.first().cloned().unwrap_or_default();
        let target_version = after_vers.first().cloned().unwrap_or_default();

        if current_version.is_empty() || target_version.is_empty() {
            continue;
        }

        match resolver
            .resolve_advisories(ecosystem, module, &current_version, &target_version)
            .await
        {
            Ok(advisories) if !advisories.is_empty() => {
                let advisory_values: Vec<serde_json::Value> = advisories
                    .into_iter()
                    .map(|a| {
                        serde_json::json!({
                            "id": a.id,
                            "title": a.title,
                            "url": a.url,
                            "severity": a.severity,
                            "github_advisory_id": a.id,
                        })
                    })
                    .collect();

                summary.resolved_advisories.insert(
                    module.clone(),
                    crate::core::engine::advisories::ResolvedAdvisoryBump {
                        before_versions: before_vers,
                        after_versions: after_vers,
                        advisories: advisory_values,
                    },
                );
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    "Failed to resolve advisories for `{}` ({}): {}",
                    module,
                    desc,
                    e
                );
            }
        }
    }
}
