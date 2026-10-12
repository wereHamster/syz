use super::{Change, DefaultBranch};
use crate::core::database::SweepKind;
use crate::core::job::LaneKey;
use crate::core::message::Payload;
use crate::core::services::Services;
use anyhow::{Context, Result};

/// Create or update the pull request that fixes the project's vulnerable dependencies, or
/// close it if there is nothing to fix.
pub async fn run(
    services: &Services,
    lane: LaneKey,
    project_id: String,
) -> Result<Option<Payload>> {
    let default_branch = DefaultBranch::resolve(services.repository(&lane)?.as_ref()).await?;

    let pr_generator = services.audit_pull_request_generator();

    let mut modifications = Vec::new();
    let mut bodies = Vec::new();

    for (ecosystem_name, patcher) in services.patchers() {
        let temp_dir = super::create_temp_dir(services.work_dir())?;

        // Without a trustworthy result for every ecosystem, neither opening nor closing the
        // pull request would be correct.
        let Some(result) = patcher
            .update_vulnerable_dependencies(default_branch.snapshot.as_ref(), temp_dir.path())
            .await
            .with_context(|| {
                format!(
                    "Failed to update vulnerable dependencies for {}",
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

        let body = match pr_generator
            .generate_pull_request_body(&result.summary)
            .await
        {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("Failed to generate audit PR body: {}", e);
                String::from("Failed to generate PR body.")
            }
        };
        bodies.push(body);
    }

    if modifications.is_empty() {
        tracing::info!("No fixable security vulnerabilities found.");
    }
    let change = (!modifications.is_empty()).then(|| Change {
        title: "Update dependencies to fix security vulnerabilities".to_string(),
        body: bodies.join("\n\n---\n\n"),
        modifications,
    });

    let kind = SweepKind::Audit;
    let outcome = default_branch.publish(kind.branch_name(), change).await?;

    Ok(Some(Payload::SweepPublished {
        lane,
        project_id,
        kind,
        outcome,
    }))
}
