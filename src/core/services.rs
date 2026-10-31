use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::actions;
use super::clients;
use super::clients::github::GitHub;
use super::database::SweepKind;
use super::dispatcher::Runner;
use super::engine::advisories::{AdvisoryResolver, OsvAdvisoryResolver};
use super::engine::ecosystems::{
    cargo::{CargoPatcher, CargoRegistry, CargoScanner},
    github_actions::{GitHubPatcher, GitHubRegistry, GitHubScanner},
    nix_flake::{NixFlakePatcher, NixFlakeRegistry, NixFlakeScanner},
    npm::{NpmPatcher, NpmRegistry, NpmScanner},
    registry_router::RegistryRouter,
    Patcher, Registry, Scanner,
};
use super::engine::pull_request_generator::{
    AuditPullRequestGenerator, DefaultAuditPullRequestGenerator, DefaultPullRequestGenerator,
    DefaultTransitiveUpdatesPullRequestGenerator, PullRequestGenerator,
    TransitiveUpdatesPullRequestGenerator,
};
use super::http_agent::HttpAgent;
use super::job::{Job, LaneKey};
use super::message::Payload;
use super::platform::{PlatformRegistry, ProjectPlatformRepository};
use super::store::Store;

/// What jobs build their clients from. Cheap to clone, so that every job can have its own.
#[derive(Clone)]
pub struct Services {
    /// Jobs may read the store, but only the main loop writes to it.
    store: Store,

    http_agent: HttpAgent,
    github: GitHub,
    platforms: Arc<PlatformRegistry>,

    /// Where jobs create the scratch directories in which package managers run.
    work_dir: PathBuf,
}

impl Services {
    pub fn new(
        store: Store,
        http_agent: HttpAgent,
        github: GitHub,
        platforms: PlatformRegistry,
        work_dir: PathBuf,
    ) -> Self {
        Self {
            store,
            http_agent,
            github,
            platforms: Arc::new(platforms),
            work_dir,
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    /// The repository that the jobs of the lane work on.
    pub fn repository(&self, lane: &LaneKey) -> Result<Box<dyn ProjectPlatformRepository>> {
        let platform = self.platforms.resolve(lane.platform)?;
        Ok(platform.repository(&lane.repository))
    }

    pub fn scanners(&self) -> Vec<Box<dyn Scanner>> {
        vec![
            Box::new(CargoScanner),
            Box::new(NpmScanner),
            Box::new(GitHubScanner),
            Box::new(NixFlakeScanner),
        ]
    }

    pub fn registry_router(&self) -> RegistryRouter {
        let mut registries: HashMap<String, Box<dyn Registry>> = HashMap::new();

        registries.insert(
            "cargo".to_string(),
            Box::new(CargoRegistry::new(clients::crates::Crates::new(
                self.http_agent.clone(),
            ))),
        );
        registries.insert(
            "npm".to_string(),
            Box::new(NpmRegistry::new(clients::npm::Npm::new(
                self.http_agent.clone(),
            ))),
        );
        registries.insert(
            "github-actions".to_string(),
            Box::new(GitHubRegistry::new(self.github.clone())),
        );
        registries.insert(
            "nix-flake".to_string(),
            Box::new(NixFlakeRegistry::new(self.github.clone())),
        );

        RegistryRouter::new(registries)
    }

    pub fn advisory_resolver(&self) -> Box<dyn AdvisoryResolver> {
        let osv_client = clients::osv::OsvClient::new(self.http_agent.clone());
        Box::new(OsvAdvisoryResolver::new(osv_client))
    }

    pub fn pull_request_generator(&self) -> Box<dyn PullRequestGenerator> {
        Box::new(DefaultPullRequestGenerator::new(
            self.registry_router(),
            self.advisory_resolver(),
            self.github.clone(),
            self.work_dir.clone(),
        ))
    }

    pub fn transitive_pull_request_generator(
        &self,
    ) -> Box<dyn TransitiveUpdatesPullRequestGenerator> {
        Box::new(DefaultTransitiveUpdatesPullRequestGenerator)
    }

    pub fn audit_pull_request_generator(&self) -> Box<dyn AuditPullRequestGenerator> {
        Box::new(DefaultAuditPullRequestGenerator)
    }

    pub fn patchers(&self) -> Vec<(&'static str, Box<dyn Patcher>)> {
        ["npm", "github-actions", "cargo", "nix-flake"]
            .into_iter()
            .filter_map(|ecosystem| Some((ecosystem, self.patcher(ecosystem)?)))
            .collect()
    }

    pub fn patcher(&self, ecosystem: &str) -> Option<Box<dyn Patcher>> {
        match ecosystem {
            "npm" => Some(Box::new(NpmPatcher::new(clients::npm::Npm::new(
                self.http_agent.clone(),
            )))),
            "github-actions" => Some(Box::new(GitHubPatcher::new(self.github.clone()))),
            "cargo" => Some(Box::new(CargoPatcher::new())),
            "nix-flake" => Some(Box::new(NixFlakePatcher::new(self.github.clone()))),
            _ => None,
        }
    }
}

#[async_trait]
impl Runner for Services {
    async fn run(&self, job: Job) -> Result<Option<Payload>> {
        match job {
            Job::Scan { lane, project_id } => {
                actions::analyze_project_dependencies::run(self, lane, project_id).await
            }
            Job::PublishBump { lane, bump_id } => {
                actions::process_bump::run(self, lane, bump_id).await
            }
            Job::PublishSweep {
                lane,
                project_id,
                kind: SweepKind::Audit,
            } => actions::update_vulnerable_dependencies::run(self, lane, project_id).await,
            Job::PublishSweep {
                lane,
                project_id,
                kind: SweepKind::Transitive,
            } => actions::update_transitive_dependencies::run(self, lane, project_id).await,
            Job::PublishSweep {
                lane,
                project_id,
                kind: SweepKind::Compatible,
            } => actions::update_compatible_dependencies::run(self, lane, project_id).await,
            Job::PruneBranches { lane, project_id } => {
                actions::prune_branches::run(self, lane, project_id).await?;
                Ok(None)
            }
            Job::ClosePullRequests { lane, branches } => {
                actions::close_pull_requests::run(self, lane, branches).await?;
                Ok(None)
            }
        }
    }
}
