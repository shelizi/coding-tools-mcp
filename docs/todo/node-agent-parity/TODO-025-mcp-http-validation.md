<!-- parity-id: NP-025 -->
<!-- parity-status: done -->
# NP-025 — MCP HTTP connection and envelope validation

- Priority: P0
- Area: security
- Status: done
- Assertion: `PA-025-MCP-HTTP-VALIDATION`
- Test file: `packages/node-agent/test/mcpHttpValidation.test.mjs`

## Gap

The Rust streamable-HTTP listener validates the request origin, the optional `MCP-Protocol-Version` header, and the JSON-RPC envelope before authentication dispatch and tool execution. The Node Agent currently authenticates the bearer token but accepts an attacker-controlled `Origin`, ignores unsupported protocol headers, and accepts malformed envelopes such as a missing `jsonrpc: "2.0"` field.

The built-in WSS tunnel forwards non-hop-by-hop headers into the local Node listener, so the missing validation also applies to tunneled requests rather than only direct localhost traffic.

## Rust evidence

- `src-tauri/src/mcp/listener.rs`
  - `validate_standard_connection`
  - `validate_json_rpc_message`
  - `origin_is_allowed`
  - `origin_matches_listener`
  - `normalized_origin`
- `src-tauri/src/auth/oauth.rs`
- Rust tests:
  - `configured_listener_origin_is_allowed`
  - `wildcard_listener_accepts_numeric_interface_origins`
  - `standard_transport_rejects_bad_version_and_origin`
  - `standard_transport_accepts_chatgpt_origin`

## Node current state

- `packages/node-agent/src/server.ts`
  - validates OAuth bearer tokens but does not validate `Origin`
  - negotiates `initialize.params.protocolVersion` but does not reject an unsupported `MCP-Protocol-Version` header
  - parses JSON and dispatches by `method` without validating the JSON-RPC envelope
- `packages/node-agent/src/tunnel.ts`
  - forwards non-hop-by-hop request headers, including `Origin` and `MCP-Protocol-Version`, to the local listener
- `packages/node-agent/test/server.test.mjs`
  - covers the successful OAuth and MCP flow but not hostile origins, unsupported headers, or malformed envelopes

## Reproduced audit evidence

Against Node Agent `0.28.2`:

- `Origin: https://attacker.example` plus an authenticated `ping` returned HTTP 200 and a successful result.
- `MCP-Protocol-Version: unsupported` returned HTTP 200 and a successful result.
- A request with `id` and `method` but no `jsonrpc` field returned HTTP 200 and a successful result.
- An array request body reached method dispatch instead of returning a transport-level invalid-request error.

The Rust test `standard_transport_rejects_bad_version_and_origin` passed on the same repository revision.

## Required implementation

Add one shared standard-connection validation layer before MCP dispatch. It must preserve the current OAuth-only product boundary while matching Rust behavior for streamable HTTP:

- allow requests without an `Origin` header;
- allow configured listener origins, configured public origin, `https://chatgpt.com`, and `https://chat.openai.com`;
- reject malformed or untrusted origins with HTTP 403 and JSON-RPC error code `-32000`;
- reject unsupported or invalid `MCP-Protocol-Version` headers with HTTP 400 and JSON-RPC error code `-32600`;
- require one JSON object using `jsonrpc: "2.0"` and classify request, notification, or client response before dispatch;
- keep authentication challenges and no-store behavior intact;
- apply the same behavior to direct HTTP and built-in WSS forwarded requests.

## Acceptance checklist

- [x] Missing `Origin` is accepted.
- [x] Loopback listener origins are accepted for loopback and unspecified bind addresses.
- [x] Numeric interface origins are accepted only when compatible with the configured bind address and port.
- [x] Configured public origin is accepted.
- [x] ChatGPT and legacy ChatGPT origins are accepted.
- [x] Attacker, malformed, wrong-scheme, wrong-host, and wrong-port origins return HTTP 403 with a bounded JSON-RPC error.
- [x] Supported protocol headers are accepted and unsupported or malformed values return HTTP 400.
- [x] Missing or incorrect `jsonrpc` is rejected before method dispatch.
- [x] Array, scalar, and structurally invalid bodies are rejected as one invalid JSON-RPC message.
- [x] Requests, notifications, and client responses are classified without changing the OAuth-only product boundary.
- [x] Built-in WSS forwarding cannot bypass Origin or protocol validation.
- [x] The planned assertion is promoted to required executable mode and its test file exists before this item is marked done.

## Dependencies

Depends on `NP-013` because the validation layer must preserve the established OAuth challenge and runtime-isolation contract.

## Verification

Implemented in Node Agent `0.28.3`. `mcpHttpValidation.test.mjs` passes four groups covering listener/public/ChatGPT Origin allowlists, unspecified and numeric interfaces, supported protocol headers, strict JSON-RPC classification, parse failures, and built-in WSS header forwarding. Existing server, OAuth, and tunnel regressions also pass; repository-wide parity and verification commands are recorded after the full phase validation.
