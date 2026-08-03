# Built-in Tunnel Resilience Requirements

> Superseded for worker sizing by `../server-managed-worker-pool/`. This file records the protocol v2 implementation history.

## Scope

Improve the built-in WSS tunnel without changing its version-2 wire protocol or
automatically replaying public HTTP requests.

## Requirements

1. Each workspace service may select 4, 8, or 16 WSS workers. Existing profiles
   and invalid stored values fall back to 4.
2. The desktop reports configured and currently connected worker counts. A live
   supervisor task with zero authenticated workers is `reconnecting`, not
   `running`.
3. Connected workers send WebSocket Ping frames periodically and reconnect when
   no server traffic or Pong is observed before the liveness deadline.
4. The server expires authenticated workers that stop producing traffic, so a
   half-open old connection cannot remain available indefinitely.
5. Reconnect delays use bounded exponential backoff with per-worker jitter and
   reset after an authenticated connection was established.
6. When a public request times out before response headers, the server sends a
   protocol `cancel` to release the assigned desktop worker. Requests are never
   replayed automatically.
7. The behavior is covered by unit and integration tests for defaults, bounds,
   status transitions, jitter, cancellation, disconnect, and reconnect routing.

## Non-goals

- Dynamic wire-level worker negotiation.
- Automatic retry of POST or other non-idempotent requests.
- Changing enrollment or device authentication.
