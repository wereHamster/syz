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

#[derive(Clone, serde::Deserialize)]
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

    #[serde(skip)]
    PersistBumpResult {
        bump_id: String,
        pull_request_url: Option<String>,
    },

    #[serde(skip)]
    PersistAnalyzedProjectDependencies {
        project_id: String,
        scan_result: AnalyzedProjectDependencies,
    },
}

impl Payload {
    pub async fn execute(&self, app: &Application) -> Result<()> {
        match self {
            Payload::Bootstrap => {
                let mut ops = Vec::new();

                let projects = app.query().list_projects().await?;
                for project in projects {
                    ops.push(crate::core::event::Op::Upsert {
                        path: format!("project/{}", project.id),
                        data: serde_json::to_value(project).unwrap_or_default(),
                    });
                }

                let dependencies = app.query().list_dependencies().await?;
                for dependency in dependencies {
                    ops.push(crate::core::event::Op::Upsert {
                        path: format!("dependency/{}", dependency.id),
                        data: serde_json::to_value(dependency).unwrap_or_default(),
                    });
                }

                let packages = app.query().list_packages().await?;
                for package in packages {
                    ops.push(crate::core::event::Op::Upsert {
                        path: format!("package/{}", package.id),
                        data: serde_json::to_value(package).unwrap_or_default(),
                    });
                }

                let bumps = app.query().list_bumps().await?;
                for bump in bumps {
                    ops.push(crate::core::event::Op::Upsert {
                        path: format!("bump/{}", bump.id),
                        data: serde_json::to_value(bump).unwrap_or_default(),
                    });
                }

                let bumpdeps = app.query().list_bumpdeps().await?;
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
                let projects = app.query().list_projects().await?;
                for project in projects {
                    app.handle()
                        .send(
                            super::database::pk(),
                            Payload::AnalyzeProjectDependencies {
                                project_id: project.id,
                            },
                        )
                        .await?;
                }

                Ok(())
            }

            Payload::AnalyzeProjectDependencies { project_id } => {
                super::actions::analyze_project_dependencies::run(app, project_id.clone()).await
            }

            Payload::ApproveBump { bump_id } => {
                app.approve_bump(bump_id).await?;

                let bump = app.query().bump(bump_id).await?;
                app.handle().broadcast(crate::core::event::Event::Commit {
                    ops: vec![crate::core::event::Op::Upsert {
                        path: format!("bump/{}", bump.id),
                        data: serde_json::to_value(bump).unwrap_or_default(),
                    }],
                })?;

                Ok(())
            }

            Payload::RetractBumpApproval { bump_id } => {
                app.retract_bump_approval(bump_id).await?;

                let bump = app.query().bump(bump_id).await?;
                app.handle().broadcast(crate::core::event::Event::Commit {
                    ops: vec![crate::core::event::Op::Upsert {
                        path: format!("bump/{}", bump.id),
                        data: serde_json::to_value(bump).unwrap_or_default(),
                    }],
                })?;

                Ok(())
            }

            Payload::ProcessBump { bump_id } => {
                super::actions::process_bump::run(app, bump_id.clone()).await
            }

            Payload::UpdateTransitiveDependencies { project_id } => {
                super::actions::update_transitive_dependencies::run(app, project_id.clone()).await
            }

            Payload::PersistBumpResult {
                bump_id,
                pull_request_url,
            } => {
                app.persist_bump_result(bump_id, pull_request_url.clone())
                    .await
            }

            Payload::PersistAnalyzedProjectDependencies {
                project_id,
                scan_result,
            } => {
                app.persist_analyzed_project_dependencies(project_id, scan_result.clone())
                    .await
            }
        }
    }
}
