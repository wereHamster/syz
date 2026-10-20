use anyhow::Result;
use async_trait::async_trait;

use super::budget::SectionKind;
use super::context::PullRequestGenerationContext;

pub mod advisories;
pub mod ecosystems;
pub mod history;
pub mod policy;
pub mod summary;

#[async_trait]
pub trait PullRequestSectionGenerator: Send + Sync {
    /// Names the section in the log when it fails.
    fn name(&self) -> &'static str;

    /// Which part of the description the section goes into.
    fn kind(&self) -> SectionKind;

    /// Returns `None` when the section doesn't apply or has nothing to report, which omits it.
    ///
    /// The result must not be longer than `budget` bytes. A section that has more than that to
    /// say cuts it down in a way that suits its content, and says that it did.
    async fn generate(
        &self,
        ctx: &PullRequestGenerationContext<'_>,
        budget: usize,
    ) -> Result<Option<String>>;
}

/// The sections of an ecosystem's part of a pull request description.
///
/// Each ecosystem decides which sections it has and how they look. Where the sections end up in
/// the description is up to their kind.
pub fn sections_for(ecosystem: &str) -> Vec<Box<dyn PullRequestSectionGenerator>> {
    match ecosystem {
        "cargo" => ecosystems::cargo::sections(),
        "npm" => ecosystems::npm::sections(),
        "github-actions" => ecosystems::github_actions::sections(),
        "nix-flake" => ecosystems::nix_flake::sections(),
        _ => {
            tracing::warn!("No pull request sections for ecosystem {}", ecosystem);
            Vec::new()
        }
    }
}
