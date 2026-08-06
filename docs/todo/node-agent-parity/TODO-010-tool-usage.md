<!-- parity-id: NP-010 -->
<!-- parity-status: done -->
# NP-010 — Persistent tool usage analytics

- Priority: P1
- Area: observability
- Status: done


## Gap

Resolved in Node Agent 0.19.0. Node now persists and rotates centrally redacted schema-v7 usage JSONL and exposes the Rust-compatible scope, filter, aggregate, percentile, payload, async-lifetime, burst, orchestration, formatting, and parallelism analysis contracts.

## Rust evidence

- `src-tauri/src/tools/tool_usage.rs`

## Node current state

- `packages/node-agent/src/toolUsage.ts`
- `packages/node-agent/src/tools.ts`
- `packages/node-agent/src/processes.ts`
- `packages/node-agent/src/dashboard.ts`
- `packages/node-agent/src/management.ts`
- `packages/node-agent/test/toolUsage.test.mjs`

## Implementation scope

A dedicated usage store now performs asynchronous append-only writes through a Rust-compatible 1,024-record bounded queue with dropped-record attribution, 20 MiB rotation with five retained files, complete-line-safe reads, stable non-path profile IDs, top-level request timing, schema filters and deterministic aggregates. Process finalization emits async lifetime events, while the Dashboard exposes a cached persistent summary.

## Acceptance checklist

- [x] Usage survives restart and rotates safely.
- [x] current_runtime/current_version/all scopes work.
- [x] Outcome, duration, tool, and timestamp filters match the schema.
- [x] p95, slowest, largest, queue, and response/request byte metrics are reported.
- [x] Async child lifetimes, bursts, and orchestration gaps are derived.
- [x] Payload inclusion remains opt-in and redacted.

## Dependencies

Requires `NP-009` for complete async-session metrics.

## Verification

Covered by deterministic persistence/restart, scope, concurrency timing, rotation, partial-tail, invalid-line, percentile, filtering, payload-redaction, burst, parallelism, async-process, permission-resume, and Dashboard tests. Run `npm run verify:repo` from `packages/node-agent`.
