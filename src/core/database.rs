use anyhow::{Context, Result};
use rand::Rng;
use std::sync::Arc;
use turso::{Builder, Connection};

#[derive(Clone)]
pub struct Database {
    db: Arc<turso::Database>,
}

impl Database {
    pub async fn open() -> Result<Self> {
        let db = Builder::new_local("data/turso.db")
            .build()
            .await
            .context("Failed to build turso database")?;

        Ok(Self { db: Arc::new(db) })
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
