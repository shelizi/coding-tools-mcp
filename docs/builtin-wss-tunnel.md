# Built-in WSS Tunnel

## Goal

Add a third tunnel choice to Coding Tools MCP without removing the existing FRP and Cloudflare providers.

- `builtin` — built-in WSS client (Rust/Desktop or Node) and dedicated Rust server; default for new workspaces
- `frp` — existing external FRP provider
- `cloudflare` — existing Cloudflare Tunnel provider

Existing workspace data is not migrated automatically: missing legacy tunnel fields continue to deserialize as FRP.

## Public routes

| Purpose | Route |
|---|---|
| WSS negotiation / control / data | `/_tunnel/v1` |
| One-time enrollment | `/_tunnel/enroll/<code>` |
| MCP | `/builtin/clients/<client-id>/mcp` |
| MCP authorization metadata | `/.well-known/oauth-authorization-server/builtin/clients/<client-id>` |
| MCP protected-resource metadata | `/.well-known/oauth-protected-resource/builtin/clients/<client-id>/mcp` |
| Actions | `/builtin/actions/<client-id>` |

The current multiplex wire protocol is version `5`. New clients offer `coding-tools-tunnel-v5-control`, `coding-tools-tunnel-v5`, and legacy `coding-tools-tunnel-v4` on the initial socket. A v5-capable server prefers split control/data mode; a v5 mixed-mode server may select `coding-tools-tunnel-v5`; a v4-only server may select `coding-tools-tunnel-v4`. New servers continue to accept authenticated v4 clients, so server and client upgrades do not need to be perfectly synchronized.

## Components

```text
ChatGPT / public HTTP client
        | HTTPS 443
        v
      Caddy
        |
        v
coding-tools-tunnel-server (Rust / Axum / SQLite)
        | 1 device-scoped Control WSS
        | 1..8 multiplexed Data WSS (default policy)
        | 16 concurrent streams per Data WSS by default
        v
Coding Tools MCP Rust/Desktop or Node client
        | loopback HTTP
        v
local MCP or Actions listener
```

Shared protocol definitions live in `crates/tunnel-protocol`. The server is in `services/tunnel-server`; the desktop/Rust client is in `src-tauri/src/tunnel/builtin.rs`; the Node implementation is in `packages/node-agent/src/tunnel.ts` and `packages/node-agent/src/tunnel/`.

## Enrollment

1. The server administrator runs the local `enroll create` CLI for a Client ID.
2. The CLI creates a random one-time code, stores only its SHA-256 digest, and prints a short-lived HTTPS link.
3. The user pastes the link into the workspace.
4. The client generates an Ed25519 keypair and device ID locally, then stores the private key in its protected local identity store before contacting the server.
5. The server treats the Client ID bound to the enrollment code as authoritative, returns it together with the server's persistent Ed25519 identity (`server_id` plus public key), and stores only the device ID, Client ID, public key, allowed services, timestamps, and revocation state.
6. The client verifies that `server_id = base64url(SHA-256(server_public_key))`, pins that server identity beside the device identity, rebuilds and saves the public route from the server-assigned Client ID, then clears the enrollment link.

An interrupted enrollment response is safely retryable: the same code, device ID, and public key return the original successful result and server-assigned Client ID. A different device cannot reuse the consumed code.

MCP and Actions in the same workspace share one device identity. Pasting a fresh enrollment link after revocation rotates to a new device ID and private key.

## WSS authentication

No shared token, per-client token, or Bearer token is supported. TLS still validates the HTTPS/WSS hostname and CA chain, but the long-lived tunnel binding is an independent application-layer Ed25519 server identity, so ordinary TLS certificate renewal or CA replacement does not break enrollment.

For v5 split mode:

