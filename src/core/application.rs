use anyhow::Result;
use tokio::sync::{broadcast, mpsc};

use super::clients::github::GitHub;
use super::database::{Database, Project};
use super::engine::ecosystems::{npm::Npm, Ecosystem};
use super::event::Event;
use super::http_agent::HttpAgent;
use super::message::{Message, Payload};

pub struct Application {
    handle: Handle,

    mailbox: mpsc::Receiver<Message>,

    http_agent: HttpAgent,
    github: GitHub,
}

impl Application {
    pub async fn new() -> Result<Self> {
        let (mailbox_tx, mailbox_rx) = mpsc::channel(100);
        let (events, _) = broadcast::channel(1000);

        let database = Database::open().await?;

        let http_agent = HttpAgent::new();
        let github = GitHub::new().await?;

        Ok(Self {
            handle: Handle {
                database,
                mailbox: mailbox_tx,
                events,
            },

            mailbox: mailbox_rx,

            http_agent,
            github,
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

    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }

    pub fn ecosystems(&self) -> Vec<Box<dyn Ecosystem>> {
        vec![Box::new(Npm::new())]
    }

    pub fn query(&self) -> Query {
        self.handle().query()
    }

    pub async fn project_repository_view(
        &self,
        project_id: &str,
    ) -> Result<Box<dyn super::engine::repository::ProjectRepositoryView>> {
        let project = self.query().project(project_id.to_string()).await?;

        match project.platform.as_str() {
            "github" => {
                let parts: Vec<&str> = project.repository.split('/').collect();
                if parts.len() != 2 {
                    anyhow::bail!("Repository must be in the format owner/repo");
                }
                let owner = parts[0].to_string();
                let repo = parts[1].to_string();

                let view = self.github.project_repository_view(owner, repo).await?;
                Ok(Box::new(view))
            }
            _ => {
                anyhow::bail!("Unsupported project platform: {}", project.platform);
            }
        }
    }

    async fn run(mut self) -> Result<()> {
        while let Some(msg) = self.mailbox.recv().await {
            tracing::info!("Processing message {}", msg.message_id);

            if let Err(e) = msg.payload.execute(&self).await {
                tracing::warn!("Failed to process message {}: {}", msg.message_id, e);
            }
        }

        tracing::error!("Application loop is unexpectedly exiting");

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

    pub async fn project(&self, project_id: String) -> Result<Project> {
        let conn = self.database.conn()?;

        let mut stmt = conn
            .prepare("SELECT id, platform, repository FROM project WHERE id = ?1")
            .await?;

        let mut rows = stmt.query((project_id,)).await?;

        if let Some(row) = rows.next().await? {
            return Ok(Project {
                id: row.get(0).unwrap_or_default(),
                platform: row.get(1).unwrap_or_default(),
                repository: row.get(2).unwrap_or_default(),
            });
        }

        Err(anyhow::anyhow!("Project not found"))
    }
}
