use anyhow::Result;
use async_trait::async_trait;

use crate::core::engine::UpdateTarget;

pub mod context;
pub mod formatting;
pub mod sections;
pub mod title;

use context::PullRequestGenerationContext;
use sections::{
    advisories::AdvisoriesSection, history::HistorySection, policy::PolicySection,
    summary::SummarySection, PullRequestSectionGenerator,
};

#[async_trait]
pub trait PullRequestGenerator: Send + Sync {
    async fn generate_pull_request_title(
        &self,
        package_group: &str,
        targets: &[UpdateTarget],
        is_major: bool,
    ) -> Result<String>;

    async fn generate_pull_request_body(
        &self,
        package_group: &str,
        ecosystem: &str,
        targets: &[UpdateTarget],
        is_major: bool,
    ) -> Result<String>;
}

pub struct DefaultPullRequestGenerator {
    registry_router: crate::core::engine::ecosystems::registry_router::RegistryRouter,
    advisory_resolver: Box<dyn crate::core::engine::advisories::AdvisoryResolver>,
    github: crate::core::clients::github::GitHub,
}

impl DefaultPullRequestGenerator {
    pub fn new(
        registry_router: crate::core::engine::ecosystems::registry_router::RegistryRouter,
        advisory_resolver: Box<dyn crate::core::engine::advisories::AdvisoryResolver>,
        github: crate::core::clients::github::GitHub,
    ) -> Self {
        Self {
            registry_router,
            advisory_resolver,
            github,
        }
    }
}

#[async_trait]
impl PullRequestGenerator for DefaultPullRequestGenerator {
    async fn generate_pull_request_title(
        &self,
        package_group: &str,
        targets: &[UpdateTarget],
        is_major: bool,
    ) -> Result<String> {
        Ok(title::generate_title(package_group, targets, is_major))
    }

    async fn generate_pull_request_body(
        &self,
        package_group: &str,
        ecosystem: &str,
        targets: &[UpdateTarget],
        is_major: bool,
    ) -> Result<String> {
        let ctx = PullRequestGenerationContext {
            package_group,
            ecosystem,
            targets,
            is_major,
            registry_router: &self.registry_router,
            advisory_resolver: self.advisory_resolver.as_ref(),
            github: &self.github,
        };

        let sections: Vec<Box<dyn PullRequestSectionGenerator>> = vec![
            Box::new(SummarySection),
            Box::new(AdvisoriesSection),
            Box::new(PolicySection),
            Box::new(HistorySection),
        ];

        let mut body = String::new();
        for section in sections {
            if let Ok(Some(content)) = section.generate(&ctx).await {
                body.push_str(&content);
            }
        }

        Ok(body)
    }
}
