# ADR 0001: Use Actor-style Message Passing for Async Work

## Context

The system executes slow, long-running taasks that involve heavy network and disk I/O across multiple ecosystem APIs (crates.io, npmjs.org, github.org).
These task must not block the main event loop.
Database writes should be executed sequentially.

## Decision

We use an Actor-style message loop to serialize writes.

When a long-running task is triggered, we spawn a concurrent asynchronous task to do the heavy lifting (network, parsing, diffing).
The worker has read-only access to all resources, but it *does not* mutate system state directly.
Once the worker finishes, it bundles the result into a message and sends it back to the application's mailbox.
The main loop processes these messages sequentially, executing the actual state mutations.

## Consequences

- **Non-blocking:** The main event loop remains responsive to new messages while slow network I/O happens in the background.
- **Sequential Mutation:** Database writes and core state mutations happen synchronously within the main loop, avoiding race conditions and complex locking mechanisms.
- **Indirection:** It introduces an artificial asynchronous **seam**. A single logical operation is split across the worker task and the message handler, making the control flow slightly harder to trace.
