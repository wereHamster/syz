# ADR-0003: Client-Server State Synchronization

## Context

Syz is built as a distributed system.
A single server (`syzd`) manages the authoritative state.
Multiple clients (like `syzctl`) connect to it to query and update this state.
The clients are expected to stay responsive and continuously display accurate, real-time data from the server.

## Decision

We will use an **Event-Driven Event Sourcing-inspired approach** via Server-Sent Events (SSE) combined with an **In-Memory Materialized View** on the client side.

Server-side state is serialized into a AT Protocol-like format (collections, records) and sent to all clients upon any change.
Clients can request a complete view of the state to be sent to the event stream (`Bootstrap` message).
Subsequent changes are automatically broadcast by the server as atomic `Commit` events.

Clients materialize the state into an in-memory data structures (eg. hash maps) and render the UI using that local data.
This decouples UI rendering from network.
As new `Commit` events arrive, the client redraws the UI as needed.

## Consequences

- **Instantaneous UI**: Client views load and swap instantly because the data is already in memory.
- **Real-Time Push**: State changes naturally flow to the clients. If a background job updates the database, that state change is immediately pushed to all clients.
- **Low Network Overhead**: After the initial Bootstrap, only tiny `Commit` delta payloads flow over the wire.
- **Increased Memory Consumption**: If the server state grows extremely large, the client will consume significant RAM since it blindly caches everything. Given our expected domain, this scale is unlikely to be problematic in the near term.
- **Eventual Consistency**: The local client state is fundamentally eventually consistent. A command sent by the client might succeed, but the client won't see the state update until the round-trip `Commit` event is processed.
