use anyhow::Result;
use std::collections::HashSet;
use tokio::sync::oneshot;

use super::actions::analyze_project_dependencies::AnalyzedProjectDependencies;
use super::actions::process_bump::generate_branch_name;
use super::application::Context;
use super::database::{Sweep, SweepKind};
use super::event::{Event, Op};
use super::job::{Job, LaneKey, PublishOutcome};
use super::platform::Platform;
use super::store::Store;

pub struct Message {
    /// A client-defined ID of the message.
    ///
    /// Events generated as a consequence of this message may carry this ID. This allows
    /// applicatino clients to correlate events (output) to messages (input).
    pub message_id: String,

    pub payload: Payload,

    /// Signalled once the main loop has executed the message and enqueued its jobs, whether
    /// it succeeded or not.
    pub processed: Option<oneshot::Sender<()>>,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(tag = "type")]
pub enum Payload {
    /// Broadcast current state of the application to all clients.
    Bootstrap,

    /// Scan every project, as AnalyzeProjectDependencies does.
    AnalyzeAllProjectsDependencies,

    /// Scan the project source code, identify which dependencies it has, check which
    /// dependencies are outdated, and update the local database with the information.
    /// The approved bumps of the project are published afterwards.
    AnalyzeProjectDependencies {
        project_id: String,
    },

    /// Remove all but the latest scan for each project, and delete all
    /// unreferenced dependency and package records.
    Vacuum,

    /// Add a new project to the database.
    AddProject {
        platform: Platform,
        repository: String,
    },

    /// Remove a project from the database, and close its bump and sweep pull requests.
    RemoveProject {
        platform: Platform,
        repository: String,
    },

    /// Approve a Bump to be processed. This only updates the database but does not
    /// schedule the actual update task.
    ApproveBump {
        bump_id: String,
    },

    /// Retract previous approval of a bump. This only updates the database, but a publish of
    /// the bump that is still queued then does nothing.
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

    /// A sweep was published: its pull request is open, or there was nothing to do and it is
    /// closed.
    #[serde(skip)]
    SweepPublished {
        lane: LaneKey,
        project_id: String,
        kind: SweepKind,
        outcome: PublishOutcome,
    },

    /// A sweep's pull request was closed or merged. Forget it, unless the sweep has
    /// meanwhile moved on to a different pull request.
    #[serde(skip)]
    SweepPullRequestClosed {
        pull_request_url: String,
    },

    /// A bump was published: its pull request is open, or there was nothing to do and it is
    /// closed.
    #[serde(skip)]
    BumpPublished {
        lane: LaneKey,
        project_id: String,
        bump_id: String,
        branch_name: String,
        outcome: PublishOutcome,
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

    /// A scan finished. The approved bumps of the project are published afterwards.
    #[serde(skip)]
    PersistAnalyzedProjectDependencies {
        project_id: String,
        scan_result: AnalyzedProjectDependencies,
    },

    /// Close the pull requests of the branches that syz created in the project's repository
    /// and that no bump or sweep tracks, and delete those branches.
    PruneBranches {
        project_id: String,
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
            Payload::SweepPublished { .. } => write!(f, "SweepPublished"),
            Payload::SweepPullRequestClosed { .. } => write!(f, "SweepPullRequestClosed"),
            Payload::BumpPublished { .. } => write!(f, "BumpPublished"),
            Payload::BumpPullRequestClosed { .. } => write!(f, "BumpPullRequestClosed"),
            Payload::PersistAnalyzedProjectDependencies { .. } => {
                write!(f, "PersistAnalyzedProjectDependencies")
            }
            Payload::PruneBranches { .. } => write!(f, "PruneBranches"),
            Payload::Purge => write!(f, "Purge"),
        }
    }
}

