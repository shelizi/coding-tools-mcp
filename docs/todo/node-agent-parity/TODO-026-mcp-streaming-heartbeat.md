<!-- parity-id: NP-026 -->
<!-- parity-status: done -->
# NP-026 — Streamable HTTP response heartbeat parity

- Priority: P1
- Area: transport
- Status: done
- Assertion: `PA-026-MCP-STREAMING`
- Test file: `packages/node-agent/test/mcpStreaming.test.mjs`

## Gap

For non-fast-path MCP calls, the Rust listener returns HTTP response headers immediately and streams JSON whitespace every ten seconds until the final JSON-RPC response is available. This keeps proxies and tunnels alive while preserving an `application/json` response body. The Node Agent currently waits for `callTool()` to complete and sends the full response in one write.

This difference is material for long-running commands and built-in WSS traffic because the connection may appear idle even though execution is healthy.

## Rust evidence

- `src-tauri/src/mcp/listener.rs`
  - `MCP_STREAM_HEARTBEAT_INTERVAL = 10 seconds`
  - `MCP_STREAM_CHANNEL_CAPACITY = 2`
  - `streaming_json_no_store`
  - `x-accel-buffering: no`
  - `x-coding-tools-streaming: 1`
- Rust integration test:
  - `real_http_disconnect_cancels_exec_and_releases_session_capacity`

## Node current state

- `packages/node-agent/src/server.ts`
  - awaits `callTool()` before `sendJson`
  - does not send streaming response headers or heartbeat bytes
- `packages/node-agent/src/processes.ts`
  - already has request lifecycle cancellation hooks that must remain authoritative
- `packages/node-agent/src/tunnel.ts`
  - can forward streamed local response chunks and already has independent local request timeouts
- `packages/node-agent/test/processLifecycle.test.mjs`
- `packages/node-agent/test/server.test.mjs`

## Reproduced audit evidence

A direct HTTP call executing a command for approximately 1.5 seconds received response headers only after approximately 1.56 seconds, at the same time as the complete body. The response had neither `x-coding-tools-streaming` nor `x-accel-buffering`.

## Required implementation

Implement a bounded streamable-HTTP response path for non-fast-path, non-notification calls:

- flush HTTP 200 response headers before the tool finishes;
- use `application/json`, `cache-control: no-store`, `x-accel-buffering: no`, and `x-coding-tools-streaming: 1`;
- emit JSON whitespace at the Rust ten-second heartbeat interval;
- bound queued stream chunks to the Rust-equivalent capacity of two;
- send exactly one final serialized JSON-RPC response;
- stop heartbeat and execution when the client disconnects;
- preserve the reconnect/session cleanup behavior already implemented for process requests;
- remain compatible with the built-in WSS local response reader and five-minute overall tunnel timeout.

Fast-path calls (`initialize`, `ping`, `tools/list`, notifications) should retain the simple non-streaming path unless the Rust contract changes.

## Acceptance checklist

- [x] Slow tool calls expose response headers before completion.
- [x] The streaming headers match Rust.
- [x] Heartbeat bytes are valid leading JSON whitespace and occur at the ten-second interval.
- [x] No heartbeat is emitted after the final payload or after cancellation.
- [x] The final body parses as one JSON-RPC response after leading whitespace is ignored.
- [x] The bounded channel cannot grow without limit under a slow client.
- [x] Client disconnect cancels active execution and eventually releases session capacity.
- [x] Fast-path requests and notifications retain their expected response behavior.
- [x] Built-in WSS forwards heartbeat and final payload without buffering the whole response.
- [x] Tunnel cancellation, connect timeout, overall timeout, and worker reuse tests remain green.
- [x] The planned assertion is promoted to required executable mode and its test file exists before this item is marked done.

## Dependencies

Depends on `NP-025` so all streamed requests first pass the shared connection and envelope validation layer.

## Verification

Implemented in Node Agent `0.28.3`. `mcpStreaming.test.mjs` passes bounded queue, early-header/heartbeat/final-body, and disconnect-detach coverage. The existing real HTTP abort regression was updated for immediate response headers and all nine process lifecycle tests pass. Built-in WSS forwards heartbeat-like and final response chunks incrementally, while all eleven tunnel cancellation, timeout, and reuse tests pass.
