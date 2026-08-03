# Server-managed Worker Pool Requirements

## Scope

Replace the desktop-configured fixed Built-in tunnel pool with a protocol v3
dynamic pool whose policy is persisted and managed by the tunnel Server Admin
UI. Protocol v2 compatibility is intentionally out of scope.

## Functional requirements

1. The server stores independent MCP and Actions worker policies with these
   fields: start workers, minimum idle workers, maximum idle workers, maximum
   workers, maximum requests per worker, maximum connection lifetime, scale
   down delay, recycle jitter, and revision.
2. A valid policy satisfies
   `1 <= min_idle <= start <= max_idle <= max_workers <= 256`, uses a recycle
   jitter from 0 through 50 percent, and uses bounded request/lifetime/delay
   values.
3. Defaults are start 4, minimum idle 2, maximum idle 4, maximum workers 16,
   maximum requests 500, maximum lifetime 3600 seconds, scale-down delay 60
   seconds, and recycle jitter 10 percent.
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

## Non-functional requirements

- Existing enrollment records, device IDs, and Ed25519 keys remain valid.
- No in-flight request is interrupted for policy scale-down or normal recycle.
- Public requests are never replayed automatically.
- Policy persistence uses additive SQLite schema changes.
