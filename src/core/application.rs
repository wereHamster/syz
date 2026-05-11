use anyhow::Result;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

use crate::core::clients;
use crate::core::platform::{GitHubPlatformAdapter, PlatformRegistry, TangledPlatformAdapter};

use super::clients::github::GitHub;
use super::clients::tangled::Tangled;
use super::database::Database;

use super::engine::ecosystems::{
    cargo::{CargoRegistry, CargoScanner},
    github_actions::{GitHubRegistry, GitHubScanner},
    npm::{NpmRegistry, NpmScanner},
    registry_router::RegistryRouter,
    Registry, Scanner,
};
use super::event::Event;
use super::http_agent::HttpAgent;
use super::message::{Message, Payload};
use super::store::Store;

pub struct Application {
    store: Store,
    handle: Handle,

    mailbox: mpsc::Receiver<Message>,

    http_agent: HttpAgent,
    github: GitHub,

    platforms: PlatformRegistry,
}

impl Application {
    pub async fn new() -> Result<Self> {
        let (mailbox_tx, mailbox_rx) = mpsc::channel(100);
        let (events, _) = broadcast::channel(1000);

        let database = Database::open().await?;
        let store = Store::new(database);

        let http_agent = HttpAgent::new();
        let github = GitHub::new().await?;
        let tangled = Tangled::new().await?;

        let mut platforms = PlatformRegistry::new();
        platforms.register(
            "github",
            Arc::new(GitHubPlatformAdapter::new(github.clone())),
        );
        platforms.register("tangled", Arc::new(TangledPlatformAdapter::new(tangled)));

        Ok(Self {
            store: store.clone(),
            handle: Handle {
                store,
                mailbox: mailbox_tx,
                events,
            },

            mailbox: mailbox_rx,

            http_agent,
            github,
            platforms,
        })
    }

    pub fn start(self) -> Handle {
        let handle = self.handle.clone();

        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

        let log_layer = super::logger::CoreLogLayer::new(handle.events.clone());

        use tracing_subscriber::layer::SubscriberExt;
        let subscriber = tracing_subscriber::registry()
            .with(filter)
            .with(log_layer)
            .with(tracing_subscriber::fmt::layer());
        let dispatch = tracing::Dispatch::new(subscriber);

        use tracing::instrument::WithSubscriber;
        tokio::spawn(
            async move {
                tracing::info!("Event loop active");

                if let Err(e) = self.run().await {
                    tracing::error!("Application loop exited with error: {:#}", e);
                }
            }
            .with_subscriber(dispatch),
        );

        handle
    }

    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn scanners(&self) -> Vec<Box<dyn Scanner>> {
        vec![
            Box::new(CargoScanner),
            Box::new(NpmScanner),
            Box::new(GitHubScanner),
        ]
    }

    pub fn registry_router(&self) -> RegistryRouter {
        let mut registries: std::collections::HashMap<String, Box<dyn Registry>> =
            std::collections::HashMap::new();

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

        RegistryRouter::new(registries)
    }

    pub async fn project_repository_view(
        &self,
        project_id: &str,
    ) -> Result<Box<dyn super::engine::repository::ProjectRepositoryView>> {
        let project = self.store.project(project_id).await?;
        let platform = self.platforms.resolve(&project.platform)?;

        platform.view(&project.repository).await
    }

    pub async fn project_repository_snapshot(
        &self,
        project_id: &str,
    ) -> Result<Box<dyn super::engine::repository::ProjectRepositorySnapshot>> {
        let view = self.project_repository_view(&project_id).await?;
        let default_branch = view.get_default_branch().await?;
        let revision = view.get_revision(default_branch.as_str()).await?;
        Ok(view.snapshot(revision.as_str()))
    }

    async fn run(mut self) -> Result<()> {
        while let Some(msg) = self.mailbox.recv().await {
            tracing::info!("Processing message {}", msg.message_id);

            if let Err(e) = msg.payload.execute(&self).await {
                tracing::warn!("Failed to process message {}: {:#}", msg.message_id, e);
            }
        }

        tracing::error!("Application loop is unexpectedly exiting");

        Ok(())
    }

    pub async fn project_repository_mutator(
        &self,
        project_id: &str,
    ) -> Result<Box<dyn super::engine::repository::ProjectRepositoryMutator>> {
        let project = self.store.project(project_id).await?;
        let platform = self.platforms.resolve(&project.platform)?;

        platform.mutator(&project.repository).await
    }

    pub fn advisory_resolver(&self) -> Box<dyn crate::core::engine::advisories::AdvisoryResolver> {
        let osv_client = crate::core::clients::osv::OsvClient::new(self.http_agent.clone());
        Box::new(crate::core::engine::advisories::OsvAdvisoryResolver::new(
            osv_client,
        ))
    }

    pub fn pull_request_generator(
        &self,
    ) -> Box<dyn crate::core::engine::pull_request_generator::PullRequestGenerator> {
        Box::new(
            crate::core::engine::pull_request_generator::DefaultPullRequestGenerator::new(
                self.registry_router(),
                self.advisory_resolver(),
                self.github.clone(),
            ),
        )
    }

    pub fn transitive_pull_request_generator(
        &self,
    ) -> Box<dyn crate::core::engine::pull_request_generator::TransitiveUpdatesPullRequestGenerator>
    {
        Box::new(crate::core::engine::pull_request_generator::DefaultTransitiveUpdatesPullRequestGenerator)
    }

    pub fn audit_pull_request_generator(
        &self,
    ) -> Box<dyn crate::core::engine::pull_request_generator::AuditPullRequestGenerator> {
        Box::new(crate::core::engine::pull_request_generator::DefaultAuditPullRequestGenerator)
    }

    pub fn patchers(
        &self,
    ) -> Vec<(
        &'static str,
        Box<dyn crate::core::engine::ecosystems::Patcher>,
    )> {
        vec![
            (
                "npm",
                Box::new(crate::core::engine::ecosystems::npm::NpmPatcher::new(
                    crate::core::clients::npm::Npm::new(self.http_agent.clone()),
                )),
            ),
            // cargo doesn't have a patcher yet in application.rs, but we can add it later if needed
        ]
    }

    pub fn patcher(
        &self,
        ecosystem: &str,
    ) -> Option<Box<dyn crate::core::engine::ecosystems::Patcher>> {
        match ecosystem {
            "npm" => Some(Box::new(
                crate::core::engine::ecosystems::npm::NpmPatcher::new(
                    crate::core::clients::npm::Npm::new(self.http_agent.clone()),
                ),
            )),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct Handle {
    store: Store,

    /// Clients send messages to this mailbox, which are then processed sequentially by
    /// the application.
    mailbox: mpsc::Sender<Message>,

    /// All events that the application generated are broadcast to this channel.
    events: broadcast::Sender<Event>,
}

impl Handle {
    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub fn broadcast(&self, event: Event) -> Result<()> {
        self.events
            .send(event)
            .map_err(|_| anyhow::anyhow!("Failed to broadcast event"))?;
        Ok(())
    }

    pub async fn send(&self, message_id: String, payload: Payload) -> Result<()> {
        let message = Message {
            message_id,
            payload,
        };

        self.mailbox
            .send(message)
            .await
            .map_err(|_| anyhow::anyhow!("Application mailbox unreachable"))?;

        Ok(())
    }
}
