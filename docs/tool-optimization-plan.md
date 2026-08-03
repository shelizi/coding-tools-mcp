# Tool reliability and efficiency improvement plan

## Goal

Reduce preventable tool failures and tool-call round trips without weakening workspace, version, or policy safeguards.

## Baseline findings

The reviewed logs show that the largest avoidable costs come from guarded edit failures, repeated retries with unchanged inputs, cross-workspace routing mistakes, quiet long-running commands polled every 30 seconds, broad searches, and duplicated history payloads. Admission queues and workspace locks are not the primary bottleneck.

## Phase 1: diagnostics and low-risk efficiency improvements

Implemented in this change:

- Editing conflicts return machine-readable recovery actions, current file hashes, candidate ranges, and mismatch reasons.
- `exec_many` reports failed and skipped command IDs, a bounded first-failure summary, and recovery actions.
- `wait_command` supports server-side waits up to 120 seconds and documents `until=finalized` for quiet commands.
- Tool-usage schema version 4 records recovery coverage, batch failure counts, command ID counts, and wait parameters.
- Usage aggregation reports identical consecutive failures, empty wait timeouts, recovery action coverage, and batch failure totals.

Success indicators:

- Fewer identical consecutive error signatures.
- Lower median tool calls between an edit failure and the next successful edit.
- Fewer zero-event `wait_command` timeouts per completed process.
- A higher percentage of edit errors with at least one recovery action.

## Implemented follow-up: generic formatting workflow

The format-related failure found during commit cleanup is addressed by a generic `format_files` workflow rather than by broadening the arbitrary command allowlist.

Implemented capabilities:

- Multi-language adapter planning with explicit, configuration, manifest, custom, and language-default selection sources.
- `files`, Git `changed`, Git `staged`, and bounded `project` scopes.
- `plan`, isolated `check`, and guarded `apply` modes.
- SHA-256 preflight and revalidation before workspace writes.
- Mirror snapshots that reject unexpected formatter changes.
- Rollback for partial workspace write failures.
- Strict and non-strict behavior for unavailable formatters.
- Workspace-local executable preference and structured shell-free arguments.
- Confirmed workspace custom adapters from `.coding-tools/formatters.json`.
- Tool-usage schema version 5 format metrics without logging file contents or full diffs.

The reusable file-action core is intentionally separate from public tool semantics so linting, semantic fixes, and import organization can share the safe execution foundation without being silently bundled into formatting.

Success indicators:

- Changed-file format checks no longer require direct `rustfmt` allowlist access.
- Plan and check requests do not mutate workspace files.
- No formatter can modify files outside its selected mirror group without detection.
- Broad project apply and custom formatter execution require explicit confirmation.

## Phase 2: workspace-aware routing

Add an optional `workspace_folder_id` to filesystem, Git, and execution tools. Resolve it in the tool hub before dispatch and include both requested and resolved workspace IDs in telemetry.

On `NOT_FOUND`, return whether the same relative path exists in another configured workspace and provide a safe `switch_workspace_folder` action. Do not allow absolute paths as a routing workaround.

Required telemetry:

- `requested_workspace_id`
- `resolved_workspace_id`
- `workspace_route_source`
- `workspace_route_changed`
- `alternate_workspace_match_count`
- `cross_workspace_recovery_used`

Success indicators:

- Fewer `NOT_FOUND`, `ABSOLUTE_PATH_DENIED`, and `PATH_OUTSIDE_WORKSPACE` errors caused by wrong workspace selection.
- Fewer workspace switches per multi-project task.

## Phase 3: recovery-chain measurement

Each failed call should expose a stable, non-secret correlation identifier. Follow-up requests may provide `retry_of_call_sequence` or `recovery_of_operation_id`.

Record:
- failure fingerprint and error code
- suggested recovery action IDs
- selected recovery action ID
- calls and elapsed time until recovery
- whether the recovery succeeded
- whether an unchanged request was repeated
- whether fresh file content or SHA was obtained before retry

This enables measuring recovery quality rather than only counting errors.

## Phase 4: search and history payload reduction

Search:

- Add `calculate_total=false` and stop after enough results.
- Record early-stop reason, excluded directories, files considered, files scanned, and result usefulness signals.
- Recommend narrower globs when a query is broad.

History:

- Default bootstrap to the latest handoff, a small recent-summary window, and a digest.
- Load older summaries on demand.
- Record payload overlap among `all_history_summary`, `session_summaries`, `latest_handoff`, and `inherited_summary`.

Success indicators:

- Lower response bytes and p95 latency for `search_text` and history bootstrap.
- No regression in task completion or restored-context accuracy.

## Phase 5: guarded Git transaction

Provide a guarded status/diff/commit transaction that validates expected HEAD and selected paths in one operation. Return a concise conflict report if HEAD changes.

Record:

- expected and actual HEAD
- selected path count
- index cleanliness
- conflict source
- transaction stage reached

## Additional log fields worth adding

The following fields are most useful for future optimization:

1. Correlation: `conversation_operation_id`, `retry_of_call_sequence`, `recovery_of_operation_id`.
2. Freshness: milliseconds between file read and guarded write, expected/current SHA, and intervening modification detection.
3. Recovery: suggested action IDs, selected action, recovery success, recovery calls, and recovery elapsed time.
4. Routing: requested/resolved workspace and alternate workspace matches.
5. Search: early-stop reason, directories excluded, files considered versus scanned, and count calculation time.
6. Process waiting: requested wait, actual wait, event count, time to first output, number of empty waits, and time to finalization.
7. History: bytes by payload section, overlap estimate, index hit/miss/rebuild, and time spent reading versus summarizing.
8. Batch editing: failing file index, edit index, atomic preflight duration, and whether another edit in the same batch would have succeeded.
9. Outcome quality: task-level completion status and number of calls from first attempt to verified success.

## Safety constraints

- Never auto-apply an edit after a version mismatch without a fresh guard.
- Never weaken path or executable boundaries to reduce error counts.
- Do not log file contents, secrets, tokens, stdin, or raw credentials.
- Hash correlation values when they could expose user-provided identifiers.
- Keep detailed payloads bounded and make aggregate metrics the default query output.
