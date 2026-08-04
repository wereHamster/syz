use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use turso::Connection;

use crate::quote_ident;

/// The current shape of a live database, normalized the same way as
/// [`crate::parse::DesiredSchema`] so the two can be compared directly.
pub struct LiveSchema {
    /// Lowercased table names.
    pub tables: HashSet<String>,
    /// Lowercased table name -> lowercased column names.
    pub columns: HashMap<String, HashSet<String>>,
    /// Lowercased index names. Implicit `sqlite_autoindex_*` indexes
    /// (created for inline PRIMARY KEY / UNIQUE constraints) are excluded,
    /// since the desired schema never declares those by name either.
    pub indexes: HashSet<String>,
}

pub async fn introspect(conn: &Connection) -> Result<LiveSchema> {
    let mut tables = HashSet::new();
    let mut indexes = HashSet::new();

    let mut rows = conn
        .query(
            "SELECT name, type FROM sqlite_master WHERE type IN ('table', 'index')",
            (),
        )
        .await
        .context("failed to list existing tables and indexes")?;

    while let Some(row) = rows.next().await? {
        let name: String = row.get(0).context("sqlite_master.name")?;
        let kind: String = row.get(1).context("sqlite_master.type")?;
        match kind.as_str() {
            "table" => {
                tables.insert(name.to_lowercase());
            }
            "index" if !name.starts_with("sqlite_autoindex_") => {
                indexes.insert(name.to_lowercase());
            }
            _ => {}
        }
    }

    let mut columns = HashMap::new();
    for table in &tables {
        let mut column_rows = conn
            .query(&format!("PRAGMA table_info({})", quote_ident(table)), ())
            .await
            .with_context(|| format!("failed to introspect columns of table {table}"))?;

        let mut table_columns = HashSet::new();
        while let Some(row) = column_rows.next().await? {
            let name: String = row.get(1).context("table_info.name")?;
            table_columns.insert(name.to_lowercase());
        }
        columns.insert(table.clone(), table_columns);
    }

    Ok(LiveSchema {
        tables,
        columns,
        indexes,
    })
}
