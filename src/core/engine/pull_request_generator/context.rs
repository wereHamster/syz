use crate::core::clients::github::GitHub;
use crate::core::engine::advisories::AdvisoryResolver;
use crate::core::engine::ecosystems::registry_router::RegistryRouter;
use crate::core::engine::repository::{FileModification, ProjectRepositorySnapshot};
use crate::core::engine::UpdateTarget;

/// What the sections of one ecosystem's part of a pull request description can draw on.
///
/// `ecosystem` and `targets` are scoped to that ecosystem. `snapshot` is the repository
/// before the update and `modifications` the files the update changes, so a section can
/// compare the state before and after.
///
/// Without a `github` client, sections skip what needs GitHub. Tests don't get one.
pub struct PullRequestGenerationContext<'a> {
    pub package_group: &'a str,
    pub ecosystem: &'a str,
    pub targets: &'a [UpdateTarget],
    pub is_major: bool,
    pub registry_router: &'a RegistryRouter,
    pub advisory_resolver: &'a dyn AdvisoryResolver,
    pub github: Option<&'a GitHub>,
    pub snapshot: &'a dyn ProjectRepositorySnapshot,
    pub modifications: &'a [FileModification],
}
