<!-- parity-id: NP-022 -->
<!-- parity-status: done -->
# NP-022 — MCP response and error contracts

- Priority: P2
- Area: tool-contract
- Status: done

## Gap

Node duplicates most structured tool payloads by serializing the entire result into MCP text content, while Rust emits a concise text summary and keeps the full data in `structuredContent`. Node also collapses several workspace failures into generic `TOOL_FAILED` envelopes and omits Rust top-level status/summary fields.

## Rust evidence

- `src-tauri/src/mcp/server.rs`
- `src-tauri/src/tools/workspace.rs`
- `src-tauri/src/tools/dispatch.rs`

## Node current state

- `packages/node-agent/src/toolContract.ts`
- `packages/node-agent/src/server.ts`
- `packages/node-agent/src/tools.ts`
- `packages/node-agent/src/workspace.ts`
- `packages/node-agent/test/mcpResponseContracts.test.mjs`
- `packages/node-agent/test/server.test.mjs`
- `packages/node-agent/test/basic.test.mjs`
- `packages/node-agent/test/processLifecycle.test.mjs`

## Required implementation

Create a bounded concise-summary layer for MCP content, preserve image content handling, and centralize Rust-compatible workspace/tool error mapping. Direct internal calls must retain their structured contract while HTTP MCP avoids duplicating large content.

## Acceptance checklist

- [x] Normal MCP tool responses contain concise bounded text plus one full `structuredContent` payload.
- [x] Large file, process, Git, project-map, and usage results are not duplicated.
- [x] Image content remains a single MCP image payload with metadata only in `structuredContent`.
- [x] Error envelopes include Rust-compatible `status`, `summary`, code, category, retryability, and details.
- [x] Filesystem errors distinguish not found, directory/type errors, absolute denial, symlink escape, and outside-workspace paths.
- [x] Unknown-tool JSON-RPC behavior and toolset revision metadata remain unchanged.
- [x] Redaction applies before both summary and structured response serialization.
- [x] Response-size and token-regression fixtures cover success and error paths.

## Dependencies

Requires `NP-017` so path errors are mapped from the final canonical resolver.

## Verification

Verified with bounded UTF-8 summaries, single-copy 32 KiB file/process/Git/project-map/usage payloads, single-image metadata-only responses, five native filesystem errno mappings, canonical workspace errors, unknown-tool JSON-RPC regression, pre-serialization redaction, serialized byte-size limits, and direct-call preservation of tool-specific statuses such as `unsupported` and `timed_out`.
