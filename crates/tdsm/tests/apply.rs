use std::path::Path;

async fn memory_conn() -> turso::Connection {
    let db = turso::Builder::new_local(":memory:")
        .build()
        .await
        .expect("failed to build in-memory turso database");
    db.connect().expect("failed to open connection")
}

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("failed to read {path:?}: {err}"))
}

#[tokio::test]
async fn apply_creates_all_tables_from_empty_database() {
    let conn = memory_conn().await;
    let schema = fixture("schema.sql");

    let migration = tdsm::apply(&conn, &schema).await.unwrap();
    assert!(!migration.is_empty());

    for table in [
        "project",
        "scan",
        "dependency",
        "package",
        "bump",
        "bumpdep",
    ] {
        let mut rows = conn
            .query(&format!("SELECT count(*) FROM {table}"), ())
            .await
            .unwrap_or_else(|err| panic!("table {table} should exist and be queryable: {err}"));
        assert!(rows.next().await.unwrap().is_some());
    }
}

#[tokio::test]
async fn apply_is_idempotent() {
    let conn = memory_conn().await;
    let schema = fixture("schema.sql");

    let first = tdsm::apply(&conn, &schema).await.unwrap();
    assert!(!first.is_empty());

    let second = tdsm::apply(&conn, &schema).await.unwrap();
    assert!(
        second.is_empty(),
        "second apply should be a no-op, got: {second}"
    );
}

#[tokio::test]
async fn plan_does_not_mutate_the_database() {
    let conn = memory_conn().await;
    let schema = fixture("schema.sql");

    let migration = tdsm::plan(&conn, &schema).await.unwrap();
    assert!(!migration.is_empty());

    let mut rows = conn
        .query(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let count: i64 = row.get(0).unwrap();
    assert_eq!(count, 0, "plan() must not create anything");

    let migration_again = tdsm::plan(&conn, &schema).await.unwrap();
    assert_eq!(migration.statements, migration_again.statements);
}

#[tokio::test]
async fn apply_adds_missing_column_and_preserves_existing_rows() {
    let conn = memory_conn().await;
    let base_schema = "CREATE TABLE widgets (id text PRIMARY KEY, name text NOT NULL);";
    tdsm::apply(&conn, base_schema).await.unwrap();

    conn.execute(
        "INSERT INTO widgets (id, name) VALUES ('1', 'sprocket')",
        (),
    )
    .await
    .unwrap();

    let evolved_schema =
        "CREATE TABLE widgets (id text PRIMARY KEY, name text NOT NULL, description text);";
    let migration = tdsm::apply(&conn, evolved_schema).await.unwrap();
    assert_eq!(migration.statements.len(), 1);
    assert!(migration.statements[0]
        .to_uppercase()
        .contains("ADD COLUMN"));

    let mut rows = conn
        .query("SELECT id, name, description FROM widgets", ())
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let id: String = row.get(0).unwrap();
    let name: String = row.get(1).unwrap();
    let description: Option<String> = row.get(2).unwrap();
    assert_eq!(id, "1");
    assert_eq!(name, "sprocket");
    assert_eq!(description, None);

    // Applying again is a no-op.
    let second = tdsm::apply(&conn, evolved_schema).await.unwrap();
    assert!(second.is_empty());
}

#[tokio::test]
async fn apply_creates_missing_index() {
    let conn = memory_conn().await;
    let schema = fixture("with_index.sql");

    let migration = tdsm::apply(&conn, &schema).await.unwrap();
    assert!(migration
        .statements
        .iter()
        .any(|s| s.to_uppercase().contains("CREATE INDEX")));

    let mut rows = conn
        .query(
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = 'widgets_owner_id'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let count: i64 = row.get(0).unwrap();
    assert_eq!(count, 1);

    // Re-applying is a no-op for the index too.
    let second = tdsm::apply(&conn, &schema).await.unwrap();
    assert!(second.is_empty());
}

#[tokio::test]
async fn apply_ignores_things_removed_from_the_desired_schema() {
    let conn = memory_conn().await;
    let wide_schema = fixture("with_index.sql");
    tdsm::apply(&conn, &wide_schema).await.unwrap();

    // A narrower desired schema (no index, and could drop a column/table in
    // principle) must not drop anything that already exists.
    let narrow_schema =
        "CREATE TABLE widgets (id text PRIMARY KEY, owner_id text, name text NOT NULL);";
    let migration = tdsm::apply(&conn, narrow_schema).await.unwrap();
    assert!(
        migration.is_empty(),
        "removing something from the desired schema must not produce DDL: {migration}"
    );

    let mut rows = conn
        .query(
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = 'widgets_owner_id'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let count: i64 = row.get(0).unwrap();
    assert_eq!(
        count, 1,
        "index should still exist even though it's no longer in the desired schema"
    );
}

#[tokio::test]
async fn comment_only_edit_produces_empty_migration() {
    let conn = memory_conn().await;
    let schema_a = "-- original comment\nCREATE TABLE widgets (id text PRIMARY KEY);";
    tdsm::apply(&conn, schema_a).await.unwrap();

    let schema_b = "-- a very different comment\nCREATE TABLE widgets (id text PRIMARY KEY);";
    let migration = tdsm::plan(&conn, schema_b).await.unwrap();
    assert!(migration.is_empty());
}

#[tokio::test]
async fn schema_fixture_reflects_pk_and_nullability_from_source() {
    let conn = memory_conn().await;
    let schema = fixture("schema.sql");
    tdsm::apply(&conn, &schema).await.unwrap();

    // bumpdep has no PRIMARY KEY at all in the reference schema.
    let mut rows = conn.query("PRAGMA table_info(bumpdep)", ()).await.unwrap();
    let mut any_pk = false;
    while let Some(row) = rows.next().await.unwrap() {
        let pk: i64 = row.get(5).unwrap();
        if pk > 0 {
            any_pk = true;
        }
    }
    assert!(!any_pk, "bumpdep should have no primary key column");

    // scan.project_id is a nullable FK column (no NOT NULL in the source).
    let mut rows = conn.query("PRAGMA table_info(scan)", ()).await.unwrap();
    let mut project_id_notnull = None;
    while let Some(row) = rows.next().await.unwrap() {
        let name: String = row.get(1).unwrap();
        if name == "project_id" {
            project_id_notnull = Some(row.get::<i64>(3).unwrap());
        }
    }
    assert_eq!(
        project_id_notnull,
        Some(0),
        "scan.project_id should be nullable per the reference schema"
    );
}
