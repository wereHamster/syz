use anyhow::Result;
use async_trait::async_trait;

use crate::core::engine::pull_request_generator::context::PullRequestGenerationContext;
use crate::core::engine::pull_request_generator::sections::PullRequestSectionGenerator;

pub struct PolicySection;

#[async_trait]
impl PullRequestSectionGenerator for PolicySection {
    async fn generate(&self, ctx: &PullRequestGenerationContext<'_>) -> Result<Option<String>> {
        let targets = ctx.targets;
        let mut body = String::new();

        let all_same = !targets.is_empty()
            && targets.iter().all(|t| {
                t.current_version.version == targets[0].current_version.version
                    && t.target_version.version == targets[0].target_version.version
            });

        let policy_targets = if all_same && targets.len() > 1 {
            vec![&targets[0]]
        } else {
            targets.iter().collect::<Vec<_>>()
        };

        for target in policy_targets {
            if target.latest_version != target.target_version.version
                && target.minimum_release_age.is_some()
            {
                if let Ok(history) = ctx
                    .registry_router
                    .fetch_release_history(
                        ctx.ecosystem,
                        &target.name,
                        &target.target_version.version,
                        &target.latest_version,
                    )
                    .await
                {
                    let count = history.len().saturating_sub(1);

                    let format_availability = |time: chrono::DateTime<chrono::Utc>| -> String {
                        let now = chrono::Utc::now();
                        let min_age = target
                            .minimum_release_age
                            .unwrap_or(chrono::Duration::zero());
                        let available_time = time + min_age;
                        let remaining = available_time.signed_duration_since(now).num_seconds();
                        let days = (remaining as f64 / 86400.0).ceil() as i64;
                        if days > 1 {
                            format!("in {} days", days)
                        } else {
                            format!("{} UTC", available_time.format("%A at %H:%M"))
                        }
                    };

                    let subject = if all_same && targets.len() > 1 {
                        ctx.package_group.to_string()
                    } else {
                        target.name.clone()
                    };

                    if count == 1 {
                        let availability = if let Some(head) = history.first() {
                            format_availability(head.publish_time)
                        } else {
                            "now".to_string()
                        };
                        body.push_str(&format!(
                            "> [!IMPORTANT]\n> 1 more recent version of `{}` is released ({}). However, that version is not approved yet by policy. It will become available {}.\n\n",
                            subject, target.latest_version, availability
                        ));
                    } else if count > 1 {
                        let latest_version = history
                            .first()
                            .map(|v| v.version.clone())
                            .unwrap_or_else(|| target.latest_version.clone());
                        let latest_availability = history
                            .first()
                            .map(|v| format_availability(v.publish_time))
                            .unwrap_or_else(|| "now".to_string());

                        let next_version_idx = history.len().saturating_sub(2);
                        let next_version = history
                            .get(next_version_idx)
                            .map(|v| v.version.clone())
                            .unwrap_or_default();
                        let next_availability = history
                            .get(next_version_idx)
                            .map(|v| format_availability(v.publish_time))
                            .unwrap_or_else(|| "now".to_string());

                        body.push_str(&format!(
                            "> [!IMPORTANT]\n> {} more recent versions of `{}` are released, the latest one being {}. However, these versions are not approved yet by policy. The next version ({}) becomes available {}, the latest version ({}) becomes available {}.\n\n",
                            count, subject, target.latest_version, next_version, next_availability, latest_version, latest_availability
                        ));
                    } else {
                        body.push_str(&format!(
                            "> [!IMPORTANT]\n> A more recent version of `{}` is released ({}). However, it is not approved yet by policy.\n\n",
                            subject, target.latest_version
                        ));
                    }
                } else {
                    let subject = if all_same && targets.len() > 1 {
                        ctx.package_group.to_string()
                    } else {
                        target.name.clone()
                    };
                    body.push_str(&format!(
                        "> [!IMPORTANT]\n> A more recent version of `{}` is released ({}). However, it is not approved yet by policy.\n\n",
                        subject, target.latest_version
                    ));
                }
            }
        }

        if body.is_empty() {
            Ok(None)
        } else {
            Ok(Some(body))
        }
    }
}
