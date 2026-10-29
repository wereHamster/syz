use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tracing::Instrument;

use crate::core::platform::{GitHubPlatformAdapter, PlatformRegistry, TangledPlatformAdapter};

use super::clients::github::GitHub;
use super::clients::tangled::Tangled;
use super::database::Database;

use super::dispatcher::{Dispatcher, Limits};
use super::event::Event;
use super::http_agent::HttpAgent;
use super::message::{Message, Payload};
use super::services::Services;
use super::store::Store;

pub struct Application {
    context: Context,

    mailbox: mpsc::Receiver<Message>,

    /// Runs the jobs that messages ask for.
    dispatcher: Dispatcher,

    /// Whether `start` also launches the daily audit of all projects.
    audit_schedule: bool,

    /// Whether the application also logs to stdout.
    stdout_logging: bool,
}

impl Application {
    pub async fn new(data_dir: &Path, work_dir: PathBuf) -> Result<Self> {
        let (mailbox_tx, mailbox_rx) = mpsc::channel(100);
        let (events, _) = broadcast::channel(1000);

        let database = Database::open(data_dir).await?;
        let store = Store::new(database);

        let http_agent = HttpAgent::new();
        let github = GitHub::new().await?;
        let tangled = Tangled::new(work_dir.clone()).await?;

        let mut platforms = PlatformRegistry::new();
        platforms.register(Arc::new(GitHubPlatformAdapter::new(github.clone())));
        platforms.register(Arc::new(TangledPlatformAdapter::new(tangled)));

        let services = Services::new(
            store.clone(),
            http_agent.clone(),
            github,
            platforms,
            work_dir,
        );
        let dispatcher = Dispatcher::new(Arc::new(services), mailbox_tx.clone(), Limits::default());

        Ok(Self {
            context: Context {
                store: store.clone(),
                handle: Handle {
                    store,
                    mailbox: mailbox_tx,
                    events,
                },
                http_agent,
            },

            mailbox: mailbox_rx,
            dispatcher,

            audit_schedule: true,
            stdout_logging: true,
        })
    }

    /// Choose whether `start` launches the daily audit of all projects (on by default).
    pub fn with_audit_schedule(mut self, enabled: bool) -> Self {
        self.audit_schedule = enabled;
        self
    }

    /// Choose whether the application also logs to stdout (on by default). Turn it off when
    /// something else draws on the terminal, the TUI for example.
    pub fn with_stdout_logging(mut self, enabled: bool) -> Self {
        self.stdout_logging = enabled;
        self
    }

    pub fn start(self) -> Handle {
        let handle = self.context.handle.clone();

        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

        let log_layer = super::logger::CoreLogLayer::new(handle.events.clone());

        use tracing_subscriber::layer::SubscriberExt;
        let subscriber = tracing_subscriber::registry()
            .with(filter)
            .with(log_layer)
            .with(self.stdout_logging.then(tracing_subscriber::fmt::layer));
        let dispatch = tracing::Dispatch::new(subscriber);

        use tracing::instrument::WithSubscriber;
        if self.audit_schedule {
            tokio::spawn(super::schedule::run(handle.clone()).with_subscriber(dispatch.clone()));
        }
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
        self.context.handle.clone()
    }

    /// The dispatcher that runs the jobs. It stays usable after `start`, to wait for the jobs
    /// to finish for example.
    pub fn dispatcher(&self) -> Dispatcher {
        self.dispatcher.clone()
    }

    pub fn store(&self) -> &Store {
        &self.context.store
    }

    async fn run(mut self) -> Result<()> {
        while let Some(Message {
            message_id,
            payload,
            processed,
        }) = self.mailbox.recv().await
        {
            let span = tracing::info_span!("message", mid = %message_id);

            async {
                tracing::info!("Processing {:}", payload);

                match payload.execute(&self.context).await {
                    // Enqueued within the message's span, which the jobs then run under.
                    Ok(jobs) => {
                        for job in jobs {
                            self.dispatcher.enqueue(job);
                        }
                    }
                    Err(e) => tracing::warn!("Failed to process message {}: {:#}", message_id, e),
                }
            }
            .instrument(span)
            .await;

            if let Some(processed) = processed {
                // The sender may have stopped waiting.
                let _ = processed.send(());
            }
        }

        tracing::error!("Application loop is unexpectedly exiting");

        Ok(())
    }
}

/// What messages work with while the main loop executes them. Side effects are not part of it:
/// messages return them as jobs instead.
pub struct Context {
    store: Store,
    handle: Handle,
    http_agent: HttpAgent,
}

impl Context {
    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    pub async fn purge_cache(&self) {
        self.http_agent.purge().await;
    }

    /// A context on the given store. Clients of the returned receiver see what is broadcast.
    #[cfg(test)]
    pub fn for_tests(store: Store) -> (Self, broadcast::Receiver<Event>) {
        // Messages don't send to the mailbox, so nothing needs to receive from it.
        let (mailbox, _) = mpsc::channel(1);
        let (events, receiver) = broadcast::channel(1000);

        let context = Self {
            store: store.clone(),
            handle: Handle {
                store,
                mailbox,
                events,
            },
            http_agent: HttpAgent::new(),
        };
        (context, receiver)
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
        // Ignore errors, there may be no subscribers (clients) connected.
        let _ = self.events.send(event);

        Ok(())
    }

    pub async fn send(&self, message_id: String, payload: Payload) -> Result<()> {
        let message = Message {
            message_id,
            payload,
            processed: None,
        };

        self.mailbox
            .send(message)
            .await
            .map_err(|_| anyhow::anyhow!("Application mailbox unreachable"))?;

        Ok(())
    }

    /// Like `send`, but waits until the main loop has executed the message. The jobs it asks
    /// for may still be running.
    pub async fn send_and_wait(&self, message_id: String, payload: Payload) -> Result<()> {
        let (processed, executed) = tokio::sync::oneshot::channel();
        let message = Message {
            message_id,
            payload,
            processed: Some(processed),
        };

        self.mailbox
            .send(message)
            .await
            .map_err(|_| anyhow::anyhow!("Application mailbox unreachable"))?;
        executed
            .await
            .map_err(|_| anyhow::anyhow!("Application loop exited before executing the message"))?;

        Ok(())
    }
}
