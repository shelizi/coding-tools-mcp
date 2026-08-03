# Built-in Tunnel Resilience Design

> Superseded for worker sizing by `../server-managed-worker-pool/`. This file records the protocol v2 implementation history.

## Worker capacity

Store `builtin_worker_count` independently for MCP and Actions. The UI exposes
the supported presets 4, 8, and 16. Rust normalizes persisted values before
starting the worker pool.

## Connection health

`BuiltinTunnelHandle` owns shared metrics for configured workers, authenticated
workers, and the most recent error. A connection guard increments the live count
after authentication and decrements it on every exit path.

The desktop is the heartbeat initiator. Each connected worker sends Ping every
15 seconds and treats 45 seconds without inbound traffic as stale. The server
uses the same 45-second inbound deadline while idle and while proxying a job.
Native WebSocket Ping/Pong frames keep the version-2 JSON protocol unchanged.

## Recovery

Each worker reconnects independently. Delay grows to 15 seconds, includes
deterministic per-worker jitter, and resets after any authenticated connection.
The server continues to retain the client route and returns 503 when its live
worker count reaches zero.

## Request cancellation

Each queued proxy job carries a cancellation receiver. If the public response
head deadline expires, the HTTP handler signals cancellation. The assigned
server worker sends `cancel`, the desktop drops the local request future, sends
`Ready`, and becomes available again. No request is reassigned or replayed.
