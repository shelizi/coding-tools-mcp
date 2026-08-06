<!-- parity-id: NP-002 -->
<!-- parity-status: done -->
# NP-002 — Command and mutation policy enforcement

- Priority: P0
- Area: security
- Status: done


## Gap

Resolved in Node Agent 0.11.0. Node now runs one Rust-compatible policy validator before admission queues, locks, permission requests, process creation, or mutation side effects.

## Rust evidence

- `src-tauri/src/tools/policy.rs`
- `src-tauri/src/tools/exec.rs`

## Node current state

- `packages/node-agent/src/tools.ts`
- `packages/node-agent/src/processes.ts`
- `packages/node-agent/src/config.ts`
- `packages/node-agent/src/policy.ts`
- `packages/node-agent/test/policy.test.mjs`

## Implementation scope

Implemented a configured policy model, shared command parser/resolver, and a single pre-dispatch validator for exec, patch, edit, file operations, and formatting.

## Acceptance checklist

- [x] Exactly one of `program`, `cmd`, or `script` is accepted.
- [x] Allowed commands and workspace-local executable entries are enforced.
- [x] Script extension and shell policies match Rust fixtures.
- [x] Network and dangerous bypass rules are explicit and tested.
- [x] Patch and mutation size/count bounds fail before side effects.

## Verification

Covered by `packages/node-agent/test/policy.test.mjs`, the existing command/formatter/file/Git suites, and the full `npm run verify:repo` release check.
