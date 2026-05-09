use anyhow::Result;
use tokio::sync::{broadcast, mpsc};

use super::database::{Database, Project};
use super::event::Event;
use super::http_agent::HttpAgent;
use super::message::{Message, Payload};

pub struct Application {
    handle: Handle,

    mailbox: mpsc::Receiver<Message>,

    http_agent: HttpAgent,
}

impl Application {
    pub async fn new() -> Result<Self> {
        let (mailbox_tx, mailbox_rx) = mpsc::channel(100);
        let (events, _) = broadcast::channel(1000);

        let database = Database::open().await?;

        let http_agent = HttpAgent::new();

        Ok(Self {
            handle: Handle {
                database,
                mailbox: mailbox_tx,
                events,
            },

            mailbox: mailbox_rx,

            http_agent,
        })
    }

    pub fn start(self) -> Handle {
        let handle = self.handle.clone();

        tokio::spawn(async move {
            tracing::info!("Event loop active");

            if let Err(e) = self.run().await {
                tracing::error!("Application loop exited with error: {}", e);
            }
        });

        handle
    }

    async fn run(mut self) -> Result<()> {
        while let Some(msg) = self.mailbox.recv().await {
            tracing::info!("Processing message {}", msg.message_id);

            match msg.payload {
                Payload::AnalyzeProjectDependencies { project_id } => {
                    tracing::info!("AnalyzeProjectDependencies {}", project_id);
                }
                _ => todo!(),
            }
        }

        Ok(())
    }
}

#[derive(Clone)]
pub struct Handle {
    database: Database,

    /// Clients send messages to this mailbox, which are then processed sequentially by
    /// the application.
    mailbox: mpsc::Sender<Message>,

    /// All events that the application generated are broadcast to this channel.
    events: broadcast::Sender<Event>,
}

impl Handle {
    pub fn query(&self) -> Query {
        Query {
            database: self.database.clone(),
        }
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

pub struct Query {
    database: Database,
}

impl Query {
    pub async fn list_projects(&self) -> Result<Vec<Project>> {
        let conn = self.database.conn()?;

        let mut stmt = conn
            .prepare("SELECT id, platform, repository FROM project")
            .await?;

        let mut rows = stmt.query(()).await?;

        let mut projects = Vec::new();
        while let Some(row) = rows.next().await? {
            projects.push(Project {
                id: row.get(0).unwrap_or_default(),
                platform: row.get(1).unwrap_or_default(),
                repository: row.get(2).unwrap_or_default(),
            });
        }

        Ok(projects)
    }
}
