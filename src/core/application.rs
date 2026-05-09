use anyhow::{Context, Result};
use std::collections::HashMap;
use tokio::sync::{broadcast, mpsc};
use turso::params;

use crate::core::clients;

use super::actions::analyze_project_dependencies::{
    AnalyzedProjectDependencies, AnalyzedProjectDependency,
};
use super::clients::github::GitHub;
use super::database::{pk, Database, Project};
use super::engine::ecosystems::{cargo::Cargo, npm::Npm, Ecosystem};
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
                tracing::error!("Application loop exited with error: {:#}", e);
            }
        });

        handle
    }

    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }

    pub fn ecosystems(&self) -> Vec<Box<dyn Ecosystem>> {
        vec![
            Box::new(Cargo::new(clients::crates::Crates::new(
                self.http_agent.clone(),
            ))),
            Box::new(Npm::new(clients::npm::Npm::new(self.http_agent.clone()))),
        ]
    }

    pub fn query(&self) -> Query {
        self.handle().query()
    }

    pub async fn project_repository_view(
        &self,
        project_id: &str,
    ) -> Result<Box<dyn super::engine::repository::ProjectRepositoryView>> {
        let project = self.query().project(project_id).await?;

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

    pub async fn project_repository_snapshot(
        &self,
        project_id: &str,
    ) -> Result<Box<dyn super::engine::repository::ProjectRepositorySnapshot>> {
        let view = self.project_repository_view(&project_id).await?;
        let default_branch = view.get_default_branch().await?;
        let revision = view.get_revision(default_branch.as_str()).await?;
        Ok(view.snapshot(revision.as_str()))
    }

    pub async fn persist_analyzed_project_pependencies(
        &self,
        project_id: &str,
        scan_result: AnalyzedProjectDependencies,
    ) -> Result<()> {
        let scan_id = pk();
        let now = chrono::Utc::now().to_rfc3339();

        let conn = self.handle.database.conn()?;

        conn.execute(
            "INSERT INTO scan (id, project_id, create_time) VALUES (?, ?, ?)",
            params![scan_id.clone(), project_id.to_string(), now],
        )
        .await
        .context("Failed to insert scan")?;

        let mut success_count = 0;

        let mut existing_bumps_query = conn
            .query(
                "SELECT id, name, major FROM bump WHERE project_id = ?",
                params![project_id.to_string()],
            )
            .await?;
        let mut existing_bumps = HashMap::new();
        let mut bump_ids_to_wipe = Vec::new();
        while let Some(row) = existing_bumps_query.next().await? {
            let id = row.get_value(0)?.as_text().unwrap().to_string();
            let name = row.get_value(1)?.as_text().unwrap().to_string();
            let major = *row.get_value(2)?.as_integer().unwrap_or(&0) != 0;
            existing_bumps.insert((name, major), id.clone());
            bump_ids_to_wipe.push(id);
        }

        for b_id in bump_ids_to_wipe {
            conn.execute("DELETE FROM bumpdep WHERE bump_id = ?", params![b_id])
                .await?;
        }

        let mut bump_cache: HashMap<(String, bool), String> = HashMap::new();

        for res in scan_result.analyzed_project_pependencies {
            let group_name = res.group_name();

            let AnalyzedProjectDependency {
                discovered_dependency,
                dependency_update_options,
            } = res;

            let r#type = &discovered_dependency.purl.ecosystem;
            let namespace = &discovered_dependency.purl.namespace;
            let db_name = &discovered_dependency.purl.name;
            let subpath = &discovered_dependency.purl.subpath;
            let locked_version = &discovered_dependency.purl.version;
            let req = &discovered_dependency.requirement;
            let min_release_age = discovered_dependency.minimum_release_age;

            {
                let mut latest_allowed = "0.0.0".to_string();
                if let Some(first_bump) = dependency_update_options.bumps.first() {
                    latest_allowed = first_bump.target_version.clone();
                }

                let pkg_version = locked_version.clone().unwrap_or(latest_allowed.clone());
                let eco_name = &r#type;
                let mut pkg_query = conn.query(
                    "SELECT id FROM package WHERE type = ? AND namespace IS ? AND name = ? AND subpath IS ? AND version = ?",
                    params![eco_name.to_string(), namespace.clone(), db_name.clone(), subpath.clone(), pkg_version.clone()]
                ).await?;

                let pkg_id = if let Some(row) = pkg_query.next().await? {
                    row.get_value(0)?
                        .as_text()
                        .context("package id should be text")?
                        .clone()
                } else {
                    let new_pkg_id = pk();
                    conn.execute(
                        "INSERT INTO package (id, type, namespace, name, subpath, version) VALUES (?, ?, ?, ?, ?, ?)",
                        params![new_pkg_id.clone(), eco_name.to_string(), namespace.clone(), db_name.clone(), subpath.clone(), pkg_version.clone()]
                    ).await?;
                    new_pkg_id
                };

                let dep_id = pk();
                conn.execute(
                    "INSERT INTO dependency (id, scan_id, specifier, package_id) VALUES (?, ?, ?, ?)",
                    params![dep_id.clone(), scan_id.clone(), req.clone(), pkg_id],
                )
                .await?;

                success_count += 1;

                let mut bumps_to_process = Vec::new();
                for bump in &dependency_update_options.bumps {
                    let bump_version = if discovered_dependency.purl.ecosystem == "github-actions" {
                        bump.target_version.clone()
                    } else {
                        bump.target_version.clone()
                    };
                    bumps_to_process.push((bump_version, bump.is_major, bump.head_version.clone()));
                }

                for (bump_version, bump_is_major, head_ver) in bumps_to_process {
                    let bump_id = if let Some(id) =
                        existing_bumps.get(&(group_name.clone(), bump_is_major))
                    {
                        id.clone()
                    } else if let Some(id) = bump_cache.get(&(group_name.clone(), bump_is_major)) {
                        id.clone()
                    } else {
                        let new_bump_id = pk();
                        conn.execute(
                            "INSERT INTO bump (id, project_id, name, major, approved) VALUES (?, ?, ?, ?, 0)",
                            params![new_bump_id.clone(), project_id.to_string(), group_name.clone(), bump_is_major]
                        ).await?;
                        bump_cache.insert((group_name.clone(), bump_is_major), new_bump_id.clone());
                        new_bump_id
                    };

                    let target_ver = bump_version.clone();
                    let min_age_mins = min_release_age.map(|d| d.num_minutes());

                    conn.execute(
                        "INSERT INTO bumpdep (bump_id, dependency_id, target_version, head_version, minimum_release_age) VALUES (?, ?, ?, ?, ?)",
                        params![bump_id, dep_id.clone(), target_ver, head_ver, min_age_mins]
                    ).await?;
                }
            }
        }

        conn.execute(
            "DELETE FROM bump WHERE project_id = ? AND id NOT IN (SELECT bump_id FROM bumpdep)",
            params![project_id.to_string()],
        )
        .await?;

        let msg = format!(
            "Scan complete. Inserted {} dependencies (found {} potential bumps).",
            success_count,
            bump_cache.len() + existing_bumps.len()
        );
        tracing::info!("{}", msg);

        Ok(())
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

    pub async fn project(&self, project_id: &str) -> Result<Project> {
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
