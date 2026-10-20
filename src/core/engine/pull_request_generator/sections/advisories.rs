use anyhow::Result;
use async_trait::async_trait;

use crate::core::engine::pull_request_generator::budget::{fit_items, SectionKind};
use crate::core::engine::pull_request_generator::context::PullRequestGenerationContext;
use crate::core::engine::pull_request_generator::sections::PullRequestSectionGenerator;

pub struct AdvisoriesSection;

#[async_trait]
impl PullRequestSectionGenerator for AdvisoriesSection {
    fn name(&self) -> &'static str {
        "advisories"
    }

    fn kind(&self) -> SectionKind {
        SectionKind::Notices
    }

    async fn generate(
        &self,
        ctx: &PullRequestGenerationContext<'_>,
        budget: usize,
    ) -> Result<Option<String>> {
        let mut advisories = Vec::new();
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
                        advisories.push(format!("> - [{}]({}): {}\n", adv.id, adv.url, adv.title));
                    }
                }
            }
        }

        if advisories.is_empty() {
            return Ok(None);
        }

        Ok(fit_items(
            "> [!NOTE]\n> This update resolves the following security advisories:\n",
            &advisories,
            "\n",
            budget,
            |n| format!("> - … and {} more\n", n),
        ))
    }
}
