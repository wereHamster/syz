use anyhow::Result;
use async_trait::async_trait;

use crate::core::engine::pull_request_generator::budget::SectionKind;
use crate::core::engine::pull_request_generator::context::PullRequestGenerationContext;
use crate::core::engine::pull_request_generator::sections::{summary, PullRequestSectionGenerator};

/// None of the other sections produce anything for flake inputs yet.
pub fn sections() -> Vec<Box<dyn PullRequestSectionGenerator>> {
    vec![Box::new(NixFlakeSummarySection)]
}

pub struct NixFlakeSummarySection;

#[async_trait]
impl PullRequestSectionGenerator for NixFlakeSummarySection {
    fn name(&self) -> &'static str {
        "nix-flake summary"
    }

    fn kind(&self) -> SectionKind {
        SectionKind::Summary
    }

    async fn generate(
        &self,
        ctx: &PullRequestGenerationContext<'_>,
        budget: usize,
    ) -> Result<Option<String>> {
        summary::generate(ctx, budget, |_, _, _| None).await
    }
}
