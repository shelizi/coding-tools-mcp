<!-- parity-id: NP-027 -->
<!-- parity-status: done -->
# NP-027 — MCP HTTP method and client-response semantics

- Priority: P1
- Area: transport
- Status: done
- Assertion: `PA-027-MCP-HTTP-SEMANTICS`
- Test file: `packages/node-agent/test/mcpHttpSemantics.test.mjs`

## Gap

The Rust streamable-HTTP listener distinguishes HTTP transport semantics from JSON-RPC method dispatch. The Node Agent currently returns a successful 204 for `DELETE /mcp`, omits the `Allow` header for unsupported methods, and treats a valid client JSON-RPC response as a method-not-found request.

Transport validation errors also need stable HTTP status codes and JSON-RPC error bodies instead of being converted into an HTTP 200 internal RPC response.

## Rust evidence

- `src-tauri/src/mcp/listener.rs`
  - `mcp_get`
  - `mcp_delete`
  - `method_not_allowed`
  - client-response acceptance in `mcp_post`
  - notification acceptance in `mcp_post`
  - `transport_error`
- Rust tests:
  - `standard_get_returns_method_not_allowed`
  - `standard_notification_returns_accepted_without_rpc_body`
  - `standard_transport_rejects_bad_version_and_origin`

## Node current state

- `packages/node-agent/src/server.ts`
  - `GET /mcp` returns 405 text without `Allow`
  - `DELETE /mcp` returns 204
  - notifications return 202
  - client responses fall through to `Method not found`
  - malformed transport input is caught by the generic JSON-RPC 200 error path
- `packages/node-agent/test/server.test.mjs`
  - currently lacks method matrix, client-response, and transport-error assertions

## Required implementation

Match Rust streamable-HTTP semantics while keeping legacy JSON transport excluded:

- `GET /mcp` returns HTTP 405 with `Allow: POST` after connection validation;
- `DELETE /mcp` returns HTTP 405 with `Allow: POST` after connection validation;
- other unsupported methods return a stable 405 and correct `Allow` header;
- notifications return HTTP 202 with no JSON-RPC body;
- valid client JSON-RPC responses return HTTP 202 with no body;
- transport-layer invalid requests use HTTP 400 or 403 and a JSON-RPC error body with `id: null`;
- normal JSON-RPC method and tool errors continue to use HTTP 200;
- all discovery, error, and JSON responses remain `cache-control: no-store`.

## Acceptance checklist

- [x] GET method response matches Rust status and `Allow` header.
- [x] DELETE method response matches Rust status and `Allow` header.
- [x] Other unsupported methods have deterministic status, body, and `Allow` behavior.
- [x] Auth and connection validation run before method rejection.
- [x] Notifications return 202 with an empty body.
- [x] Valid client responses containing `result` return 202.
- [x] Valid client responses containing `error` return 202.
- [x] Invalid request/notification/response envelopes return transport-level 400 errors.
- [x] Origin rejection returns transport-level 403.
- [x] Normal unknown methods and unknown tools retain their JSON-RPC 200 contracts.
- [x] All applicable responses retain no-store caching behavior.
- [x] The planned assertion is promoted to required executable mode and its test file exists before this item is marked done.

## Dependencies

Depends on `NP-025` because method handling requires the same request classification and standard-connection validation.

## Verification

Implemented in Node Agent `0.28.3`. `mcpHttpSemantics.test.mjs` passes four groups covering authentication and connection validation ordering, `Allow: POST`, notification and client-response acceptance, transport-level 400/403 errors, JSON-RPC 200 method errors, no-store headers, and streamable-HTTP discovery metadata. Existing server and MCP response-contract regressions remain green.
