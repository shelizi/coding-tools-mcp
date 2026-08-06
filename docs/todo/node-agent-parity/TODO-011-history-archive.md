<!-- parity-id: NP-011 -->
<!-- parity-status: done -->
# NP-011 — History archive locking and idempotency

- Priority: P1
- Area: persistence
- Status: done


## Gap

Resolved in Node Agent 0.20.0. Node and Rust now share the same version-1 index, numbered structured Markdown, atomic write, scan/rebuild, validation, summary, digest, redaction, and turn-idempotency contracts. Both runtimes also use one cross-platform atomic lock-directory protocol so they can safely update the same archive.

## Rust evidence

- `src-tauri/src/tools/history/mod.rs`
- `src-tauri/src/tools/history/storage.rs`
- `src-tauri/src/tools/history/markdown.rs`

## Node current state

- `packages/node-agent/src/history.ts`
- `packages/node-agent/src/historyModel.ts`
- `packages/node-agent/src/historyMarkdown.ts`
- `packages/node-agent/src/historyStorage.ts`
- `packages/node-agent/test/history.test.mjs`
- `src-tauri/src/tools/history/storage.rs`
- `src-tauri/tests/history_session.rs`

## Implementation scope

The Node implementation is split into model, Markdown, storage, and orchestration layers. `index.json` and numbered Markdown are written atomically; missing/corrupt indexes rebuild from metadata; validation never mutates source documents. Rust advisory file locking was replaced with the same owner-token lock-directory protocol used by Node, including bounded wait and stale-owner recovery.

## Acceptance checklist

- [x] Rust and Node read and update the same index format.
- [x] Concurrent bootstrap/checkpoint uses an exclusive bounded lock.
- [x] Missing or corrupt indexes rebuild from Markdown.
- [x] Duplicate session mappings and sequence gaps are rejected or explicitly repaired.
- [x] Checkpoints are redacted, atomic, and idempotent by turn/content identity.
- [x] Bootstrap returns bounded summaries, omission counts, digest, and lock timing.

## Dependencies

Requires `NP-001`.

## Verification

Covered by Node shared-index, corrupt-index, duplicate-session, gap/invalid/empty validation, concurrent-context allocation, lock timeout/stale recovery, stable-target, redaction, deterministic turn ID, duplicate/update, bounded-summary, and no-side-effect tests. Rust history integration includes concurrent allocation and Node-compatible lock-owner waiting. Run `npm run verify:repo` and `cargo test --test history_session`.
