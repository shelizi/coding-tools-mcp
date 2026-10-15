# Coding Tools MCP audit logging

Coding Tools MCP keeps two complementary audit surfaces.

## Workspace operation history

`operation_log` is the compact, task-independent workspace history. It records operation/tool identity, lifecycle state, reason, affected-file summaries, verification/result metadata, safe process `output_refs`, and Git provenance such as commit/head values. Raw command output is intentionally not copied into the operation record.

`output://<session-id>/stdout` and `output://<session-id>/stderr` references address retained process-session output. Finalized sessions are short-lived (currently one hour), so these references are for detailed near-term inspection rather than the durable output archive.

## Structured diagnostics

`diagnostics.jsonl` is the canonical durable diagnostic stream. Node and Rust use the same audit policy:

- Tool arguments are centrally redacted before persistence.
- Tool-call diagnostics include `conversation_context_id_hash`, a SHA-256 correlation of the host conversation/session key when one is available. The raw host session key is not persisted in this field.
- Arguments up to 64 KiB are retained structurally; larger arguments fall back to a 4 KiB redacted preview plus size/hash metadata.
- Individual redacted argument strings may occupy the same 64 KiB bounded record budget, allowing ordinary edit `old_text` / `new_text` payloads to remain auditable instead of being cut at 4 KiB.
- Finalized process sessions persist total stdout/stderr byte counts plus at most the final 16 KiB per stream as redacted durable audit evidence.
- `stdout_audit_tail_bytes` / `stderr_audit_tail_bytes` report how much tail evidence is present. `*_audit_tail_truncated` tells the caller that earlier output existed or was not retained.
- Persisted stdout/stderr text remains payload data: normal compact `query_tool_usage` results omit it. Request `include_payloads=true` explicitly when audit content is required.
- Sensitive-output sessions persist `[REDACTED]` rather than their terminal contents.

The canonical diagnostics file rotates at 20 MiB and retains five rotated files. The legacy `mcp-tool-usage.jsonl` compatibility stream may still be dual-written while migration compatibility is enabled.

## What can be reconstructed

For ordinary MCP-mediated work, an auditor can correlate command arguments, workspace/session identifiers, operation state, file edit payloads, affected files, bounded process output evidence, verification status, and Git commit/head provenance. Human account identity is not inferred: if the MCP transport does not provide an authenticated user/account identifier, the audit trail identifies the conversation/session pseudonymously rather than claiming which person operated it. Large edits and process output are deliberately bounded; the audit trail records truncation metadata rather than pretending the retained content is complete.

This design avoids storing unlimited terminal output or bypassing central secret redaction. If a deployment requires complete unlimited historical stdout/stderr, use a separate access-controlled blob/archive sink rather than increasing the JSONL record without bounds.
