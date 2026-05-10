use anyhow::Result;
use async_trait::async_trait;

use crate::core::engine::pull_request_generator::context::PullRequestGenerationContext;
use crate::core::engine::pull_request_generator::sections::PullRequestSectionGenerator;

pub struct AdvisoriesSection;

#[async_trait]
impl PullRequestSectionGenerator for AdvisoriesSection {
    async fn generate(&self, ctx: &PullRequestGenerationContext<'_>) -> Result<Option<String>> {
        let mut advisories_md = String::new();
        let mut seen_advisories = std::collections::HashSet::new();

        for target in ctx.targets {
            if let Ok(advs) = ctx
                .advisory_resolver
                .resolve_advisories(
                    ctx.ecosystem,
                    &target.name,
                    &target.current_version.version,
                    &target.target_version.version,
                )
                .await
            {
                for adv in advs {
                    if seen_advisories.insert(adv.id.clone()) {
                        advisories_md
                            .push_str(&format!("> - [{}]({}): {}\n", adv.id, adv.url, adv.title));
                    }
                }
            }
        }

        if !advisories_md.is_empty() {
            let mut body = String::new();
            body.push_str(
                "> [!CAUTION]\n> This update resolves the following security advisories:\n",
            );
            body.push_str(&advisories_md);
            body.push_str("\n");
            Ok(Some(body))
        } else {
            Ok(None)
        }
    }
}
