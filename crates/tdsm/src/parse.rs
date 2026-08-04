use anyhow::{bail, Context, Result};
use sqlparser::ast::{ObjectName, Statement};
use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser;

/// A `CREATE TABLE` statement from the desired schema, normalized enough to
/// diff against a live database.
pub struct DesiredTable {
    /// Lowercased table name, used for matching against the live database.
    pub name: String,
    /// The full `CREATE TABLE ...` statement, used verbatim when the table
    /// doesn't exist yet.
    pub create_sql: String,
    pub columns: Vec<DesiredColumn>,
}

pub struct DesiredColumn {
    /// Lowercased column name, used for matching against the live database.
    pub name: String,
    /// The column definition fragment (`name TYPE constraints...`), used
    /// verbatim in `ALTER TABLE ... ADD COLUMN <def_sql>`.
    pub def_sql: String,
}

/// A `CREATE INDEX` statement from the desired schema.
pub struct DesiredIndex {
    /// Lowercased index name, used for matching against the live database.
    pub name: String,
    /// The full `CREATE INDEX ...` statement, used verbatim when the index
    /// doesn't exist yet.
    pub create_sql: String,
}

#[derive(Default)]
pub struct DesiredSchema {
    pub tables: Vec<DesiredTable>,
    pub indexes: Vec<DesiredIndex>,
}

pub fn parse_schema(sql: &str) -> Result<DesiredSchema> {
    let statements =
        Parser::parse_sql(&SQLiteDialect {}, sql).context("failed to parse desired schema SQL")?;

    let object_name = |name: &ObjectName| name.to_string();

    let mut schema = DesiredSchema::default();

    for statement in statements {
        match &statement {
            Statement::CreateTable(create_table) => {
                let name = object_name(&create_table.name);
                let columns = create_table
                    .columns
                    .iter()
                    .map(|column| DesiredColumn {
                        name: column.name.value.to_lowercase(),
                        def_sql: column.to_string(),
                    })
                    .collect();

                schema.tables.push(DesiredTable {
                    name: name.to_lowercase(),
                    create_sql: statement.to_string(),
                    columns,
                });
            }
            Statement::CreateIndex(create_index) => {
                let name = create_index
                    .name
                    .as_ref()
                    .map(object_name)
                    .context("CREATE INDEX without a name is not supported")?;

                schema.indexes.push(DesiredIndex {
                    name: name.to_lowercase(),
                    create_sql: statement.to_string(),
                });
            }
            other => bail!("unsupported statement in desired schema: {other}"),
        }
    }

    Ok(schema)
}
