# Syz Agent Guide

## Stack

Rust workspace. Tokio async. HTTP/SSE for net. Turso DB.

## Architecture

Server (`syzd`) -> Authoritative state. Async actor model.
Client (`syzctl`) -> CLI. Local in-memory materialized view. Eventual consistency.

## Protocol

Command -> `POST /messages`. JSON RPC.
State sync -> `GET /events`. Server-Sent Events (SSE). `Bootstrap` full state, `Commit` delta.
Auth -> Bearer Token. `SYZD_AUTH_TOKEN`.

## Async / Actor Model

Main loop -> reducer. Process `Message` sequential. Mutate DB sync. Broadcast. Return `Vec<Job>`. Avoid race/lock.
Slow task (I/O, network) -> `Job` (`src/core/job.rs`). Never spawn or send to own mailbox from `execute()`.
Dispatcher (`src/core/dispatcher.rs`) -> per-repository lane, FIFO, one job at a time. Per-class caps (audit, general). Timeout. Coalesce duplicate queued jobs.
Job run (`src/core/actions/`) -> read-only store. Finish -> `Ok(Some(outcome Payload))` to mailbox. Fail -> `Err`, lane logs it (log = failure channel to clients).
Late outcome (record gone) -> loop returns `ClosePullRequests` job.

## File Map

- `src/bin/syzd.rs` -> Server init.
- `src/bin/syzctl.rs` -> Client init.
- `src/core/application.rs` -> Main actor loop. Mailbox logic.
- `src/core/message.rs` -> Command payloads & execution (`Payload::execute`).
- `src/core/event.rs` -> SSE schema (`Op::Upsert`, `Event::Commit`).
- `src/core/job.rs` -> `Job`, `LaneKey`, `PublishOutcome`.
- `src/core/dispatcher.rs` -> Lanes, caps, coalescing, timeouts.
- `src/core/services.rs` -> Clonable clients for jobs. `Runner` impl.
- `src/core/actions/` -> Job implementations. Shared publish step (`DefaultBranch::publish`).
- `src/server/handlers/` -> HTTP APIs (`POST /messages`, `GET /events`).
- `src/core/engine/ecosystems/` -> Dependency scanner implementations (Cargo, NPM, GitHub Actions). Trait `Ecosystem`.

## Formatting

Before commit/push -> run `cargo fmt --all`. CI job `Format` runs `cargo fmt --all --check`, fails PR on diff.
Config -> default rustfmt. No `rustfmt.toml`.

## Code Rules

Write DB -> only inside main loop (`message.rs` `execute()`).
Network I/O -> only inside a job (`actions/` or `engine/`).
Mutate state -> emit `Event::Commit` with `Op::Upsert` to sync clients.
Add new ecosystem -> implement `Ecosystem` trait.
