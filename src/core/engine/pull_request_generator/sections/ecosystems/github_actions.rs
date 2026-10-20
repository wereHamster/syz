use anyhow::Result;
use async_trait::async_trait;

use crate::core::engine::pull_request_generator::budget::SectionKind;
use crate::core::engine::pull_request_generator::context::PullRequestGenerationContext;
use crate::core::engine::pull_request_generator::sections::{
    advisories::AdvisoriesSection, history, policy::PolicySection, summary,
    PullRequestSectionGenerator,
};

pub fn sections() -> Vec<Box<dyn PullRequestSectionGenerator>> {
    vec![
        Box::new(GitHubActionsSummarySection),
        Box::new(PolicySection),
        Box::new(AdvisoriesSection),
        Box::new(GitHubActionsHistorySection),
    ]
}

pub struct GitHubActionsSummarySection;

#[async_trait]
impl PullRequestSectionGenerator for GitHubActionsSummarySection {
    fn name(&self) -> &'static str {
        "github-actions summary"
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

pub struct GitHubActionsHistorySection;

#[async_trait]
impl PullRequestSectionGenerator for GitHubActionsHistorySection {
    fn name(&self) -> &'static str {
        "github-actions history"
    }

    fn kind(&self) -> SectionKind {
        SectionKind::History
    }

    async fn generate(
        &self,
        ctx: &PullRequestGenerationContext<'_>,
        budget: usize,
    ) -> Result<Option<String>> {
        history::generate(ctx, budget).await
    }
}