1. The client opens one initial socket and offers `coding-tools-tunnel-v5-control`, `coding-tools-tunnel-v5`, and `coding-tools-tunnel-v4`. No extra physical socket is opened before negotiation finishes.
2. When the server selects `coding-tools-tunnel-v5-control`, that socket becomes the device-scoped Control connection. The server sends a random short-lived nonce, its pinned `server_id`, and an Ed25519 signature over the v5 connection transcript.
3. The client verifies the server ID and signature against the public key pinned during enrollment **before it sends any device proof**.
4. The client signs the split-session authentication payload containing protocol version, connection role, connection ID, optional session ID, and device ID.
5. The server verifies the enrolled device key and returns a signed session acknowledgement containing the authoritative session ID and multiplex connection policy.
6. The Control connection registers logical `(Client ID, service)` routes. Multiple workspaces targeting the same tunnel origin and enrolled device share this Control session and its Data pool; routes remain workspace-specific.
7. Data connections use `coding-tools-tunnel-v5-data`, authenticate against the same device/session binding, and carry multiplexed request/response streams. Stream IDs are session-global and each stream is paired with a route ID.

Legacy v4 retains the original mutually authenticated worker handshake (`challenge` -> `authenticate` -> signed `hello_ack` -> `ready`). New clients fall back to it when the server selects `coding-tools-tunnel-v4`, and new servers continue to serve existing authenticated v4 clients.

A captured authentication response cannot be replayed on another connection because each challenge is random and short-lived. A WSS endpoint with a valid HTTPS certificate but a different application-layer server key is rejected by an already-enrolled client.

In v5 split mode, capacity is device/session scoped rather than one physical worker pool per route. The default multiplex policy is 1 startup Data connection, minimum 1, maximum idle 2, maximum 8, and 16 concurrent streams per Data connection. At 75% utilization the client adds 2 Data connections; queued work or a server-reported demand gap triggers the burst step of at least 4. Excess idle Data connections are retired only after 90 seconds. The Control connection remains exactly one while Data capacity expands and contracts.

The server reports session-wide demand (`queued_requests`, oldest queue wait, active streams, available stream slots, and desired connections). Queue wait is based on real pending-request age rather than a synthetic flag. New work is not assigned while Control is detached.

Control and Data have separate failure domains. Losing one Data connection fails only the streams assigned to that socket. A short Control interruption enters a same-session reconnect grace period: existing Data connections and active streams stay alive, while new assignments wait for Control to rebind. If the grace period expires, clients establish a fresh session, replay surviving logical routes transactionally, then replace stale Data connections. Pending route removals are excluded from rebuild so a workspace being stopped is not resurrected. Shared-workspace ownership can hand off to another workspace without intentionally tearing down the whole device hub.

## Wire messages

v5 Control text frames include session authentication/acknowledgement, route register/unregister, capacity/demand updates, Ping/Pong, and errors. Route lifecycle and session policy stay on Control.

v5 Data text frames carry multiplexed request/response heads, request/response ends, stream-local cancellation, Ping/Pong, and errors. Binary frames carry multiplexed request/response body chunks with route ID + session-global stream ID framing. Request bodies remain bounded to 8 MiB; response paths use bounded channels and backpressure.

Legacy v4 continues to use `challenge`, `authenticate`, `hello_ack`, `policy_update`, `ready`, `request_head`, `request_end`, `response_head`, `response_end`, `cancel`, and `error`, with body chunks in binary frames.

## Device administration

The optional Admin WebUI uses a separate listener, normally `8089`, and is never mounted on the public tunnel router. Deployment keeps this port inside the container network (or bound to loopback) and exposes it only through a private-path reverse proxy when remote access is required.

Admin login uses a username plus password (not a shared Bearer admin key):

- `CODING_TOOLS_TUNNEL_ADMIN_USERNAME`
- `CODING_TOOLS_TUNNEL_ADMIN_PASSWORD_FILE` (preferred) or `CODING_TOOLS_TUNNEL_ADMIN_PASSWORD`
- Password must be at least 12 bytes after trim; the server does **not** auto-generate it
- Successful login creates a random server-side session with a `Secure`, `HttpOnly`, `SameSite=Strict` host-only cookie; mutating requests also require a per-session CSRF token
- Password verification uses Argon2 and is unrelated to device WSS authentication

