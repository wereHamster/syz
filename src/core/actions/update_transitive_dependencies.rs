use super::{Change, DefaultBranch};
use crate::core::database::SweepKind;
use crate::core::job::LaneKey;
use crate::core::message::Payload;
use crate::core::services::Services;
use anyhow::{Context, Result};

/// Create or update the pull request that updates all transitive dependencies of the project,
/// or close it if there is nothing to update.
pub async fn run(
    services: &Services,
    lane: LaneKey,
    project_id: String,
) -> Result<Option<Payload>> {
    let default_branch = DefaultBranch::resolve(services.repository(&lane)?.as_ref()).await?;

    let pr_generator = services.transitive_pull_request_generator();
    let advisory_resolver = services.advisory_resolver();

    let mut modifications = Vec::new();
    let mut summaries = Vec::new();

    for (ecosystem_name, patcher) in services.patchers() {
        let temp_dir = super::create_temp_dir(services.work_dir())?;

        // Without a trustworthy result for every ecosystem, neither opening nor closing the
        // pull request would be correct.
        let Some(mut result) = patcher
            .update_transitive_dependencies(default_branch.snapshot.as_ref(), temp_dir.path())
            .await
            .with_context(|| {
                format!(
                    "Failed to update transitive dependencies for {}",
                    ecosystem_name
                )
            })?
        else {
            continue;
        };
        if result.modifications.is_empty() {
            continue;
        }

        modifications.extend(result.modifications);

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

        summaries.push(format!("### {}\n\n{}", ecosystem_name, body));
    }

    let change = (!modifications.is_empty()).then(|| Change {
        title: "Update transitive dependencies".to_string(),
        body: summaries.join("\n\n"),
        modifications,
    });

    let kind = SweepKind::Transitive;
    let outcome = default_branch.publish(kind.branch_name(), change).await?;

    Ok(Some(Payload::SweepPublished {
        lane,
        project_id,
        kind,
        outcome,
    }))
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
