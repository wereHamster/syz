use crate::core::clients::github::GitHub;
use crate::core::engine::advisories::AdvisoryResolver;
use crate::core::engine::ecosystems::registry_router::RegistryRouter;
use crate::core::engine::UpdateTarget;

pub struct PullRequestGenerationContext<'a> {
    pub package_group: &'a str,
    pub ecosystem: &'a str,
    pub targets: &'a [UpdateTarget],
    pub is_major: bool,
    pub registry_router: &'a RegistryRouter,
    pub advisory_resolver: &'a dyn AdvisoryResolver,
    pub github: &'a GitHub,
}