The WebUI can create enrollment links, list device status, revoke devices, and edit the independent MCP and Actions legacy worker-pool policies. The v5 split multiplex policy is currently the protocol/server default rather than this per-service v4 policy. Local CLI remains available as a recovery and headless-management path:

```text
coding-tools-tunnel-server enroll create --client-id <id> --service both --ttl-seconds 600
coding-tools-tunnel-server devices list
coding-tools-tunnel-server devices revoke --device-id <id>
```

Docker / Compose setup, password file layout, and container CLI examples live in [`services/tunnel-server/README.md`](../services/tunnel-server/README.md).

No management route is exposed on the public `8088` listener.

## Path handling

MCP preserves the complete public path because the local listener exposes the same namespaced MCP and OAuth routes. Actions removes only `/builtin/actions/<client-id>` before forwarding. Partial path-segment matches are rejected.

## Security limits

- HTTPS/WSS only
- strict Client ID and device ID character set
- Ed25519 device keys generated locally
- persistent Ed25519 server identity pinned at enrollment; `server_id` is SHA-256 of the server public key
- server challenge is verified before the client reveals its device proof
- final authentication acknowledgement is signed over the authenticated identity/session transcript and authoritative policy (`hello_ack` for v4, session acknowledgement for v5)
- HTTPS certificate rotation is independent of the pinned server identity
- server stores public keys, not private keys
- one-time enrollment codes stored as SHA-256 digests
- atomic SQLite enrollment consumption and device revocation
- separate opt-in management listener with mandatory Admin username/password (min 12 bytes)
- management login uses Argon2 password verification, random session cookies, and CSRF tokens
- management HTML sets no-store, nosniff, no-referrer, frame denial and a restrictive CSP
- fixed loopback target; server cannot request arbitrary hosts or ports
- hop-by-hop headers removed in both directions
- bounded request, route-command, Data-assignment, and response queues
- stale v4 worker expiry plus v5 stream/control cancellation release unusable or abandoned capacity
- no TCP, UDP, remote-port, P2P, plugins, dashboard, or arbitrary proxy features
- isolated route namespace allows FRP and Built-in to coexist

## Deployment

The public server listens on internal port `8088`. The optional management server listens on a separately configured internal address such as `0.0.0.0:8089`. Prefer binding published ports to loopback or a private Docker network; do not expose Admin on the public internet. Mount the complete `/data` directory as a private named volume; it contains `tunnel.db`, the persistent `server-identity.json` used for application-layer pinning, the most recent 2,000 Admin WebUI log entries, and daily tracing files under `/data/logs`. The identity path can be overridden with `CODING_TOOLS_TUNNEL_SERVER_IDENTITY`. Back up this server identity with the database: replacing or losing it intentionally causes pinned clients to reject the server and requires re-enrollment; replacing only the HTTPS certificate does not.

An optional Compose example (build, secrets, healthcheck, volume) is under `services/tunnel-server/compose.example.yml`. See that directory’s README for step-by-step create/start/enroll commands. Caddy must route these public paths before the FRP fallback:

```text
/_tunnel/v1
/_tunnel/enroll/*
/builtin/*
/.well-known/oauth-authorization-server/builtin/*
/.well-known/oauth-protected-resource/builtin/*
```

## Protocol migration and fallback

Existing enrolled v3 clients do not silently trust a new server key. Their existing device private key is preserved, but they fail closed until a one-time enrollment succeeds and records the application-layer server pin. After that migration, stale enrollment URLs are ignored as before. Server identity rotation remains fail-closed; there is no silent key rollover.

The v4 -> v5 transition is intentionally different: enrollment identity and mutual-auth trust stay valid. New clients offer v5 split, v5 mixed, and v4 on the first WebSocket handshake. A v4-only server can select v4 and continue operating; a new server also accepts a v4-only client. Cross-version regression tests cover both directions.

