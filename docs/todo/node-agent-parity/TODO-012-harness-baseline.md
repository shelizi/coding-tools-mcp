<!-- parity-id: NP-012 -->
<!-- parity-status: done -->
# NP-012 — Harness baseline enforcement and evidence

- Priority: P1
- Area: harness
- Status: done


## Gap

Resolved in Node Agent 0.21.0. Node now captures the same Rust branch, HEAD, per-file state and fingerprint baseline; enforces stale active-task gates; refreshes expected state after successful tracked operations; returns project/change evidence; and stores Rust-shaped operation JSONL per canonical workspace ID.

## Rust evidence

- `src-tauri/src/harness/state.rs`
- `src-tauri/src/harness/store.rs`
- `src-tauri/src/harness/tools.rs`
- `src-tauri/src/tools/dispatch.rs`
- `src-tauri/src/tools/session.rs`

## Node current state

- `packages/node-agent/src/taskTools.ts`
- `packages/node-agent/src/operationSummary.ts`
- `packages/node-agent/src/processes.ts`
- `packages/node-agent/src/state.ts`
- `packages/node-agent/src/tools.ts`
- `packages/node-agent/src/types.ts`
- `packages/node-agent/src/dashboard.ts`
- `packages/node-agent/test/harnessBaseline.test.mjs`

## Implementation scope

Node uses Rust's canonical-path workspace hash, skipped-directory set, per-file SHA-256/byte/binary entries, little-endian fingerprint input, capability reasons, baseline-gated tool classification, automatic expected-state refresh, task event evidence, project-state comparison, and correlated started/terminal operation record flow. Rust and Node retained processes both defer their terminal operation until actual session finalization and use synchronized bounded result-summary allowlists. Existing folder-ID tasks migrate on first workspace use. `finish_task.summary` is trimmed and persisted as the auditable completion reason, while omission falls back to the task objective; finishing creates an immutable baseline-to-current change set, records `task_finished` evidence, and stores `latest_change_id`. `change_summary.change_id` selects that exact persisted snapshot, validates optional task ownership, survives restart, and otherwise falls back to the task's latest snapshot or a live preview for an unfinished task. Rust currently has no persisted verification IDs or rollback snapshots in this Harness foundation; Node therefore preserves empty `verification` and `risks` arrays and `not_available_in_foundation` rollback rather than adding divergent behavior.

## Acceptance checklist

- [x] `harness_status` calculates current baseline match and Rust capability metadata.
- [x] Stale branch/HEAD and external file changes return stable reasons and next actions.
- [x] Successful tracked operations refresh the expected fingerprint through the Rust-compatible flow.
- [x] Baselines persist per-file hashes and change summaries return current hash/status evidence.
- [x] Task events persist and `change_summary` returns Rust foundation evidence, empty verification/risks, and unavailable rollback.
- [x] Operation logs use the canonical Rust workspace ID, are isolated per workspace, and survive restart.
- [x] Rust and Node retained process sessions append exactly one real completed/failed terminal with bounded diagnostics and no raw command or output payload.
- [x] `project_state.clean` is computed from the complete file set before `max_files` truncates the returned page.
- [x] `task_context.max_bytes` bounds the serialized response in both runtimes and reports truncation when task or event details are reduced.
- [x] `finish_task.summary` persists as structured completion reason and `task_finished` evidence, with task-objective fallback when omitted.
- [x] Finishing persists an immutable change set and `latest_change_id`; `change_summary.change_id` selects it exactly across restart and rejects malformed or cross-task IDs.

## Verification

Covered by Node tests for baseline entries/fingerprint, stale file and Git gates, dry-run allowance, expected-state refresh, project/change evidence, clean-before-truncation, bounded task context, persisted finish reasons, immutable change selection and restart recovery, malformed and cross-task change IDs, legacy task migration, workspace-isolated persistent operation logs, and the Rust `exec_many` classifier boundary. Rust tests cover the same project-state and task-context bounds, finish-summary fallback, immutable change persistence and selection, bounded operation summaries, and retained-process terminal correlation. Dispatcher policy/process/redaction/profile/server regressions also run before `npm run verify:repo`.
