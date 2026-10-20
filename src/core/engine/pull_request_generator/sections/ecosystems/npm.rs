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
        Box::new(NpmSummarySection),
        Box::new(PolicySection),
        Box::new(AdvisoriesSection),
        Box::new(NpmHistorySection),
    ]
}

/// Links the versions to a diff of the published packages.
fn npmx_diff_url(name: &str, old: &str, new: &str) -> Option<String> {
    Some(format!(
        "https://npmx.dev/diff/{}/v/{}...{}",
        name, old, new
    ))
}

pub struct NpmSummarySection;

#[async_trait]
impl PullRequestSectionGenerator for NpmSummarySection {
    fn name(&self) -> &'static str {
        "npm summary"
    }

    fn kind(&self) -> SectionKind {
        SectionKind::Summary
    }

    async fn generate(
        &self,
        ctx: &PullRequestGenerationContext<'_>,
        budget: usize,
    ) -> Result<Option<String>> {
        summary::generate(ctx, budget, npmx_diff_url).await
    }
}

pub struct NpmHistorySection;

#[async_trait]
impl PullRequestSectionGenerator for NpmHistorySection {
    fn name(&self) -> &'static str {
        "npm history"
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
