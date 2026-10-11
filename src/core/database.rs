use std::path::Path;

use anyhow::{Context, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};
use turso::{Builder, Connection};

use crate::core::platform::Platform;

const SCHEMA: &str = include_str!("database/schema.sql");

#[derive(Clone)]
pub struct Database {
    db: turso::Database,
}

impl Database {
    /// Open the database in `data_dir`, creating the directory if necessary.
    pub async fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("Failed to create data directory {}", data_dir.display()))?;

        // Turso only accepts UTF-8 paths. Don't let a lossy conversion point it
        // at a different file than the directory we just created.
        let path = data_dir.join("turso.db");
        let path = path
            .to_str()
            .with_context(|| format!("Data directory {} is not valid UTF-8", data_dir.display()))?;

        let db = Builder::new_local(path)
            .build()
            .await
            .context("Failed to build turso database")?;

        let conn = db.connect().context("Failed to create connection")?;
        let migration = tdsm::plan(&conn, SCHEMA)
            .await
            .context("Failed to plan database migration")?;
        if !migration.is_empty() {
            migration
                .apply()
                .await
                .context("Failed to apply database migration")?;
            tracing::info!("Applied database schema:\n{migration}");
        }

        Ok(Self { db })
    }

    pub fn conn(&self) -> Result<Connection> {
        self.db.connect().context("Failed to create connection")
    }
}

/// Generate a primary key for use in the database. Our primary keys are all
/// 256 bit (32 bytes) random buffers encoded into a string with Base 58.
pub fn pk() -> String {
    let mut buf = [0u8; 32];
    rand::rng().fill_bytes(&mut buf);
    bs58::encode(buf).into_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub platform: Platform,
    pub repository: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scan {
    pub id: String,
    pub project_id: String,
    pub create_time: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependency {
    pub id: String,
    pub scan_id: String,
    pub specifier: Option<String>,
    pub package_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub id: String,
    pub r#type: String,
    pub namespace: Option<String>,
    pub name: String,
    pub version: String,
    pub subpath: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bump {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub major: bool,
    pub approved: bool,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BumpDep {
    pub bump_id: String,
    pub dependency_id: String,
    pub target_version: String,
    pub head_version: String,
    pub minimum_release_age: Option<i64>,
}

/// The kinds of project-wide updates that land in a single pull request, as opposed
/// to a [`Bump`] of specific dependencies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SweepKind {
    /// Update dependencies to fix security vulnerabilities.
    Audit,

    /// Update all transitive dependencies.
    Transitive,
}

impl SweepKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SweepKind::Audit => "audit",
            SweepKind::Transitive => "transitive",
        }
    }

    /// The kind of sweep whose pull request is opened from the given branch, if any.
    pub fn from_branch_name(branch_name: &str) -> Option<Self> {
        [SweepKind::Audit, SweepKind::Transitive]
            .into_iter()
            .find(|kind| kind.branch_name() == branch_name)
    }

    /// The branch that the pull request of this kind is opened from.
    pub fn branch_name(&self) -> &'static str {
        match self {
            SweepKind::Audit => "syz/audit",
            SweepKind::Transitive => "syz/transitive",
        }
    }
}

impl std::str::FromStr for SweepKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "audit" => Ok(SweepKind::Audit),
            "transitive" => Ok(SweepKind::Transitive),
            other => anyhow::bail!("Unsupported pull request kind: {}", other),
        }
    }
}

impl std::fmt::Display for SweepKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sweep {
    pub project_id: String,
    pub kind: SweepKind,
    pub url: String,
}

impl Sweep {
    /// The path under which clients mirror this record.
    pub fn path(project_id: &str, kind: SweepKind) -> String {
        format!("sweep/{}/{}", project_id, kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_kind_round_trips_through_its_branch_name() {
        for kind in [SweepKind::Audit, SweepKind::Transitive] {
            assert_eq!(SweepKind::from_branch_name(kind.branch_name()), Some(kind));
        }
    }

    #[test]
    fn bump_branches_are_not_sweeps() {
        assert_eq!(SweepKind::from_branch_name("syz/update-actix-web"), None);
        assert_eq!(SweepKind::from_branch_name("main"), None);
    }
}
