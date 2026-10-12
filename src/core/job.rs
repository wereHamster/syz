use super::database::{Project, SweepKind};
use super::platform::Platform;

/// Identifies the lane that a job runs on. Jobs against the same repository share a lane, which
/// runs them one at a time, so that they never race each other on the hosting platform.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LaneKey {
    pub platform: Platform,
    pub repository: String,
}

impl LaneKey {
    /// The lane of the project's repository. A project and its repository are one-to-one.
    pub fn of(project: &Project) -> Self {
        Self {
            platform: project.platform,
            repository: project.repository.clone(),
        }
    }
}

impl std::fmt::Display for LaneKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.platform, self.repository)
    }
}

/// External work (network, package managers, git) that a message asks for. Jobs run in the
/// background, outside of the main loop, and report back with an outcome message.
///
/// Jobs may read the store, but never write to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    /// Scan the project and look up which of its dependencies are outdated. The approved
    /// bumps of the project are published afterwards.
    Scan { lane: LaneKey, project_id: String },

    /// Create or update the pull request of the bump, or close it if there is nothing to do.
    /// The bump is read when the job starts, so that it uses the freshest targets, and the
    /// job does nothing if the bump is gone or no longer approved by then.
    PublishBump { lane: LaneKey, bump_id: String },

    /// Create or update the pull request of the project's sweep of the given kind, or close
    /// it if there is nothing to do.
    PublishSweep {
        lane: LaneKey,
        project_id: String,
        kind: SweepKind,
    },

    /// Close the pull requests opened from the given branches, and delete the branches. This
    /// looks nothing up, so that it works after the records owning the branches are gone.
    ClosePullRequests {
        lane: LaneKey,
        branches: Vec<String>,
    },
}

/// Which concurrency cap a job runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobClass {
    /// Audit sweeps, which the daily audit starts for every project at once. They have a cap
    /// of their own, so that they cannot starve the other jobs.
    Audit,

    /// Everything else.
    General,
}

impl Job {
    pub fn lane(&self) -> &LaneKey {
        match self {
            Job::Scan { lane, .. }
            | Job::PublishBump { lane, .. }
            | Job::PublishSweep { lane, .. }
            | Job::ClosePullRequests { lane, .. } => lane,
        }
    }

    pub fn class(&self) -> JobClass {
        match self {
            Job::PublishSweep {
                kind: SweepKind::Audit,
                ..
            } => JobClass::Audit,
            _ => JobClass::General,
        }
    }

    /// Folds `other` into this job if it would duplicate it while this one is still queued.
    /// Returns whether it did, in which case `other` must not be queued.
    ///
    /// A queued job resolves its base only once it starts, so running a duplicate afterwards
    /// would only redo the same work.
    pub fn absorb(&mut self, other: &Job) -> bool {
        match (self, other) {
            (
                Job::Scan { project_id, .. },
                Job::Scan {
                    project_id: other_project_id,
                    ..
                },
            ) => project_id == other_project_id,
            (
                Job::PublishBump { bump_id, .. },
                Job::PublishBump {
                    bump_id: other_bump_id,
                    ..
                },
            ) => bump_id == other_bump_id,
            (
                Job::PublishSweep {
                    project_id, kind, ..
                },
                Job::PublishSweep {
                    project_id: other_project_id,
                    kind: other_kind,
                    ..
                },
            ) => project_id == other_project_id && kind == other_kind,
            // Branch sets can differ, and closing is idempotent.
            _ => false,
        }
    }
}

impl std::fmt::Display for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Job::Scan { lane, .. } => write!(f, "scan of {}", lane),
            Job::PublishBump { lane, bump_id } => {
                write!(f, "publish of bump {} in {}", bump_id, lane)
            }
            Job::PublishSweep { lane, kind, .. } => write!(f, "{} sweep of {}", kind, lane),
            Job::ClosePullRequests { lane, branches } => {
                write!(f, "close of {} in {}", branches.join(", "), lane)
            }
        }
    }
}

/// How a publish ended, when it didn't fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishOutcome {
    /// The pull request with this URL is open and up to date.
    Opened { url: String },

    /// There was nothing to publish, and the pull request (if there was one) is closed.
    Closed,
}

impl PublishOutcome {
    pub fn url(&self) -> Option<&str> {
        match self {
            PublishOutcome::Opened { url } => Some(url),
            PublishOutcome::Closed => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane() -> LaneKey {
        LaneKey {
            platform: Platform::GitHub,
            repository: "owner/repo".to_string(),
        }
    }

    fn scan(project_id: &str) -> Job {
        Job::Scan {
            lane: lane(),
            project_id: project_id.to_string(),
        }
    }

    fn publish_bump(bump_id: &str) -> Job {
        Job::PublishBump {
            lane: lane(),
            bump_id: bump_id.to_string(),
        }
    }

    fn publish_sweep(project_id: &str, kind: SweepKind) -> Job {
        Job::PublishSweep {
            lane: lane(),
            project_id: project_id.to_string(),
            kind,
        }
    }

    fn close(branches: &[&str]) -> Job {
        Job::ClosePullRequests {
            lane: lane(),
            branches: branches.iter().map(|b| b.to_string()).collect(),
        }
    }

    #[test]
    fn a_duplicate_scan_is_dropped() {
        assert!(scan("a").absorb(&scan("a")));
    }

    #[test]
    fn scans_of_different_projects_do_not_merge() {
        assert!(!scan("a").absorb(&scan("b")));
    }

    #[test]
    fn a_duplicate_bump_publish_is_dropped() {
        assert!(publish_bump("x").absorb(&publish_bump("x")));
        assert!(!publish_bump("x").absorb(&publish_bump("y")));
    }

    #[test]
    fn a_duplicate_sweep_publish_is_dropped() {
        assert!(publish_sweep("a", SweepKind::Audit).absorb(&publish_sweep("a", SweepKind::Audit)));
        assert!(!publish_sweep("a", SweepKind::Audit)
            .absorb(&publish_sweep("a", SweepKind::Transitive)));
        assert!(!publish_sweep("a", SweepKind::Audit).absorb(&publish_sweep("b", SweepKind::Audit)));
    }

    #[test]
    fn closes_never_merge() {
        assert!(!close(&["x"]).absorb(&close(&["x"])));
    }

    #[test]
    fn different_kinds_never_merge() {
        assert!(!scan("a").absorb(&publish_sweep("a", SweepKind::Audit)));
        assert!(!publish_bump("x").absorb(&scan("a")));
    }

    #[test]
    fn only_audit_sweeps_run_under_the_audit_cap() {
        assert_eq!(
            publish_sweep("a", SweepKind::Audit).class(),
            JobClass::Audit
        );
        assert_eq!(
            publish_sweep("a", SweepKind::Transitive).class(),
            JobClass::General
        );
        assert_eq!(scan("a").class(), JobClass::General);
        assert_eq!(publish_bump("x").class(), JobClass::General);
        assert_eq!(close(&["x"]).class(), JobClass::General);
    }

    #[test]
    fn describes_the_job_and_its_repository() {
        assert_eq!(scan("a").to_string(), "scan of github:owner/repo");
        assert_eq!(
            publish_bump("x").to_string(),
            "publish of bump x in github:owner/repo"
        );
        assert_eq!(
            publish_sweep("a", SweepKind::Audit).to_string(),
            "audit sweep of github:owner/repo"
        );
        assert_eq!(
            close(&["syz/a", "syz/b"]).to_string(),
            "close of syz/a, syz/b in github:owner/repo"
        );
    }
}