## Validation

```text
cargo test --manifest-path crates/tunnel-protocol/Cargo.toml
cargo test --manifest-path services/tunnel-server/Cargo.toml
cargo clippy --manifest-path services/tunnel-server/Cargo.toml --all-targets -- -D warnings
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib -- --test-threads=1
pnpm run verify
```

The suites cover actual HTTP enrollment, Ed25519 mutual authentication, public HTTP proxying, revocation rejection, idempotent enrollment retry, multiplex routing, multi-workspace shared sessions, Data scaling, Control rebind/fresh-session recovery, interrupted route replay, pending-removal exclusion, owner handoff, bounded route-queue waits, response cancellation, and cross-version v4 fallback in both directions.

### Enterprise cold-start burst validation (2026-09-07)

The production-like `home-rust` route reproduced the cold-start admission bug with 128 synchronized requests: 65 requests reached the local MCP and returned the expected OAuth `401`, while 63 were rejected immediately as `503` with `X-Tunnel-Error: worker_capacity_exhausted`. The integration regression `cold_burst_waits_for_bounded_queue_capacity_while_workers_scale_up` made the race deterministic: with `max_pending_requests=32`, 96 of 128 requests were rejected before delayed worker offers arrived on the old implementation.

The fix keeps the dispatcher queue bounded while changing admission semantics: when all pending slots are occupied, ingress waits for a released slot under the same `worker_acquire_timeout` deadline. A queue-slot release wakes one waiter; a `503` is emitted only if the shared acquisition deadline really expires. The tunnel-server regression suite passed after the change, including the cold-burst and saturation/deadline cases.

Live server revision `11a7c67bff8f4e641c7036792535baa3a1e9baab` was then deployed to the tunnel container. The first 2,000-request, concurrency-128 cold run demonstrated scale-up from 2 established WSS sockets (Control + 1 Data) to 9 (Control + 8 Data) and eliminated all premature `503`s, but observed 30 transient `502`s. They did not reproduce: a subsequent cold 512/128 run returned 512/512 expected `401`s, and the final body-aware cold 2,000/128 run returned 2,000/2,000 expected `401`s with zero `502`, zero `503`, and zero network errors. A final warm 256/128 run returned 256/256 expected `401`s with zero 5xx responses. After the final burst, the live Rust pool was observed at 9 established WSS sockets and then retired idle Data sockets back to 2 (Control + 1 Data). Rust split-scaling and concurrent-stream focused regressions passed, and the Node multiplex pool planner focused regression passed.

### Cross-device public-route isolation (2026-09-07)

A public `(Client ID, service)` route now has exactly one live device owner. Legacy workers, multiplex-v5 connections, and split-v5 sessions all acquire the same server-side ownership lease before their worker/Data capacity is published. If another authenticated device tries to attach while the current owner still has active capacity, the server rejects it with `route is already owned by another device` instead of placing both devices into one dispatcher pool.

When the previous owner has no active workers left, a different authorized device may take over the public route. The takeover installs a fresh `ClientPool` and atomically updates the public route entry, so pending jobs or stale available-worker offers from the previous owner cannot be consumed by the new device. Device/session authentication and `(route_id, stream_id)` response pairing remain additional isolation layers underneath this ownership boundary.

The integration regression `same_public_route_rejects_second_live_device_owner` reproduced the pre-fix cross-device sharing path and now passes. Focused split-v5 and multiplex-v5 routing tests also pass, and the complete tunnel-server suite passes 50/50 tests.

## Deliberate limitations

- Request bodies are buffered rather than streamed end to end.
- Route entries remain logical session state. If usable Data capacity is unavailable, requests wait only within the configured bounded queue/acquire behavior and otherwise fail rather than creating an unbounded backlog.
- Requests or streams that lose their Data connection are not replayed automatically; callers may retry idempotent operations.
- Public deployment still requires validation in the real reverse-proxy / Docker environment.