impl Payload {
    /// Apply the message to the database and broadcast the change. Returns the jobs that the
    /// message asks for, which the caller dispatches; nothing here talks to the outside world.
    pub async fn execute(self, ctx: &Context) -> Result<Vec<Job>> {
        let store = ctx.store();

        match self {
            Payload::Bootstrap => {
                let mut ops = Vec::new();

                let projects = store.list_projects().await?;
                for project in projects {
                    ops.push(Op::Upsert {
                        path: format!("project/{}", project.id),
                        data: serde_json::to_value(project).unwrap_or_default(),
                    });
                }

                let dependencies = store.list_dependencies().await?;
                for dependency in dependencies {
                    ops.push(Op::Upsert {
                        path: format!("dependency/{}", dependency.id),
                        data: serde_json::to_value(dependency).unwrap_or_default(),
                    });
                }

                let packages = store.list_packages().await?;
                for package in packages {
                    ops.push(Op::Upsert {
                        path: format!("package/{}", package.id),
                        data: serde_json::to_value(package).unwrap_or_default(),
                    });
                }

                let bumps = store.list_bumps().await?;
                for bump in bumps {
                    ops.push(Op::Upsert {
                        path: format!("bump/{}", bump.id),
                        data: serde_json::to_value(bump).unwrap_or_default(),
                    });
                }

                let bumpdeps = store.list_bumpdeps().await?;
                for bumpdep in bumpdeps {
                    ops.push(Op::Upsert {
                        path: format!("bumpdep/{}/{}", bumpdep.bump_id, bumpdep.dependency_id),
                        data: serde_json::to_value(bumpdep).unwrap_or_default(),
                    });
                }

                let sweeps = store.list_sweeps().await?;
                for pull_request in sweeps {
                    ops.push(Op::Upsert {
                        path: Sweep::path(&pull_request.project_id, pull_request.kind),
                        data: serde_json::to_value(pull_request).unwrap_or_default(),
                    });
                }

                ctx.handle().broadcast(Event::Commit { ops })?;

                Ok(Vec::new())
            }

            Payload::AnalyzeAllProjectsDependencies => Ok(store
                .list_projects()
                .await?
                .into_iter()
                .map(|project| Job::Scan {
                    lane: LaneKey::of(&project),
                    project_id: project.id,
                })
                .collect()),

            Payload::AnalyzeProjectDependencies { project_id } => {
                let project = store.project(&project_id).await?;

                Ok(vec![Job::Scan {
                    lane: LaneKey::of(&project),
                    project_id,
                }])
            }

            Payload::AddProject {
                platform,
                repository,
            } => {
                let project = store.add_project(platform, repository).await?;

                ctx.handle().broadcast(Event::Commit {
                    ops: vec![Op::Upsert {
                        path: format!("project/{}", project.id),
                        data: serde_json::to_value(project).unwrap_or_default(),
                    }],
                })?;

                Ok(Vec::new())
            }

            Payload::RemoveProject {
                platform,
                repository,
            } => {
                let Some(project) = store
                    .find_project_by_repository(platform, &repository)
                    .await?
                else {
                    anyhow::bail!("Project not found");
                };

                // Collected before the rows are gone. A pull request that is being opened
                // right now is closed when its outcome finds the record gone.
                let mut branches: Vec<String> = store
                    .list_bumps()
                    .await?
                    .into_iter()
                    .filter(|bump| bump.project_id == project.id && bump.url.is_some())
                    .map(|bump| generate_branch_name(&bump.name, bump.major))
                    .collect();
                branches.sort();
                branches.dedup();
                branches.extend(
                    store
                        .list_sweeps()
                        .await?
                        .into_iter()
                        .filter(|sweep| sweep.project_id == project.id)
                        .map(|sweep| sweep.kind.branch_name().to_string()),
                );

                let ops = store.remove_project(platform, repository).await?;

                ctx.handle().broadcast(Event::Commit { ops })?;

                Ok(close_pull_requests(LaneKey::of(&project), branches))
            }

            Payload::ApproveBump { bump_id } => {
                let updated_bump = store.approve_bump(&bump_id).await?;

                ctx.handle().broadcast(Event::Commit {
                    ops: vec![Op::Upsert {
                        path: format!("bump/{}", updated_bump.id),
                        data: serde_json::to_value(updated_bump).unwrap_or_default(),
                    }],
                })?;

                Ok(Vec::new())
            }

            Payload::RetractBumpApproval { bump_id } => {
                let updated_bump = store.retract_bump_approval(&bump_id).await?;

                ctx.handle().broadcast(Event::Commit {
                    ops: vec![Op::Upsert {
                        path: format!("bump/{}", updated_bump.id),
                        data: serde_json::to_value(updated_bump).unwrap_or_default(),
                    }],
                })?;

                Ok(Vec::new())
            }

            Payload::ProcessBump { bump_id } => {
                let bump = store.bump(&bump_id).await?;
                if !bump.approved {
                    tracing::info!(
                        "Bump {} ({}) is not approved, not publishing it",
                        bump.name,
                        bump_id
                    );
                    return Ok(Vec::new());
                }
                let project = store.project(&bump.project_id).await?;

                Ok(vec![Job::PublishBump {
                    lane: LaneKey::of(&project),
                    bump_id,
                }])
            }

            Payload::UpdateTransitiveDependencies { project_id } => {
                publish_sweep(store, project_id, SweepKind::Transitive).await
            }

            Payload::UpdateVulnerableDependencies { project_id } => {
                publish_sweep(store, project_id, SweepKind::Audit).await
            }

            Payload::UpdateSweeps { project_id } => {
                let project = store.project(&project_id).await?;
                let sweeps = store.list_sweeps().await?;

                Ok(sweep_updates(sweeps, &LaneKey::of(&project), &project_id))
            }

            Payload::Vacuum => {
                let ops = store.vacuum().await?;

                ctx.handle().broadcast(Event::Commit { ops })?;

                Ok(Vec::new())
            }

            Payload::SweepPublished {
                lane,
                project_id,
                kind,
                outcome,
            } => {
                if store.find_project(&project_id).await?.is_none() {
                    // The project was removed while the sweep was being published. Nothing
                    // tracks its pull request now, close it.
                    return Ok(match outcome {
                        PublishOutcome::Opened { .. } => {
                            close_pull_requests(lane, vec![kind.branch_name().to_string()])
                        }
                        PublishOutcome::Closed => Vec::new(),
                    });
                }

                let op = match outcome {
                    PublishOutcome::Opened { url } => {
                        store.set_sweep(&project_id, kind, url).await?
                    }
                    PublishOutcome::Closed => store.clear_sweep(&project_id, kind).await?,
                };

                ctx.handle().broadcast(Event::Commit { ops: vec![op] })?;

                Ok(Vec::new())
            }

            Payload::SweepPullRequestClosed { pull_request_url } => {
                // Looked up here rather than by the sender, so that it is ordered after any
                // SweepPublished that is already queued.
                if let Some(sweep) = store.sweep_by_url(&pull_request_url).await? {
                    let op = store.clear_sweep(&sweep.project_id, sweep.kind).await?;

                    ctx.handle().broadcast(Event::Commit { ops: vec![op] })?;
                }

                Ok(Vec::new())
            }

            Payload::BumpPublished {
                lane,
                project_id,
                bump_id,
                branch_name,
                outcome,
            } => {
                let Some(updated_bump) = store.set_bump_url(&bump_id, outcome.url()).await? else {
                    // The bump was removed (by a rescan, or with its project) while it was
                    // being published, so the removal could not know about its pull request.
                    // Nothing tracks the pull request now, close it.
                    return match outcome {
                        PublishOutcome::Opened { .. } => {
                            close_unused_pull_requests(store, lane, &project_id, vec![branch_name])
                                .await
                        }
                        PublishOutcome::Closed => Ok(Vec::new()),
                    };
                };

                ctx.handle().broadcast(Event::Commit {
                    ops: vec![Op::Upsert {
                        path: format!("bump/{}", updated_bump.id),
                        data: serde_json::to_value(updated_bump).unwrap_or_default(),
                    }],
                })?;

                Ok(Vec::new())
            }

            Payload::BumpPullRequestClosed {
                pull_request_url,
                merged,
            } => {
                // Looked up here rather than by the sender, so that it is ordered after any
                // BumpPublished that is already queued.
                let Some(bump) = store.bump_by_url(&pull_request_url).await? else {
                    tracing::info!("No bump found for pull request {}", pull_request_url);
                    return Ok(Vec::new());
                };

                if merged {
                    let ops = store.remove_bump(&bump.id).await?;

                    ctx.handle().broadcast(Event::Commit { ops })?;
                } else {
                    let updated_bump = store.abandon_bump(&bump.id).await?;

                    ctx.handle().broadcast(Event::Commit {
                        ops: vec![Op::Upsert {
                            path: format!("bump/{}", updated_bump.id),
                            data: serde_json::to_value(updated_bump).unwrap_or_default(),
                        }],
                    })?;
                }

                Ok(Vec::new())
            }

            Payload::PersistAnalyzedProjectDependencies {
                project_id,
                scan_result,
            } => {
                let Some(project) = store.find_project(&project_id).await? else {
                    tracing::info!(
                        "Project {} was removed while it was scanned, dropping the scan",
                        project_id
                    );
                    return Ok(Vec::new());
                };
                let lane = LaneKey::of(&project);

                let (ops, obsolete_bumps) = store
                    .persist_analyzed_project_dependencies(&project_id, scan_result)
                    .await?;

                ctx.handle().broadcast(Event::Commit { ops })?;

                // The rescan dropped these bumps; their pull requests would be left dangling.
                let branches = obsolete_bumps
                    .iter()
                    .map(|bump| generate_branch_name(&bump.name, bump.major))
                    .collect();
                let mut jobs =
                    close_unused_pull_requests(store, lane.clone(), &project_id, branches).await?;

                let bumps = store.list_bumps().await?;
                jobs.extend(
                    bumps
                        .into_iter()
                        .filter(|b| b.project_id == project_id && b.approved)
                        .map(|bump| Job::PublishBump {
                            lane: lane.clone(),
                            bump_id: bump.id,
                        }),
                );

                Ok(jobs)
            }

            Payload::PruneBranches { project_id } => {
                let project = store.project(&project_id).await?;

                Ok(vec![Job::PruneBranches {
                    lane: LaneKey::of(&project),
                    project_id,
                }])
            }

            Payload::Purge => {
                ctx.purge_cache().await;
                Ok(Vec::new())
            }
        }
    }
}

