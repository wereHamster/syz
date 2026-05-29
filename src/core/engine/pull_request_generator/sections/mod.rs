use anyhow::Result;
use async_trait::async_trait;

use super::context::PullRequestGenerationContext;

pub mod advisories;
pub mod history;
pub mod policy;
pub mod summary;

#[async_trait]
pub trait PullRequestSectionGenerator: Send + Sync {
    async fn generate(
        &self,
        ctx: &PullRequestGenerationContext<'_>,
        current_length: usize,
    ) -> Result<Option<String>>;
}
