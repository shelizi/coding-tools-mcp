<!-- parity-id: NP-006 -->
<!-- parity-status: done -->
# NP-006 — Gitignore-aware workspace walking

- Priority: P1
- Area: file-contract
- Status: done

## Gap

Resolved in Node Agent 0.15.0. List, search, project inspection, and formatter project scopes now share one bounded Gitignore-aware walker.

## Rust evidence

- `src-tauri/src/tools/file.rs`
- `src-tauri/src/tools/workspace.rs`

## Node current state

- `packages/node-agent/src/gitignore.ts`
- `packages/node-agent/src/workspace.ts`
- `packages/node-agent/src/fileTools.ts`
- `packages/node-agent/src/formatterTools.ts`
- `packages/node-agent/test/gitignoreWalking.test.mjs`

## Implementation scope

The walker evaluates root, ancestor, and nested ignore files; keeps hidden and ignored controls independent; preserves the permanent `.git` boundary; and reports symlinks without traversing them.

## Acceptance checklist

- [x] Root and nested `.gitignore` rules are respected.
- [x] Negated rules restore matching paths with Git-compatible parent-directory behavior.
- [x] `include_ignored=true` bypasses ignore rules but still excludes `.git` internals.
- [x] Hidden-file behavior remains independent from ignored-file behavior.
- [x] Traversal remains bounded and symlink-safe.

## Verification

Covered by `packages/node-agent/test/gitignoreWalking.test.mjs`, a representative differential against `git check-ignore --no-index`, focused read/formatter regressions, and the full `npm run verify:repo` release check.
