# Server-managed Worker Pool Requirements

## Scope

Replace the desktop-configured fixed Built-in tunnel pool with a protocol v3
dynamic pool whose policy is persisted and managed by the tunnel Server Admin
UI. Protocol v2 compatibility is intentionally out of scope.

## Functional requirements

1. The server stores independent MCP and Actions worker policies with these
   fields: start workers, minimum idle workers, maximum idle workers, maximum
   workers, maximum requests per worker, maximum connection lifetime, scale
   down delay, recycle jitter, pending-request limit, worker-acquire timeout,
   connecting limit/grace, scale-down step, burst-warm floor/window, and revision.
2. A valid policy satisfies
   `1 <= min_idle <= start <= max_idle <= max_workers <= 256`, uses a recycle
   jitter from 0 through 50 percent, and uses bounded request/lifetime/delay
   values.
3. Defaults are start 4, minimum idle 2, maximum idle 4, maximum workers 16,
   maximum requests 500, maximum lifetime 3600 seconds, scale-down delay 60
   seconds, recycle jitter 10 percent, pending limit 32, worker acquisition 10
   seconds, automatic connecting limit, 1-second connecting grace, scale-down
   step 4, automatic burst warm floor, and 120-second burst warm window.
4. Authenticated Admin UI sessions can read and update both policies. Mutations
   require the existing CSRF protection and invalid policy input returns 400.
5. Every authenticated v3 worker receives the current service policy in
   `hello_ack`. A saved policy is pushed to idle connected workers without a
   server restart.
6. The server refuses new authenticated workers once the route-level
   `(client_id, service)` maximum is reached.
7. A desktop starts with one bootstrap worker, learns the server policy, then
   grows until the start/minimum-idle requirements are satisfied.
8. The desktop grows when idle workers fall below the minimum, never exceeds
   maximum workers, and gracefully retires excess idle workers only after the
   scale-down delay.
9. A worker retires after its jittered request limit or connection lifetime.
   It finishes the active request, does not send another `ready`, closes the
   socket, and the pool replaces it when policy capacity requires it.
10. Desktop status exposes connected, idle, busy, configured maximum, policy
    revision, and cumulative recycled worker counts.
11. Workspace forms no longer own or display a Built-in worker-count setting.
12. Pending public requests use a true bounded per-route reservation and return
    explicit retryable 503 responses when the queue is full or acquisition expires.
13. Response-head timeout starts only after a worker is assigned and remains a 504.
14. Assigned requests carry an optional demand hint so the desktop may grow in
    bounded chunks rather than one worker at a time.
15. Connecting workers count as expected reserve only during grace; after grace
    they no longer suppress fresh capacity and are not killed solely for latency.
16. Scale-down is staged and retains a recent-burst warm floor before returning
    to normal maximum idle.
17. Admin observability exposes queue depth, wait, rejections, and acquisition
    timeouts; a reusable authenticated load-test script reports capacity outcomes.
18. `exec_many mode=auto` must apply dependencies and hard safety/resource-lock
    rules before consulting historical statistics; statistics cannot bypass locks.
19. Parallel history stores only normalized command-family signatures, hashed
    working-directory scope, outcome class, overlap, and lock-wait metrics.
20. Evidence-required pairs stay sequential until at least five safety samples
    reach an 80-percent Wilson lower confidence bound of 0.70.
21. Repeated conflicts force sequential scheduling, while high resource-lock
    serialization reduces recommended parallelism.
22. Tool-usage analysis exposes pair evidence and LLM recommendations. A learned
    contextual policy remains out of scope until labels and sample volume mature.

## Non-functional requirements

- Existing enrollment records, device IDs, and Ed25519 keys remain valid.
- No in-flight request is interrupted for policy scale-down or normal recycle.
- Public requests are never replayed automatically.
- Policy persistence uses additive SQLite schema changes.
