<!-- parity-id: NP-005 -->
<!-- parity-status: done -->
# NP-005 — UTF-16 and BOM-aware text decoding

- Priority: P1
- Area: file-contract
- Status: done


## Gap

Resolved in Node Agent 0.14.0. Node now uses one strict, bounded decoder for UTF-8, UTF-8 BOM, UTF-16LE BOM, and UTF-16BE BOM, with stable binary and unsupported-encoding errors.

## Rust evidence

- `src-tauri/src/tools/file.rs`

## Node current state

- `packages/node-agent/src/fileTools.ts`
- `packages/node-agent/src/workspace.ts`
- `packages/node-agent/src/textCodec.ts`
- `packages/node-agent/src/tools.ts`
- `packages/node-agent/src/editRecovery.ts`
- `packages/node-agent/test/textDecoding.test.mjs`

## Implementation scope

Implemented a bounded decoder shared by read, search, project inspection, and edit preflight. Text edits preserve the original encoding and BOM, while Git patch preflight explicitly rejects UTF-16.

## Acceptance checklist

- [x] UTF-8 with and without BOM is decoded and reported correctly.
- [x] UTF-16LE BOM is decoded and reported correctly.
- [x] UTF-16BE BOM is decoded and reported correctly.
- [x] Malformed and unsupported encodings return a stable error.
- [x] Byte limits are enforced before unbounded conversion.

## Verification

Covered by `packages/node-agent/test/textDecoding.test.mjs`, read/edit recovery regressions, and the full `npm run verify:repo` release check.
