<!-- parity-id: NP-001 -->
<!-- parity-status: done -->
# NP-001 — Central sensitive output redaction

- Priority: P0
- Area: security
- Status: done


## Gap

Resolved in Node Agent 0.10.0. Every tool result now passes through a Rust-compatible central redaction context, while process sessions persist protected-source sensitivity across initial execution and retained-session APIs.

## Rust evidence

- `src-tauri/src/tools/redaction.rs`
- `src-tauri/src/tools/session.rs`

## Node current state

- `packages/node-agent/src/tools.ts`
- `packages/node-agent/src/processes.ts`
- `packages/node-agent/src/redaction.ts`
- `packages/node-agent/test/redaction.test.mjs`

## Implementation scope

Implemented a reusable recursive redactor, Rust-compatible sensitive source-path detection, process-session sensitivity tracking, and stable response metadata (`sensitive_data_redacted`, `redaction_count`, warnings).

## Acceptance checklist

- [x] Sensitive key names and nested values are redacted.
- [x] Authorization headers, private keys, JWT-like tokens, and common credential assignments are redacted.
- [x] Reads of protected credential paths do not return raw content.
- [x] Sensitive process stdout, stderr, and delta events are withheld.
- [x] False-positive regression fixtures cover normal source code and hashes.

## Verification

Covered by `packages/node-agent/test/redaction.test.mjs`, the existing core tool suite, and the full `npm run verify:repo` release check.
