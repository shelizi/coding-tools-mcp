<!-- parity-id: NP-014 -->
<!-- parity-status: done -->
# NP-014 — Built-in WSS cancellation during local response

- Priority: P1
- Area: transport
- Status: done


## Gap

Resolved in Node Agent 0.22.0. Node now consumes matching cancel frames while awaiting local HTTP response headers and while streaming the response body, aborts the corresponding local I/O, and returns the worker to the ready pool without leaking a queue waiter or reader.

The earlier roadmap text incorrectly included `policy_update` among the Rust-supported busy-response frames. The current Rust `forward_request` accepts a matching `cancel` plus WebSocket Ping/Pong during these phases and rejects other text controls as unexpected. Node follows that implemented Rust contract rather than adding broader policy behavior.

## Rust evidence

- `src-tauri/src/tunnel/builtin.rs`

## Node current state

- `packages/node-agent/src/runtime.ts`
- `packages/node-agent/src/tunnel.ts`
- `packages/node-agent/test/tunnel.test.mjs`

## Implementation scope

Local fetch and response streaming race against an abortable worker-queue wait. When local I/O wins, the losing queue waiter is removed before it can consume a later frame. When a matching cancel wins, Node aborts fetch, cancels the response reader when present, suppresses `response_end` and error frames, counts the request like Rust, and sends `ready` for the same worker. WebSocket heartbeat remains active independently of local I/O.

## Acceptance checklist

- [x] Cancel before local response headers aborts fetch.
- [x] Cancel during response streaming cancels the reader and local request.
- [x] Matching cancel emits no response end or request error.
- [x] WebSocket heartbeat liveness remains active during local I/O.
- [x] The same worker returns to ready and completes later requests.
- [x] Losing queue waits are removed without consuming later frames.

## Verification

The local WebSocketServer integration now covers delayed headers, a delayed streaming body, server Ping/client Pong during both phases, local connection closure after cancel, absence of response-end/error frames, Rust-compatible completed-request accounting, and successful reuse of the same single worker after each cancellation. The existing dynamic WorkerPolicy test also verifies that a completed local response does not leave a stale queue waiter that consumes the later policy update.
