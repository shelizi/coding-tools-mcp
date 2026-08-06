<!-- parity-id: NP-019 -->
<!-- parity-status: done -->
# NP-019 — Windows process startup resilience

- Priority: P1
- Area: runtime
- Status: done

## Gap

Rust protects Windows process creation with a bounded startup gate, launch spacing, an early-exit probe, transient `0xC0000142` retries, deterministic backoff, a failure window, and a circuit breaker. Node currently performs a single `spawn()` and exposes no equivalent startup diagnostics.

## Rust evidence

- `src-tauri/src/tools/process_start.rs`
- `src-tauri/src/platform/windows/process.rs`
- `src-tauri/src/tools/exec.rs`
- `src-tauri/src/tools/session.rs`

## Node current state

- `packages/node-agent/src/processStartup.ts`
- `packages/node-agent/src/processes.ts`
- `packages/node-agent/src/types.ts`
- `packages/node-agent/src/tools.ts`
- `packages/node-agent/test/processStartup.test.mjs`
- `packages/node-agent/test/processLifecycle.test.mjs`

## Required implementation

Add a pure-JavaScript startup controller without introducing a native dependency. It must serialize only the launch phase, preserve normal process concurrency, classify retryable Windows startup exits, expose bounded diagnostics, and avoid retrying policy, path, or ordinary command failures.

## Acceptance checklist

- [x] Startup gate and minimum launch spacing match Rust defaults.
- [x] Early startup probing detects transient Windows initialization failures.
- [x] Retry count and deterministic delays match the Rust contract.
- [x] Failure-window circuit breaker prevents retry storms and recovers after cooldown.
- [x] Non-Windows platforms and non-retryable failures remain single-attempt.
- [x] Session snapshots expose attempts, retry count, delays, gate wait, and startup slots.
- [x] Cancellation, timeout, detached lifecycle, and output capture remain correct during retries.
- [x] No native module or install script is added.

## Dependencies

Requires the completed process lifecycle foundation in `NP-009`.

## Verification

Verified with an injected launcher/clock for deterministic gate, spacing, retry, timeout, cancellation, and circuit-breaker behavior; signed and unsigned `0xC0000142` classification; a conditional real Windows child startup fixture; retained-session diagnostics; quick buffered-output capture; spawn-failure mapping; and the existing process, Git, formatter, WSL, and folder-isolation regression suites.
