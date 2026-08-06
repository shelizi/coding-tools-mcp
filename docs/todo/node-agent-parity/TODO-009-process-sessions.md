<!-- parity-id: NP-009 -->
<!-- parity-status: done -->
# NP-009 — Retained process session lifecycle parity

- Priority: P1
- Area: runtime
- Status: done


## Gap

Resolved in Node Agent 0.18.0. Retained commands now follow the Rust session lifecycle contract for interactive stdin, operation reattachment, output pagination, timing, lock metadata, termination recovery, detached cleanup, retention, and Agent shutdown.

## Rust evidence

- `src-tauri/src/tools/session.rs`
- `src-tauri/src/tools/exec.rs`

## Node current state

- `packages/node-agent/src/processes.ts`
- `packages/node-agent/src/types.ts`
- `packages/node-agent/src/tools.ts`
- `packages/node-agent/src/server.ts`
- `packages/node-agent/test/processLifecycle.test.mjs`

## Implementation scope

Process sessions now provide pipe-backed interactive mode without native dependencies, explicit and automatic operation deduplication, Rust-compatible snapshots and control-tool errors, UTF-8-safe output pagination, 90-second detached cleanup, 15-minute/128-session finalized retention, and server-restart finalization.

## Acceptance checklist

- [x] Interactive and stdin-open state is accurate.
- [x] Termination reason, recoverable, suggestion, and timeout fields match Rust semantics.
- [x] First-output and elapsed timings are recorded.
- [x] Detached sessions expire only after the configured grace period.
- [x] Reattachment and deduplication metadata identifies the retained session.
- [x] Output truncation and cursor-expiry metadata remain consistent.
- [x] `exec_health_check` verifies worker availability, session creation, command execution, and both stdout/stderr capture using the Rust response shape.

## Dependencies

Requires `NP-001` and `NP-002`.

## Verification

Covered by dedicated lifecycle tests for interactive stdin, heartbeat waits, operation conflict and deduplication grace, timeout recovery, detached cleanup and cancellation, UTF-8 output pagination, delta cursor continuation, resource-lock metadata, Agent shutdown, finalized-session retention, and the full exec worker/session/stdout/stderr health probe contract. The existing command, graph, post-check, and sensitive-output suites remain green.
