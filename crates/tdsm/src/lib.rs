//! `tdsm` (Turso Declarative Schema Migration): diff a desired SQL schema
//! against a live Turso database and apply the difference.
//!
//! This first version is intentionally simple: it only ever *adds* things —
//! missing tables, missing columns on existing tables, and missing indexes.
//! Renames, deletions, and column type/constraint changes are out of scope;
//! such differences between the desired schema and the live database are
//! silently ignored rather than acted upon.

mod diff;
mod introspect;
mod parse;

use std::fmt;

use anyhow::{Context, Result};
use turso::Connection;

/// An ordered, human-readable list of DDL statements needed to bring the
/// live database's tables/columns/indexes up to date with the desired
/// schema. Empty when the database is already up to date.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Migration {
    pub statements: Vec<String>,
}

impl Migration {
    pub fn is_empty(&self) -> bool {
        self.statements.is_empty()
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
    Ok(diff::diff(&desired, &live))
}

/// Compute the migration and execute it against `conn`, inside a single
/// transaction. Idempotent: converging an already-up-to-date database
/// returns an empty [`Migration`] and writes nothing.
pub async fn apply(conn: &Connection, desired_schema_sql: &str) -> Result<Migration> {
    let migration = plan(conn, desired_schema_sql).await?;
    if migration.is_empty() {
        return Ok(migration);
    }

    conn.execute("BEGIN TRANSACTION", ())
        .await
        .context("failed to begin migration transaction")?;

    for statement in &migration.statements {
        if let Err(err) = conn.execute(statement, ()).await {
            let _ = conn.execute("ROLLBACK", ()).await;
            return Err(err).with_context(|| format!("failed to execute: {statement}"));
        }
    }

    conn.execute("COMMIT", ())
        .await
        .context("failed to commit migration transaction")?;

    Ok(migration)
}

pub(crate) fn quote_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}
