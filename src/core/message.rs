use anyhow::Result;

use super::actions::analyze_project_dependencies::AnalyzedProjectDependencies;
use super::application::Application;
use super::database::{Sweep, SweepKind};
use super::platform::Platform;

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
    /// With `trigger_bumps`, the approved bumps of the project are processed afterwards.
    AnalyzeProjectDependencies {
        project_id: String,
        trigger_bumps: bool,
    },

    /// Remove all but the latest scan for each project, and delete all
    /// unreferenced dependency and package records.
    Vacuum,

    /// Add a new project to the database.
    AddProject {
        platform: Platform,
        repository: String,
    },

    /// Remove a project from the database.
    RemoveProject {
        platform: Platform,
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

    /// Re-run every sweep of the project that has an open pull request, so that it follows
    /// the default branch. Sweeps without one are only run on demand.
    #[serde(skip)]
    UpdateSweeps {
        project_id: String,
    },

    /// Record the outcome of a sweep: the URL of its open pull
    /// request, or `None` if it has none (anymore).
    #[serde(skip)]
    PersistSweep {
        project_id: String,
        kind: SweepKind,
        pull_request_url: Option<String>,
    },

    /// A sweep's pull request was closed or merged. Forget it, unless the sweep has
    /// meanwhile moved on to a different pull request.
    #[serde(skip)]
    SweepPullRequestClosed {
        pull_request_url: String,
    },

    #[serde(skip)]
    PersistBumpResult {
        bump_id: String,
        pull_request_url: Option<String>,
    },

    /// A bump's pull request was closed. A merged pull request removes the bump; one that
    /// was closed without merging clears the bump's URL and retracts its approval, since
    /// closing signals the change is unwanted and it must not be reprocessed until approved
    /// again.
    #[serde(skip)]
    BumpPullRequestClosed {
        pull_request_url: String,
        merged: bool,
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
            Payload::UpdateSweeps { .. } => write!(f, "UpdateSweeps"),
            Payload::PersistSweep { .. } => write!(f, "PersistSweep"),
            Payload::SweepPullRequestClosed { .. } => write!(f, "SweepPullRequestClosed"),
            Payload::PersistBumpResult { .. } => write!(f, "PersistBumpResult"),
            Payload::BumpPullRequestClosed { .. } => write!(f, "BumpPullRequestClosed"),
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

                let sweeps = app.store().list_sweeps().await?;
                for pull_request in sweeps {
                    ops.push(crate::core::event::Op::Upsert {
                        path: Sweep::path(&pull_request.project_id, pull_request.kind),
                        data: serde_json::to_value(pull_request).unwrap_or_default(),
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

            Payload::UpdateSweeps { project_id } => {
                let sweeps = app.store().list_sweeps().await?;
                for payload in sweep_updates(sweeps, &project_id) {
                    app.handle()
                        .send(crate::core::database::pk(), payload)
                        .await
                        .map_err(|e| anyhow::anyhow!(e))?;
                }

                Ok(())
            }

            Payload::Vacuum => {
                app.store().vacuum().await?;
                Ok(())
            }

            Payload::PersistSweep {
                project_id,
                kind,
                pull_request_url,
            } => {
                let op = match pull_request_url {
                    Some(url) => app.store().set_sweep(&project_id, kind, url).await?,
                    None => app.store().clear_sweep(&project_id, kind).await?,
                };

                app.handle()
                    .broadcast(crate::core::event::Event::Commit { ops: vec![op] })?;

                Ok(())
            }

            Payload::SweepPullRequestClosed { pull_request_url } => {
                // Looked up here rather than by the sender, so that it is ordered after any
                // PersistSweep that is already queued.
                if let Some(sweep) = app.store().sweep_by_url(&pull_request_url).await? {
                    let op = app
                        .store()
                        .clear_sweep(&sweep.project_id, sweep.kind)
                        .await?;

                    app.handle()
                        .broadcast(crate::core::event::Event::Commit { ops: vec![op] })?;
                }

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

            Payload::BumpPullRequestClosed {
                pull_request_url,
                merged,
            } => {
                // Looked up here rather than by the sender, so that it is ordered after any
                // PersistBumpResult that is already queued.
                let Some(bump) = app.store().bump_by_url(&pull_request_url).await? else {
                    tracing::info!("No bump found for pull request {}", pull_request_url);
                    return Ok(());
                };

                if merged {
                    let ops = app.store().remove_bump(&bump.id).await?;

                    app.handle()
                        .broadcast(crate::core::event::Event::Commit { ops })?;
                } else {
                    let updated_bump = app.store().abandon_bump(&bump.id).await?;

                    app.handle().broadcast(crate::core::event::Event::Commit {
                        ops: vec![crate::core::event::Op::Upsert {
                            path: format!("bump/{}", updated_bump.id),
                            data: serde_json::to_value(updated_bump).unwrap_or_default(),
                        }],
                    })?;
                }

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

/// The messages that re-run the given project's sweeps which have an open pull request.
fn sweep_updates(sweeps: Vec<Sweep>, project_id: &str) -> Vec<Payload> {
    sweeps
        .into_iter()
        .filter(|sweep| sweep.project_id == project_id)
        .map(|sweep| match sweep.kind {
            SweepKind::Audit => Payload::UpdateVulnerableDependencies {
                project_id: project_id.to_string(),
            },
            SweepKind::Transitive => Payload::UpdateTransitiveDependencies {
                project_id: project_id.to_string(),
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sweep(project_id: &str, kind: SweepKind) -> Sweep {
        Sweep {
            project_id: project_id.to_string(),
            kind,
            url: format!("https://github.com/owner/{}/pull/1", project_id),
        }
    }

    #[test]
    fn updates_only_the_kinds_with_an_open_pull_request() {
        let sweeps = vec![sweep("a", SweepKind::Audit)];
        let updates = sweep_updates(sweeps, "a");
        assert!(matches!(
            updates.as_slice(),
            [Payload::UpdateVulnerableDependencies { project_id }] if project_id == "a"
        ));

        let sweeps = vec![sweep("a", SweepKind::Transitive)];
        let updates = sweep_updates(sweeps, "a");
        assert!(matches!(
            updates.as_slice(),
            [Payload::UpdateTransitiveDependencies { project_id }] if project_id == "a"
        ));
    }

    #[test]
    fn updates_both_kinds_when_both_are_open() {
        let sweeps = vec![
            sweep("a", SweepKind::Audit),
            sweep("a", SweepKind::Transitive),
        ];
        assert_eq!(sweep_updates(sweeps, "a").len(), 2);
    }

    #[test]
    fn ignores_the_sweeps_of_other_projects() {
        let sweeps = vec![
            sweep("b", SweepKind::Audit),
            sweep("b", SweepKind::Transitive),
        ];
        assert!(sweep_updates(sweeps, "a").is_empty());
    }

    #[test]
    fn updates_nothing_without_open_sweeps() {
        assert!(sweep_updates(Vec::new(), "a").is_empty());
    }
}
