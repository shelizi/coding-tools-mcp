<!-- parity-id: NP-021 -->
<!-- parity-status: done -->
# NP-021 — Workspace conversation contracts

- Priority: P1
- Area: workspace-contract
- Status: done

## Gap

Node conversation selection/default-cwd maps are unbounded, switching a folder resets its cwd instead of restoring prior folder state, duplicate physical roots are accepted under different IDs, and list/switch metadata is smaller than Rust.

## Rust evidence

- `src-tauri/src/tools/hub.rs`
- `src-tauri/src/tools/context.rs`
- `src-tauri/src/tools/workspace.rs`

## Node current state

- `packages/node-agent/src/conversation.ts`
- `packages/node-agent/src/config.ts`
- `packages/node-agent/src/management.ts`
- `packages/node-agent/src/server.ts`
- `packages/node-agent/src/tools.ts`
- `packages/node-agent/src/types.ts`
- `packages/node-agent/src/workspace.ts`
- `packages/node-agent/src/wsl.ts`
- `packages/node-agent/test/workspaceConversation.test.mjs`
- `packages/node-agent/test/server.test.mjs`
- `packages/node-agent/test/config.test.mjs`
- `packages/node-agent/test/management.test.mjs`

## Required implementation

Introduce a bounded 128-context LRU keyed by conversation identity, retain per-folder cwd state, canonicalize and reject duplicate physical roots, and align list/switch metadata and selection scope with Rust while preserving legacy clients.

## Acceptance checklist

- [x] Conversation state is bounded to 128 contexts with deterministic LRU eviction.
- [x] Each conversation retains an independent selected folder.
- [x] Each conversation restores the prior default cwd when switching back to a folder.
- [x] Different folder IDs cannot resolve to the same physical workspace root.
- [x] Duplicate roots are detected across equivalent Windows, UNC, and WSL forms.
- [x] `list_workspace_folders` and `switch_workspace_folder` expose Rust-compatible isolation, history, profile, and cwd metadata.
- [x] Missing conversation metadata uses a stable fallback context without cross-client leakage.
- [x] Management save/load preserves canonical folder identities.

## Dependencies

Requires `NP-018` so conversation selection references folder-scoped resources.

## Verification

Verified with multi-conversation selection, deterministic 128-context LRU eviction, evicted-context cwd restoration, per-folder cwd switching, per-runtime legacy fallback, strict MCP missing-metadata routing, HTTP transport coverage, canonical and symlink/junction duplicate roots, Windows/UNC/WSL identity equivalence, stable profile identity across restarts, and management/config canonical persistence fixtures.
