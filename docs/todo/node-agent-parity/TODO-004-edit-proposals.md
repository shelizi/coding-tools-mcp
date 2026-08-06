<!-- parity-id: NP-004 -->
<!-- parity-status: done -->
# NP-004 — Edit proposal and patch recovery workflow

- Priority: P1
- Area: tool-contract
- Status: done


## Gap

Resolved in Node Agent 0.13.0. Node now implements bounded, expiring edit proposals, all three apply modes, stale guards, restricted proposal patches, and structured multi-hunk patch recovery before Git execution.

## Rust evidence

- `src-tauri/src/tools/patch.rs`

## Node current state

- `packages/node-agent/src/fileTools.ts`
- `packages/node-agent/src/editRecovery.ts`
- `packages/node-agent/src/types.ts`
- `packages/node-agent/test/editRecovery.test.mjs`

## Implementation scope

Implemented runtime-scoped proposal creation, five-minute TTL and bounded eviction, stale-file and stale-candidate guards, accept/replacement/patch application, patch efficiency limits, expected-hash checks, and aggregated hunk recovery details.

## Acceptance checklist

- [x] Ambiguous edits return `proposal_required` without writing.
- [x] Proposal IDs expire and are bounded.
- [x] Changed files or candidates reject stale proposals.
- [x] Patch, replacement, and accept application modes are supported.
- [x] Multiple failed hunks are reported together with recovery actions.

## Verification

Covered by `packages/node-agent/test/editRecovery.test.mjs`, existing file/Git/permission tests, and the full `npm run verify:repo` release check.
