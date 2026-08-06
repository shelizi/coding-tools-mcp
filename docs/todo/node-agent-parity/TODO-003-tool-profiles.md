<!-- parity-id: NP-003 -->
<!-- parity-status: done -->
# NP-003 — Tool profiles and profile-specific exposure

- Priority: P0
- Area: exposure
- Status: done


## Gap

Resolved in Node Agent 0.12.0. Node now derives all five profile catalogs, annotations, tool names, and revisions directly from the Rust registry and enforces the effective profile before dispatch.

## Rust evidence

- `src-tauri/src/tools/registry.rs`
- `src-tauri/src/mcp/server.rs`

## Node current state

- `packages/node-agent/src/catalog.ts`
- `packages/node-agent/src/server.ts`
- `packages/node-agent/src/config.ts`
- `packages/node-agent/src/tools.ts`
- `packages/node-agent/src/management.ts`
- `packages/node-agent/src/rustCatalog.generated.ts`
- `packages/node-agent/test/profile.test.mjs`

## Implementation scope

Implemented configured and permission-resolved profiles, Rust-generated profile catalogs, profile revisions and compatibility annotations, pre-dispatch exposure enforcement, and consistent reporting across MCP and management surfaces.

## Acceptance checklist

- [x] Every Rust profile has an exact Node tool-name fixture.
- [x] Hidden tools cannot be called through direct RPC.
- [x] Toolset revisions change with profile and contract changes.
- [x] Compat read-only annotations match Rust.
- [x] Management configuration validates and displays the active profile.

## Dependencies

Requires `NP-002` so exposed profiles and execution policy cannot diverge.

## Verification

Covered by `packages/node-agent/test/profile.test.mjs`, management tests, generated Rust contract checks, and the full `npm run verify:repo` release check.
