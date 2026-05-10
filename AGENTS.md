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

Slow task (I/O, network) -> spawn background worker (`src/core/actions/`). Read-only.
Background worker finish -> send `Message` to app mailbox.
Main loop -> process `Message` sequential. Mutate DB sync. Avoid race/lock.

## File Map

- `src/bin/syzd.rs` -> Server init.
- `src/bin/syzctl.rs` -> Client init.
- `src/core/application.rs` -> Main actor loop. Mailbox logic.
- `src/core/message.rs` -> Command payloads & execution (`Payload::execute`).
- `src/core/event.rs` -> SSE schema (`Op::Upsert`, `Event::Commit`).
- `src/core/actions/` -> Background async workers.
- `src/server/handlers/` -> HTTP APIs (`POST /messages`, `GET /events`).
- `src/core/engine/ecosystems/` -> Dependency scanner implementations (Cargo, NPM, GitHub Actions). Trait `Ecosystem`.

## Code Rules

Write DB -> only inside main loop (`message.rs` `execute()`).
Network I/O -> only inside background worker (`actions/` or `engine/`).
Mutate state -> emit `Event::Commit` with `Op::Upsert` to sync clients.
Add new ecosystem -> implement `Ecosystem` trait.
