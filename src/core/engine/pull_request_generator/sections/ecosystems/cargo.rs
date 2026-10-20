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
        Box::new(CargoSummarySection),
        Box::new(PolicySection),
        Box::new(AdvisoriesSection),
        Box::new(CargoHistorySection),
    ]
}

pub struct CargoSummarySection;

#[async_trait]
impl PullRequestSectionGenerator for CargoSummarySection {
    fn name(&self) -> &'static str {
        "cargo summary"
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

pub struct CargoHistorySection;

#[async_trait]
impl PullRequestSectionGenerator for CargoHistorySection {
    fn name(&self) -> &'static str {
        "cargo history"
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
