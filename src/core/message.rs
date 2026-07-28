use anyhow::Result;

use super::actions::analyze_project_dependencies::AnalyzedProjectDependencies;
use super::application::Application;

#[derive(Clone)]
pub struct Message {
    /// A client-defined ID of the message.
    ///
    /// Events generated as a consequence of this message may carry this ID. This allows
    /// applicatino clients to correlate events (output) to messages (input).
    pub message_id: String,

    pub payload: Payload,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(tag = "type")]
pub enum Payload {
    /// Broadcast current state of the application to all clients.
    Bootstrap,

    /// Lists all projects and sends an AnalyzeProjectDependencies message for each one.
    AnalyzeAllProjectsDependencies,

    /// Scan the project source code, identify which dependencies it has, check which
    /// dependencies are outdated, and update the local database with the information.
    AnalyzeProjectDependencies {
        project_id: String,
        trigger_bumps: bool,
    },

    /// Remove all but the latest scan for each project, and delete all
    /// unreferenced dependency and package records.
    Vacuum,

    /// Add a new project to the database.
    AddProject {
        platform: String,
        repository: String,
    },

    /// Remove a project from the database.
    RemoveProject {
        platform: String,
        repository: String,
    },

    /// Approve a Bump to be processed. This only updates the database but does not
    /// schedule the actual update task.
    ApproveBump {
        bump_id: String,
    },

    /// Retract previous approval of a bump. This only updates the database but does not
    /// cancel any inflight updates.
    RetractBumpApproval {
        bump_id: String,
    },

    /// For the gien Bump, create (or update) the branch and pull request.
    ProcessBump {
        bump_id: String,
    },

    UpdateTransitiveDependencies {
        project_id: String,
    },

    UpdateVulnerableDependencies {
        project_id: String,
    },

    #[serde(skip)]
    PersistBumpResult {
        bump_id: String,
        pull_request_url: Option<String>,
    },

    /// Clear a bump's stored pull request URL, e.g. because the PR was closed
    /// without being merged.
    #[serde(skip)]
    ClearBumpUrl {
        bump_id: String,
    },

    /// Remove a bump entirely, e.g. because its pull request was merged.
    #[serde(skip)]
    RemoveBump {
        bump_id: String,
    },

    #[serde(skip)]
    PersistAnalyzedProjectDependencies {
        project_id: String,
        scan_result: AnalyzedProjectDependencies,
        trigger_bumps: bool,
    },

    /// Purge all caches.
    Purge,
}

impl std::fmt::Display for Payload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Payload::Bootstrap => write!(f, "Bootstrap"),
            Payload::AnalyzeAllProjectsDependencies => write!(f, "AnalyzeAllProjectsDependencies"),
            Payload::AnalyzeProjectDependencies { .. } => write!(f, "AnalyzeProjectDependencies"),
            Payload::Vacuum => write!(f, "Vacuum"),
            Payload::AddProject { .. } => write!(f, "AddProject"),
            Payload::RemoveProject { .. } => write!(f, "RemoveProject"),
            Payload::ApproveBump { .. } => write!(f, "ApproveBump"),
            Payload::RetractBumpApproval { .. } => write!(f, "RetractBumpApproval"),
            Payload::ProcessBump { .. } => write!(f, "ProcessBump"),
            Payload::UpdateTransitiveDependencies { .. } => {
                write!(f, "UpdateTransitiveDependencies")
            }
            Payload::UpdateVulnerableDependencies { .. } => {
                write!(f, "UpdateVulnerableDependencies")
            }
            Payload::PersistBumpResult { .. } => write!(f, "PersistBumpResult"),
            Payload::ClearBumpUrl { .. } => write!(f, "ClearBumpUrl"),
            Payload::RemoveBump { .. } => write!(f, "RemoveBump"),
            Payload::PersistAnalyzedProjectDependencies { .. } => {
                write!(f, "PersistAnalyzedProjectDependencies")
            }
            Payload::Purge => write!(f, "Purge"),
        }
    }
}

