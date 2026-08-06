<!-- parity-id: NP-018 -->
<!-- parity-status: done -->
# NP-018 — Workspace-folder runtime isolation

- Priority: P0
- Area: security
- Status: done

## Gap

Node shares process sessions, pending permissions, operation fingerprints, locks, and other execution resources across configured folders. The audit reproduced a process created in folder A being listed and killed from folder B, and a permission request created in A being resumed after switching to B and writing into B.

## Rust evidence

- `src-tauri/src/tools/context.rs`
- `src-tauri/src/tools/hub.rs`
- `src-tauri/src/tools/permission.rs`
- `src-tauri/src/tools/session.rs`

## Node current state

- `packages/node-agent/src/executionScope.ts`
- `packages/node-agent/src/folderRuntime.ts`
- `packages/node-agent/src/server.ts`
- `packages/node-agent/src/tools.ts`
- `packages/node-agent/src/processes.ts`
- `packages/node-agent/src/editRecovery.ts`
- `packages/node-agent/src/fileTools.ts`
- `packages/node-agent/src/dashboard.ts`
- `packages/node-agent/src/management.ts`
- `packages/node-agent/src/types.ts`
- `packages/node-agent/test/processLifecycle.test.mjs`
- `packages/node-agent/test/folderIsolation.test.mjs`

## Required implementation

Create folder-scoped execution resources and bind every retained process, operation fingerprint, resource lock, pending permission, and resume payload to an immutable workspace identity. Resuming must execute against the original folder or fail safely; it must never reinterpret the request using the currently selected folder.

## Acceptance checklist

- [x] Sessions started in one folder are invisible and uncontrollable from another folder.
- [x] `wait_command`, `read_output`, `send_input`, `kill_session`, and operation reattachment enforce folder identity.
- [x] Pending permissions store their original folder and canonical workspace identity.
- [x] Switching folders before `request_permissions` cannot redirect a mutation.
- [x] Exact cross-folder resume IDs route to the original folder; unknown and expired IDs return `RESUME_OPERATION_NOT_FOUND`, while stale workspace identity returns `RESUME_OPERATION_STALE`.
- [x] Folder resources are disposed without leaking child processes, timers, or locks.
- [x] Per-folder dedupe and resource-lock behavior remains deterministic.
- [x] Single-folder behavior and transport cancellation regressions pass.

## Dependencies

Requires `NP-017` so stored workspace identities use the canonical containment model.

## Verification

Implemented `folderIsolation.test.mjs` with two-folder process visibility/control, operation and fingerprint reuse, independent resource locks, output-reference isolation, immutable permission resume routing, expired/stale/missing resume IDs, the 256-entry pending-operation limit, edit-proposal isolation, multi-workspace dashboard aggregation, and configured-root symlink replacement after retention. Runtime creation snapshots each canonical workspace root, so retained operations cannot follow a later root-link retarget. Existing process lifecycle, server-close recovery, transport cancellation, management, edit recovery, and basic tool suites remain green.
