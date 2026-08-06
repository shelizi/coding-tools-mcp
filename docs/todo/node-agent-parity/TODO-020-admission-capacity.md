<!-- parity-id: NP-020 -->
<!-- parity-status: done -->
# NP-020 — Global and workspace admission capacity

- Priority: P1
- Area: runtime
- Status: done

## Gap

Rust applies both global and workspace-local blocking/process admission and defaults to 128/64 local, 1024/512 global, plus 512 active sessions. Node has one runtime-local semaphore layer with lower defaults of 32/16 and 128 active sessions.

## Rust evidence

- `src-tauri/src/tools/context.rs`
- `src-tauri/src/tools/hub.rs`
- `src-tauri/src/tools/session.rs`

## Node current state

- `packages/node-agent/src/runtime.ts`
- `packages/node-agent/src/server.ts`
- `packages/node-agent/src/tools.ts`
- `packages/node-agent/src/types.ts`
- `packages/node-agent/test/admissionCapacity.test.mjs`
- `packages/node-agent/test/processLifecycle.test.mjs`

## Required implementation

Model global capacity separately from folder-local capacity, use both gates for every admitted operation, report both waits and limits, and align defaults without weakening configurable bounds. Shared HTTP and built-in WSS requests must consume the same global capacity.

## Acceptance checklist

- [x] Blocking and process operations acquire global and folder-local admission in a deadlock-safe order.
- [x] Defaults match Rust: 128/64 local, 1024/512 global, and 512 active sessions.
- [x] Configuration bounds and management reporting include both capacity layers.
- [x] Queue-wait telemetry distinguishes global, workspace, and blocking/process lanes.
- [x] Folder A saturation does not consume folder B local capacity beyond the global cap.
- [x] HTTP and built-in WSS transports share the same global gates.
- [x] Cancellation releases all acquired permits exactly once.
- [x] `server_info`, process snapshots, and usage records expose consistent limits and waits.

## Dependencies

Requires `NP-018` because workspace-local gates belong to folder-scoped execution resources.

## Verification

Verified with deterministic global-before-workspace acquisition, cancellation waiter cleanup, Rust-compatible configuration defaults and bounds, management reporting, two-folder runtime isolation, shared transport context, separate global/workspace wait telemetry, and the complete Node Agent regression suite.
