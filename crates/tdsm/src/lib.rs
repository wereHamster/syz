//! `tdsm` (Turso Declarative Schema Migration): diff a desired SQL schema
//! against a live Turso database and apply the difference.
//!
//! This first version is intentionally simple: it only ever *adds* things —
//! missing tables, missing columns on existing tables, and missing indexes.
//! Renames, deletions, and column type/constraint changes are out of scope
//! by design (not a runtime failure mode): tables/columns/indexes that exist
//! live but are absent from the desired schema are deliberately left alone,
//! not diffed as removals.

mod diff;
mod introspect;
mod parse;

use std::fmt;

use anyhow::{Context, Result};
use turso::Connection;

/// An ordered, human-readable list of DDL statements needed to bring the
/// live database's tables/columns/indexes up to date with the desired
/// schema, bound to the connection it will run against. Empty when the
/// database is already up to date.
#[derive(Clone)]
pub struct Migration {
    pub statements: Vec<String>,
    conn: Connection,
}

impl Migration {
    pub fn is_empty(&self) -> bool {
        self.statements.is_empty()
    }

    /// Execute the migration against the connection it was planned from,
    /// inside a single transaction. Idempotent: applying an empty
    /// [`Migration`] writes nothing.
    pub async fn apply(&self) -> Result<()> {
        if self.is_empty() {
            return Ok(());
        }

        self.conn
            .execute("BEGIN TRANSACTION", ())
            .await
            .context("failed to begin migration transaction")?;

        for statement in &self.statements {
            if let Err(err) = self.conn.execute(statement, ()).await {
                let _ = self.conn.execute("ROLLBACK", ()).await;
                return Err(err).with_context(|| format!("failed to execute: {statement}"));
            }
        }

        self.conn
            .execute("COMMIT", ())
            .await
            .context("failed to commit migration transaction")?;

        Ok(())
    }
}

impl fmt::Display for Migration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for statement in &self.statements {
            writeln!(f, "{statement};")?;
        }
        Ok(())
    }
}

/// Diff the live database against `desired_schema_sql`. Read-only: issues
/// no writes.
pub async fn plan(conn: &Connection, desired_schema_sql: &str) -> Result<Migration> {
    let desired = parse::parse_schema(desired_schema_sql)?;
    let live = introspect::introspect(conn).await?;
    let statements = diff::diff(&desired, &live);
    Ok(Migration {
        statements,
        conn: conn.clone(),
    })
}

pub(crate) fn quote_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}
