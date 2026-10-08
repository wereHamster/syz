# tdsm

**Turso Declarative Schema Migration**: diff a desired SQL schema against a
live Turso database, plan the DDL needed to converge, and apply it.

## Motivation

Instead of maintaining an ordered list of versioned migration files, the
schema lives in a single SQL file that is the source of truth. On startup,
[`syz`] feeds that file to this crate and lets the live database converge
toward it. There is no migration bookkeeping: planning is a plain diff
between the desired schema and what actually exists.

## What it does

- [`plan`] parses the desired schema (SQLite dialect), introspects the live
  database (`sqlite_master`, `PRAGMA table_info`), and returns a
  [`Migration`] — an ordered list of DDL statements. Planning is read-only.
- [`Migration::apply`] runs the statements against the connection it was
  planned from, inside a single transaction, rolling back on the first
  failure. Applying an empty migration writes nothing, so it is idempotent.
- `Migration` implements `Display` so the planned SQL can be logged or
  previewed before applying.

The diff is **additive only**:

1. `CREATE TABLE` for missing tables (the statement is used verbatim).
2. `ALTER TABLE ... ADD COLUMN` for missing columns on existing tables.
3. `CREATE INDEX` for missing indexes (implicit `sqlite_autoindex_*`
   indexes are ignored).

Name matching is case-insensitive throughout.

## Scope

By design, this crate only ever *adds* things. Renames, deletions, and
column type/constraint changes are out of scope, and tables, columns, or
indexes that exist live but are absent from the desired schema are left
alone — not diffed as removals. The desired schema may only contain
`CREATE TABLE` and named `CREATE INDEX` statements; anything else is
rejected at parse time.

## Testing

Integration tests in `tests/apply.rs` exercise fresh databases, idempotent
re-application, and schema evolution (adding tables, columns, and indexes).

[`syz`]: ../../README.md
[`plan`]: src/lib.rs
[`Migration`]: src/lib.rs