impl Payload {
    pub async fn execute(self, app: &Application) -> Result<()> {
        match self {
            Payload::Bootstrap => {
                let mut ops = Vec::new();

                let projects = app.store().list_projects().await?;
                for project in projects {
                    ops.push(crate::core::event::Op::Upsert {
                        path: format!("project/{}", project.id),
                        data: serde_json::to_value(project).unwrap_or_default(),
                    });
                }

                let dependencies = app.store().list_dependencies().await?;
                for dependency in dependencies {
                    ops.push(crate::core::event::Op::Upsert {
                        path: format!("dependency/{}", dependency.id),
                        data: serde_json::to_value(dependency).unwrap_or_default(),
                    });
                }

                let packages = app.store().list_packages().await?;
                for package in packages {
                    ops.push(crate::core::event::Op::Upsert {
                        path: format!("package/{}", package.id),
                        data: serde_json::to_value(package).unwrap_or_default(),
                    });
                }

                let bumps = app.store().list_bumps().await?;
                for bump in bumps {
                    ops.push(crate::core::event::Op::Upsert {
                        path: format!("bump/{}", bump.id),
                        data: serde_json::to_value(bump).unwrap_or_default(),
                    });
                }

                let bumpdeps = app.store().list_bumpdeps().await?;
                for bumpdep in bumpdeps {
                    ops.push(crate::core::event::Op::Upsert {
                        path: format!("bumpdep/{}/{}", bumpdep.bump_id, bumpdep.dependency_id),
                        data: serde_json::to_value(bumpdep).unwrap_or_default(),
                    });
                }

                app.handle()
                    .broadcast(crate::core::event::Event::Commit { ops })?;

                Ok(())
            }

            Payload::AnalyzeAllProjectsDependencies => {
                let projects = app.store().list_projects().await?;
                for project in projects {
                    app.handle()
                        .send(
                            super::database::pk(),
                            Payload::AnalyzeProjectDependencies {
                                project_id: project.id,
                                trigger_bumps: false,
                            },
                        )
                        .await?;
                }

                Ok(())
            }

            Payload::AnalyzeProjectDependencies {
                project_id,
                trigger_bumps,
            } => {
                super::actions::analyze_project_dependencies::run(app, project_id, trigger_bumps)
                    .await
            }

            Payload::AddProject {
                platform,
                repository,
            } => {
                let project = app.store().add_project(platform, repository).await?;

                app.handle().broadcast(crate::core::event::Event::Commit {
                    ops: vec![crate::core::event::Op::Upsert {
                        path: format!("project/{}", project.id),
                        data: serde_json::to_value(project).unwrap_or_default(),
                    }],
                })?;

                Ok(())
            }

            Payload::RemoveProject {
                platform,
                repository,
            } => {
                let ops = app.store().remove_project(platform, repository).await?;

                app.handle()
                    .broadcast(crate::core::event::Event::Commit { ops })?;

                Ok(())
            }

            Payload::ApproveBump { bump_id } => {
                let updated_bump = app.store().approve_bump(&bump_id).await?;

                app.handle().broadcast(crate::core::event::Event::Commit {
                    ops: vec![crate::core::event::Op::Upsert {
                        path: format!("bump/{}", updated_bump.id),
                        data: serde_json::to_value(updated_bump).unwrap_or_default(),
                    }],
                })?;

                Ok(())
            }

            Payload::RetractBumpApproval { bump_id } => {
                let updated_bump = app.store().retract_bump_approval(&bump_id).await?;

                app.handle().broadcast(crate::core::event::Event::Commit {
                    ops: vec![crate::core::event::Op::Upsert {
                        path: format!("bump/{}", updated_bump.id),
                        data: serde_json::to_value(updated_bump).unwrap_or_default(),
                    }],
                })?;

                Ok(())
            }

            Payload::ProcessBump { bump_id } => {
                super::actions::process_bump::run(app, bump_id).await
            }

            Payload::UpdateTransitiveDependencies { project_id } => {
                super::actions::update_transitive_dependencies::run(app, project_id).await
            }

            Payload::UpdateVulnerableDependencies { project_id } => {
                super::actions::update_vulnerable_dependencies::run(app, project_id).await
            }

            Payload::Vacuum => {
                app.store().vacuum().await?;
                Ok(())
            }

            Payload::PersistBumpResult {
                bump_id,
                pull_request_url,
            } => {
                let updated_bump = app
                    .store()
                    .persist_bump_result(&bump_id, pull_request_url)
                    .await?;

                app.handle().broadcast(crate::core::event::Event::Commit {
                    ops: vec![crate::core::event::Op::Upsert {
                        path: format!("bump/{}", updated_bump.id),
                        data: serde_json::to_value(updated_bump).unwrap_or_default(),
                    }],
                })?;

                Ok(())
            }

            Payload::ClearBumpUrl { bump_id } => {
                let updated_bump = app.store().clear_bump_url(&bump_id).await?;

                app.handle().broadcast(crate::core::event::Event::Commit {
                    ops: vec![crate::core::event::Op::Upsert {
                        path: format!("bump/{}", updated_bump.id),
                        data: serde_json::to_value(updated_bump).unwrap_or_default(),
                    }],
                })?;

                Ok(())
            }

            Payload::RemoveBump { bump_id } => {
                let ops = app.store().remove_bump(&bump_id).await?;

                app.handle()
                    .broadcast(crate::core::event::Event::Commit { ops })?;

                Ok(())
            }

            Payload::PersistAnalyzedProjectDependencies {
                project_id,
                scan_result,
                trigger_bumps,
            } => {
                let ops = app
                    .store()
                    .persist_analyzed_project_dependencies(&project_id, scan_result)
                    .await?;

                app.handle()
                    .broadcast(crate::core::event::Event::Commit { ops })?;

                if trigger_bumps {
                    let bumps = app.store().list_bumps().await?;
                    for bump in bumps
                        .into_iter()
                        .filter(|b| b.project_id == project_id && b.approved)
                    {
                        app.handle()
                            .send(
                                crate::core::database::pk(),
                                Payload::ProcessBump { bump_id: bump.id },
                            )
                            .await
                            .map_err(|e| anyhow::anyhow!(e))?;
                    }
                }
                Ok(())
            }

            Payload::Purge => {
                app.purge_cache().await;
                Ok(())
            }
        }
    }
}
