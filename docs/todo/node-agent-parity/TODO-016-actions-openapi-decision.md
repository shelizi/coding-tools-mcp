<!-- parity-id: NP-016 -->
<!-- parity-status: excluded -->
# NP-016 — Decide Actions and OpenAPI scope for Node Agent

- Priority: P2
- Area: product-boundary
- Status: excluded

## Gap

The Rust Desktop Client exposes a separate Actions/OpenAPI listener with independent authentication and tunnel routes. The Node Agent has always been documented, routed, and packaged as an MCP-only service.

## Decision

Maintain the current product boundary:

- Node Agent remains MCP-only.
- No Actions listener or OpenAPI document is added.
- No Actions API-key, OAuth, or no-auth configuration is added.
- Built-in WSS continues to expose only the scoped MCP, OAuth, and well-known metadata routes.
- Rust Actions/OpenAPI behavior is an intentional product divergence rather than future Node implementation work.

## Rust evidence

- `src-tauri/src/actions/listener.rs`
- `src-tauri/src/actions/openapi.rs`
- `src-tauri/src/actions/auth.rs`

## Node current state

- `packages/node-agent/src/server.ts`
- `packages/node-agent/src/tunnel.ts`
- `packages/node-agent/README.md`

## Decision checklist

- [x] Node Agent remains MCP-only.
- [x] Actions tool visibility and profile mapping remain outside Node Agent scope.
- [x] Actions authentication and secret storage remain outside Node Agent scope.
- [x] Actions local and built-in WSS routes remain unexposed.
- [x] The exclusion is recorded as `ND-006` in the manifest and intentional-divergence documentation.

## Dependencies

The decision was made after `NP-003`; existing MCP profile exposure remains authoritative.

## Verification

The manifest status, this status marker, `ND-006`, the README phase description, and `INTENTIONAL-DIVERGENCES.md` must remain consistent.
