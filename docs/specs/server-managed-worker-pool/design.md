# Server-managed Worker Pool Design

## Protocol v3

`WorkerPolicy` lives in the shared protocol crate. `hello_ack` carries the
authoritative policy and `policy_update` carries later revisions. The WSS
subprotocol and signed protocol version move directly to v3.

## Persistence and Admin UI

`WorkerPolicyStore` shares the tunnel SQLite database and owns one row per
service. It validates writes, increments revisions transactionally, and
broadcasts successful updates through Tokio watch channels. Admin API routes
read and update policies using the existing authenticated session and CSRF
boundary.

## Server enforcement

The route pool atomically acquires an active-worker slot using the policy
maximum after authentication and before `hello_ack`. Each worker subscribes to
its service policy. While idle it can receive policy broadcasts and forward a
`policy_update` frame. Busy workers receive the newest revision after returning
to idle.

## Desktop pool manager

The manager starts a bootstrap task and tracks workers as connecting, idle,
busy, or retiring. Worker events update shared telemetry. Reconciliation grows
to satisfy startup/minimum-idle policy and selects only idle workers for
scale-down. A delayed reconciliation prevents oscillation.

Each worker calculates a deterministic jittered request limit from its worker
identity. Request and lifetime retirement are checked only at idle boundaries.
The reconnect loop exits for a planned retirement; unexpected disconnects keep
the existing bounded jittered reconnect behavior.

## Migration

The change intentionally drops protocol v2. Old serialized
`builtin_worker_count` fields are ignored by serde after the model field is
removed. Enrollment and device tables are unchanged.