/// The job that publishes the project's sweep of the given kind.
async fn publish_sweep(store: &Store, project_id: String, kind: SweepKind) -> Result<Vec<Job>> {
    let project = store.project(&project_id).await?;

    Ok(vec![Job::PublishSweep {
        lane: LaneKey::of(&project),
        project_id,
        kind,
    }])
}

/// The jobs that re-run the given project's sweeps which have an open pull request.
fn sweep_updates(sweeps: Vec<Sweep>, lane: &LaneKey, project_id: &str) -> Vec<Job> {
    sweeps
        .into_iter()
        .filter(|sweep| sweep.project_id == project_id)
        .map(|sweep| Job::PublishSweep {
            lane: lane.clone(),
            project_id: project_id.to_string(),
            kind: sweep.kind,
        })
        .collect()
}

/// The job that closes the pull requests of the branches, if there are any.
fn close_pull_requests(lane: LaneKey, branches: Vec<String>) -> Vec<Job> {
    if branches.is_empty() {
        return Vec::new();
    }

    vec![Job::ClosePullRequests { lane, branches }]
}

/// The job that closes the pull requests of the branches that none of the project's bumps
/// uses. Different bump names can map to the same branch, and so share a pull request.
async fn close_unused_pull_requests(
    store: &Store,
    lane: LaneKey,
    project_id: &str,
    branches: Vec<String>,
) -> Result<Vec<Job>> {
    let live: HashSet<String> = store
        .list_bumps()
        .await?
        .into_iter()
        .filter(|bump| bump.project_id == project_id)
        .map(|bump| generate_branch_name(&bump.name, bump.major))
        .collect();

    let (kept, unused): (Vec<String>, Vec<String>) = branches
        .into_iter()
        .partition(|branch| live.contains(branch));
    for branch in kept {
        tracing::info!("Keeping the pull request of {}, a bump uses it", branch);
    }

    Ok(close_pull_requests(lane, unused))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::database::{Database, Project};
    use turso::params;

    struct Fixture {
        _dir: tempfile::TempDir,
        store: Store,
        ctx: Context,
        project: Project,
    }

    impl Fixture {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::new(Database::open(dir.path()).await.unwrap());
            let project = store
                .add_project(Platform::GitHub, "owner/repo".to_string())
                .await
                .unwrap();
            let (ctx, _) = Context::for_tests(store.clone());

            Self {
                _dir: dir,
                store,
                ctx,
                project,
            }
        }

        fn lane(&self) -> LaneKey {
            LaneKey::of(&self.project)
        }

        async fn insert_bump(&self, id: &str, name: &str, approved: bool, url: Option<&str>) {
            let conn = self.store.database().conn().unwrap();
            conn.execute(
                "INSERT INTO bump (id, project_id, name, major, approved, url) VALUES (?, ?, ?, 0, ?, ?)",
                params![id, self.project.id.as_str(), name, approved, url],
            )
            .await
            .unwrap();
        }

        async fn execute(&self, payload: Payload) -> Vec<Job> {
            payload.execute(&self.ctx).await.unwrap()
        }
    }

    fn url(n: u32) -> String {
        format!("https://github.com/owner/repo/pull/{}", n)
    }

    fn sweep(project_id: &str, kind: SweepKind) -> Sweep {
        Sweep {
            project_id: project_id.to_string(),
            kind,
            url: format!("https://github.com/owner/{}/pull/1", project_id),
        }
    }

    fn lane() -> LaneKey {
        LaneKey {
            platform: Platform::GitHub,
            repository: "owner/repo".to_string(),
        }
    }

    fn publish_sweep(project_id: &str, kind: SweepKind) -> Job {
        Job::PublishSweep {
            lane: lane(),
            project_id: project_id.to_string(),
            kind,
        }
    }

    #[test]
    fn updates_only_the_kinds_with_an_open_pull_request() {
        let sweeps = vec![sweep("a", SweepKind::Audit)];
        assert_eq!(
            sweep_updates(sweeps, &lane(), "a"),
            vec![publish_sweep("a", SweepKind::Audit)]
        );

        let sweeps = vec![sweep("a", SweepKind::Transitive)];
        assert_eq!(
            sweep_updates(sweeps, &lane(), "a"),
            vec![publish_sweep("a", SweepKind::Transitive)]
        );
    }

    #[test]
    fn updates_both_kinds_when_both_are_open() {
        let sweeps = vec![
            sweep("a", SweepKind::Audit),
            sweep("a", SweepKind::Transitive),
        ];
        assert_eq!(sweep_updates(sweeps, &lane(), "a").len(), 2);
    }

    #[test]
    fn ignores_the_sweeps_of_other_projects() {
        let sweeps = vec![
            sweep("b", SweepKind::Audit),
            sweep("b", SweepKind::Transitive),
        ];
        assert!(sweep_updates(sweeps, &lane(), "a").is_empty());
    }

    #[test]
    fn updates_nothing_without_open_sweeps() {
        assert!(sweep_updates(Vec::new(), &lane(), "a").is_empty());
    }

    #[tokio::test]
    async fn analyzing_all_projects_scans_each_on_its_own_lane() {
        let f = Fixture::new().await;
        let other = f
            .store
            .add_project(Platform::Tangled, "owner/other".to_string())
            .await
            .unwrap();

        let mut jobs = f.execute(Payload::AnalyzeAllProjectsDependencies).await;
        jobs.sort_by_key(|job| job.lane().repository.clone());

        assert_eq!(
            jobs,
            vec![
                Job::Scan {
                    lane: LaneKey::of(&other),
                    project_id: other.id.clone(),
                },
                Job::Scan {
                    lane: f.lane(),
                    project_id: f.project.id.clone(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn processing_a_bump_publishes_it_only_if_approved() {
        let f = Fixture::new().await;
        f.insert_bump("approved", "serde", true, None).await;
        f.insert_bump("pending", "tokio", false, None).await;

        let jobs = f
            .execute(Payload::ProcessBump {
                bump_id: "approved".to_string(),
            })
            .await;
        assert_eq!(
            jobs,
            vec![Job::PublishBump {
                lane: f.lane(),
                bump_id: "approved".to_string(),
            }]
        );

        let jobs = f
            .execute(Payload::ProcessBump {
                bump_id: "pending".to_string(),
            })
            .await;
        assert!(jobs.is_empty());
    }

    #[tokio::test]
    async fn updating_sweeps_publishes_the_open_ones() {
        let f = Fixture::new().await;
        f.store
            .set_sweep(&f.project.id, SweepKind::Audit, url(1))
            .await
            .unwrap();

        let jobs = f
            .execute(Payload::UpdateSweeps {
                project_id: f.project.id.clone(),
            })
            .await;

        assert_eq!(jobs, vec![publish_sweep(&f.project.id, SweepKind::Audit)]);
    }

    #[tokio::test]
    async fn a_published_bump_records_its_pull_request() {
        let f = Fixture::new().await;
        f.insert_bump("bump", "serde", true, None).await;

        let jobs = f
            .execute(Payload::BumpPublished {
                lane: f.lane(),
                project_id: f.project.id.clone(),
                bump_id: "bump".to_string(),
                branch_name: generate_branch_name("serde", false),
                outcome: PublishOutcome::Opened { url: url(1) },
            })
            .await;

        assert!(jobs.is_empty());
        assert_eq!(f.store.bump("bump").await.unwrap().url, Some(url(1)));
    }

    #[tokio::test]
    async fn a_bump_with_nothing_to_publish_forgets_its_closed_pull_request() {
        let f = Fixture::new().await;
        f.insert_bump("bump", "serde", true, Some(&url(1))).await;

        let jobs = f
            .execute(Payload::BumpPublished {
                lane: f.lane(),
                project_id: f.project.id.clone(),
                bump_id: "bump".to_string(),
                branch_name: generate_branch_name("serde", false),
                outcome: PublishOutcome::Closed,
            })
            .await;

        assert!(jobs.is_empty());
        assert_eq!(f.store.bump("bump").await.unwrap().url, None);
    }

    #[tokio::test]
    async fn a_late_bump_publish_closes_the_pull_request_nothing_tracks() {
        let f = Fixture::new().await;
        let branch_name = generate_branch_name("serde", false);

        let jobs = f
            .execute(Payload::BumpPublished {
                lane: f.lane(),
                project_id: f.project.id.clone(),
                bump_id: "gone".to_string(),
                branch_name: branch_name.clone(),
                outcome: PublishOutcome::Opened { url: url(1) },
            })
            .await;
        assert_eq!(
            jobs,
            vec![Job::ClosePullRequests {
                lane: f.lane(),
                branches: vec![branch_name.clone()],
            }]
        );

        let jobs = f
            .execute(Payload::BumpPublished {
                lane: f.lane(),
                project_id: f.project.id.clone(),
                bump_id: "gone".to_string(),
                branch_name,
                outcome: PublishOutcome::Closed,
            })
            .await;
        assert!(jobs.is_empty());
    }

    #[tokio::test]
    async fn a_late_bump_publish_keeps_a_pull_request_another_bump_uses() {
        let f = Fixture::new().await;
        // "Foo@Bar" and "foo---bar" map to the same branch; only the latter exists.
        assert_eq!(
            generate_branch_name("Foo@Bar", false),
            generate_branch_name("foo---bar", false)
        );
        f.insert_bump("live", "foo---bar", false, None).await;

        let jobs = f
            .execute(Payload::BumpPublished {
                lane: f.lane(),
                project_id: f.project.id.clone(),
                bump_id: "gone".to_string(),
                branch_name: generate_branch_name("Foo@Bar", false),
                outcome: PublishOutcome::Opened { url: url(1) },
            })
            .await;

        assert!(jobs.is_empty());
    }

    #[tokio::test]
    async fn a_published_sweep_records_or_forgets_its_pull_request() {
        let f = Fixture::new().await;
        let published = |outcome| Payload::SweepPublished {
            lane: f.lane(),
            project_id: f.project.id.clone(),
            kind: SweepKind::Audit,
            outcome,
        };

        let jobs = f
            .execute(published(PublishOutcome::Opened { url: url(1) }))
            .await;
        assert!(jobs.is_empty());
        assert_eq!(f.store.list_sweeps().await.unwrap().len(), 1);

        let jobs = f.execute(published(PublishOutcome::Closed)).await;
        assert!(jobs.is_empty());
        assert!(f.store.list_sweeps().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_late_sweep_publish_closes_the_pull_request_of_a_removed_project() {
        let f = Fixture::new().await;
        f.execute(Payload::RemoveProject {
            platform: Platform::GitHub,
            repository: "owner/repo".to_string(),
        })
        .await;

        let jobs = f
            .execute(Payload::SweepPublished {
                lane: f.lane(),
                project_id: f.project.id.clone(),
                kind: SweepKind::Transitive,
                outcome: PublishOutcome::Opened { url: url(1) },
            })
            .await;

        assert_eq!(
            jobs,
            vec![Job::ClosePullRequests {
                lane: f.lane(),
                branches: vec!["syz/transitive".to_string()],
            }]
        );
        assert!(f.store.list_sweeps().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn removing_a_project_closes_its_pull_requests() {
        let f = Fixture::new().await;
        f.insert_bump("open", "serde", true, Some(&url(1))).await;
        f.insert_bump("unpublished", "tokio", true, None).await;
        f.store
            .set_sweep(&f.project.id, SweepKind::Audit, url(2))
            .await
            .unwrap();

        let jobs = f
            .execute(Payload::RemoveProject {
                platform: Platform::GitHub,
                repository: "owner/repo".to_string(),
            })
            .await;

        assert_eq!(
            jobs,
            vec![Job::ClosePullRequests {
                lane: f.lane(),
                branches: vec![
                    generate_branch_name("serde", false),
                    "syz/audit".to_string()
                ],
            }]
        );
        assert!(f.store.list_projects().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn removing_a_project_without_pull_requests_runs_nothing() {
        let f = Fixture::new().await;

        let jobs = f
            .execute(Payload::RemoveProject {
                platform: Platform::GitHub,
                repository: "owner/repo".to_string(),
            })
            .await;

        assert!(jobs.is_empty());
    }

    #[tokio::test]
    async fn a_rescan_closes_the_pull_requests_of_dropped_bumps() {
        let f = Fixture::new().await;
        // Dropped by the rescan, which finds no dependencies.
        f.insert_bump("dropped", "serde", true, Some(&url(1))).await;

        let jobs = f
            .execute(Payload::PersistAnalyzedProjectDependencies {
                project_id: f.project.id.clone(),
                scan_result: AnalyzedProjectDependencies {
                    analyzed_project_dependencies: Vec::new(),
                },
            })
            .await;

        assert_eq!(
            jobs,
            vec![Job::ClosePullRequests {
                lane: f.lane(),
                branches: vec![generate_branch_name("serde", false)],
            }]
        );
    }

    #[tokio::test]
    async fn a_rescan_publishes_the_approved_bumps_that_survive() {
        use crate::core::actions::analyze_project_dependencies::AnalyzedProjectDependency;
        use crate::core::engine::{
            DependencyUpdateOption, DiscoveredDependency, PackageInfo, ProposedBump, UpdateType,
            PURL,
        };

        let f = Fixture::new().await;
        f.insert_bump("approved", "serde", true, None).await;
        f.insert_bump("pending", "tokio", false, None).await;

        let analyzed = |name: &str| AnalyzedProjectDependency {
            discovered_dependency: DiscoveredDependency {
                purl: PURL {
                    ecosystem: "cargo".to_string(),
                    namespace: None,
                    name: name.to_string(),
                    subpath: None,
                    version: Some("1.0.0".to_string()),
                },
                requirement: "1".to_string(),
                minimum_release_age: None,
            },
            dependency_update_options: DependencyUpdateOption {
                package_info: PackageInfo { repo_url: None },
                bumps: vec![ProposedBump {
                    target_version: "1.1.0".to_string(),
                    head_version: "1.1.0".to_string(),
                    is_major: false,
                    update_type: UpdateType::Minor,
                }],
            },
        };

        let jobs = f
            .execute(Payload::PersistAnalyzedProjectDependencies {
                project_id: f.project.id.clone(),
                scan_result: AnalyzedProjectDependencies {
                    analyzed_project_dependencies: vec![analyzed("serde"), analyzed("tokio")],
                },
            })
            .await;

        assert_eq!(
            jobs,
            vec![Job::PublishBump {
                lane: f.lane(),
                bump_id: "approved".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn a_scan_of_a_removed_project_is_dropped() {
        let f = Fixture::new().await;
        f.execute(Payload::RemoveProject {
            platform: Platform::GitHub,
            repository: "owner/repo".to_string(),
        })
        .await;

        let jobs = f
            .execute(Payload::PersistAnalyzedProjectDependencies {
                project_id: f.project.id.clone(),
                scan_result: AnalyzedProjectDependencies {
                    analyzed_project_dependencies: Vec::new(),
                },
            })
            .await;

        assert!(jobs.is_empty());
        let conn = f.store.database().conn().unwrap();
        let mut rows = conn.query("SELECT 1 FROM scan", ()).await.unwrap();
        assert!(rows.next().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn vacuuming_broadcasts_what_it_deleted() {
        let f = Fixture::new().await;
        let conn = f.store.database().conn().unwrap();
        for (id, time) in [
            ("old", "2026-01-01T00:00:00Z"),
            ("new", "2026-02-01T00:00:00Z"),
        ] {
            conn.execute(
                "INSERT INTO scan (id, project_id, create_time) VALUES (?, ?, ?)",
                params![id, f.project.id.as_str(), time],
            )
            .await
            .unwrap();
        }
        conn.execute(
            "INSERT INTO package (id, type, name, version) VALUES ('pkg', 'cargo', 'serde', '1.0.0')",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO dependency (id, scan_id, package_id) VALUES ('dep', 'old', 'pkg')",
            (),
        )
        .await
        .unwrap();

        let (ctx, mut events) = Context::for_tests(f.store.clone());
        let jobs = Payload::Vacuum.execute(&ctx).await.unwrap();

        assert!(jobs.is_empty());
        let Event::Commit { ops } = events.try_recv().unwrap() else {
            panic!("expected a commit");
        };
        let mut deleted: Vec<String> = ops
            .into_iter()
            .map(|op| match op {
                Op::Delete { path } => path,
                Op::Upsert { path, .. } => panic!("unexpected upsert of {path}"),
            })
            .collect();
        deleted.sort();
        assert_eq!(deleted, ["dependency/dep", "package/pkg", "scan/old"]);
    }
}
