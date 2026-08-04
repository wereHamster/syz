use crate::introspect::LiveSchema;
use crate::parse::DesiredSchema;
use crate::quote_ident;

/// Compare `desired` against `live` and produce the additive-only set of
/// statements needed to converge: `CREATE TABLE` for missing tables,
/// `ALTER TABLE ... ADD COLUMN` for missing columns on existing tables, and
/// `CREATE INDEX` for missing indexes. Tables/columns/indexes present in
/// `live` but absent from `desired` are left alone.
pub fn diff(desired: &DesiredSchema, live: &LiveSchema) -> Vec<String> {
    let mut statements = Vec::new();

    for table in &desired.tables {
        if !live.tables.contains(&table.name) {
            statements.push(table.create_sql.clone());
            continue;
        }

        let existing_columns = live.columns.get(&table.name);
        for column in &table.columns {
            let already_present = existing_columns
                .map(|columns| columns.contains(&column.name))
                .unwrap_or(false);

            if !already_present {
                statements.push(format!(
                    "ALTER TABLE {} ADD COLUMN {}",
                    quote_ident(&table.name),
                    column.def_sql
                ));
            }
        }
    }

    for index in &desired.indexes {
        if !live.indexes.contains(&index.name) {
            statements.push(index.create_sql.clone());
        }
    }

    statements
}
