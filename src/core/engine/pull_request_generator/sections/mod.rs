use anyhow::Result;
use async_trait::async_trait;

use super::context::PullRequestGenerationContext;

pub mod advisories;
pub mod history;
pub mod policy;
pub mod summary;

use advisories::AdvisoriesSection;
use history::HistorySection;
use policy::PolicySection;
use summary::SummarySection;

#[async_trait]
pub trait PullRequestSectionGenerator: Send + Sync {
    /// Names the section in the log when it fails.
    fn name(&self) -> &'static str;

    /// Returns `None` when the section doesn't apply or has nothing to report, which omits it.
    async fn generate(
        &self,
        ctx: &PullRequestGenerationContext<'_>,
        current_length: usize,
    ) -> Result<Option<String>>;
}

/// The sections of an ecosystem's part of a pull request description, in priority order.
pub fn sections_for(ecosystem: &str) -> Vec<Box<dyn PullRequestSectionGenerator>> {
    match ecosystem {
        // None of the other sections produce anything for flake inputs yet.
        "nix-flake" => vec![Box::new(SummarySection)],
        _ => vec![
            Box::new(SummarySection),
            Box::new(PolicySection),
            Box::new(AdvisoriesSection),
            Box::new(HistorySection),
        ],
    }
}
