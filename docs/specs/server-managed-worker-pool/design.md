# Server-managed Worker Pool Design

## Protocol v3

`WorkerPolicy` lives in the shared protocol crate. `hello_ack` carries the
authoritative policy and `policy_update` carries later revisions. The WSS
subprotocol and signed protocol version move directly to v3.

## Persistence and Admin UI

`WorkerPolicyStore` shares the tunnel SQLite database and owns one row per
service. It validates writes, increments revisions transactionally, and
broadcasts successful updates through Tokio watch channels. Additive migrations
supply defaults for queue limits, worker-acquire timeout, connecting grace,
scale-down step, and burst-warm controls. Admin API routes read and update all
policy fields using the existing authenticated session and CSRF boundary.

## Server enforcement

The route pool atomically acquires an active-worker slot using the policy
maximum after authentication and before `hello_ack`. Each worker subscribes to
its service policy. While idle it can receive policy broadcasts and forward a
`policy_update` frame. Busy workers receive the newest revision after returning
to idle.

Each route reserves a bounded pending slot before enqueue. Assignment and
response-head completion use separate one-shot channels and deadlines. Queue
capacity or acquisition expiry returns explicit retryable 503 responses;
response-head timeout is only possible after assignment. Dispatcher cleanup
removes abandoned jobs, and observability tracks queue depth/wait and capacity
failures.

At assignment, the server calculates a bounded `WorkerDemand` hint from active
workers and pending demand and includes it in `request_head` without changing
the v3 subprotocol.

## Desktop pool manager

The manager starts a bootstrap task and tracks workers as connecting, idle,
busy, or retiring. Worker and demand events update shared telemetry.
Reconciliation grows in bounded chunks to satisfy startup/minimum-idle and
server demand. Recently started connecting tasks count as expected reserve only
within the configured grace. Older connecting tasks keep their bounded reconnect
loop but stop suppressing fresh capacity, avoiding false retirement on slow networks.

Scale-down selects only idle workers and retires at most `scale_down_step` per
reconciliation. A recent burst retains an automatic or explicit warm floor for
the configured window, then returns gradually to maximum idle.

Each worker calculates a deterministic jittered request limit from its worker
identity. Request and lifetime retirement are checked only at idle boundaries.
The reconnect loop exits for a planned retirement; unexpected disconnects keep
the existing bounded jittered reconnect behavior.

## Explainable parallel scheduling

`exec_many mode=auto` uses a layered decision model rather than a black-box
machine-learning model. Dependency edges select DAG mode first. Opaque shell
commands remain sequential, and inferred or explicit resource locks always
override statistical evidence.

Parallel executions emit bounded observations for command pairs. Signatures
contain a normalized program/verb and a truncated hash of the working directory;
raw command text, arguments, and paths are not persisted in the pair history.
Outcomes distinguish useful overlap, known lock serialization, explicit conflict
markers, unrelated failures, and commands that never overlapped.

Unknown structured command pairs require repeated explicit parallel observations.
Auto mode permits them only after at least five safety samples and an 80-percent
Wilson lower confidence bound of 0.70. Repeated conflicts force sequential mode;
high lock-serialization rates reduce or remove parallelism. Tool-usage reports
expose the evidence and LLM-facing recommendations. A contextual bandit is
deferred until signatures, labels, and sample volume are demonstrably stable.

## Migration

The change intentionally drops protocol v2. Protocol v3 remains wire-compatible
with earlier v3 workers because demand is optional and new policy fields have
serde defaults. Existing worker-policy tables are upgraded with additive SQLite
columns; enrollment and device tables are unchanged.
