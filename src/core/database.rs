use anyhow::{Context, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};
use turso::{Builder, Connection};

use crate::core::platform::Platform;

#[derive(Clone)]
pub struct Database {
    db: turso::Database,
}

impl Database {
    pub async fn open() -> Result<Self> {
        let db = Builder::new_local("data/turso.db")
            .build()
            .await
            .context("Failed to build turso database")?;

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
