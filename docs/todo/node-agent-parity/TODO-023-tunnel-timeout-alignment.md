<!-- parity-id: NP-023 -->
<!-- parity-status: done -->
# NP-023 — Built-in tunnel timeout alignment

- Priority: P2
- Area: transport
- Status: done

## Gap

Node retains demand hints for 10 seconds while Rust uses 3 seconds, causing a longer warm worker floor. Rust also has a dedicated 10-second local connection timeout; Node currently relies on a five-minute overall request abort without a separate connect bound.

## Rust evidence

- `src-tauri/src/tunnel/builtin.rs`
- `crates/tunnel-protocol/src/lib.rs`

## Node current state

- `packages/node-agent/src/tunnel.ts`
- `packages/node-agent/src/tunnelPolicy.ts`
- `packages/node-agent/src/types.ts`
- `packages/node-agent/src/dashboard.ts`
- `packages/node-agent/test/tunnel.test.mjs`

## Required implementation

Align demand hint lifetime and local connection behavior while retaining the existing cancellation and streaming semantics. The implementation must distinguish connection establishment from the overall local response deadline using capabilities available in the supported Node runtime.

## Acceptance checklist

- [x] Default demand hint TTL is 3 seconds.
- [x] Worker reconciliation drops the demand floor after the same Rust boundary.
- [x] Local connection establishment fails within approximately 10 seconds.
- [x] The existing overall local request timeout remains bounded and separately reported.
- [x] Cancel during connect, headers, and streaming aborts all local work.
- [x] Heartbeat remains live during local I/O and timeout handling.
- [x] Worker reuse, recycling, scale-down, and completed-request accounting remain correct.
- [x] Tunnel status exposes the final timeout reason without leaking request data.

## Dependencies

Requires the completed cancellation foundation in `NP-014`.

## Verification

Verified with exported 3-second demand, 10-second connect, and 5-minute overall timeout constants; an exact fake-clock demand boundary; refused and hanging connection fixtures; cancellation during connect, delayed headers, and streaming; heartbeat liveness; bounded timeout status; and worker reuse/accounting regressions. `npm run verify:repo` completed all 197 Node tests with 196 passing and the explicitly gated live-WSL test skipped; the Rust catalog matched, and Desktop compatibility metadata was synchronized to `0.1.36` independently from Node Agent `0.28.2`.
