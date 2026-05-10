# ADR 0002: Client-Server Protocol

## Context

Syz consists of a server (`syzd`) and CLI client (`syzctl`).
The server operates on an asynchronous actor model (see ADR-0001).
We need a network protocol that naturally bridges this internal architecture to external clients.

## Decision

We will use an **HTTP JSON RPC-like approach for commands**, **Server-Sent Events (SSE) for event streaming**, and **Bearer Token Authentication**.

1. **Protocol:**
   - **Command Submission:** Clients will send `POST /messages` with a JSON body representing the command payload.
   - **Event Streaming:** Clients will connect to `GET /events` using Server-Sent Events (SSE) to receive real-time streams of internal `Event` emissions.
2. **Authentication:** Both endpoints will require an `Authorization: Bearer <token>` header. The server will accept a static, pre-shared secret.
