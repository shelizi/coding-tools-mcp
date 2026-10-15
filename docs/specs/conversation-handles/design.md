# Design

## 概述
After existing HTTP authentication and transport validation, a private ingress
adapter extracts conversation_handle/create_conversation_handle and derives an
owner fingerprint from the verified Bearer credential. It resolves a memory-only
store scoped to the current profile/runtime. It injects only the resolved internal
conversation key into the dispatch copy of metadata; caller metadata cannot use
the reserved ctmcp-handle: namespace. Diagnostics see only original safe metadata.

The normal dispatcher and workspace permission path remain authoritative. Native
identity continues to be the routing key for native-bound handles. Identity-less
handles use a distinct unpredictable internal key. Native presence and value must
match the issuance context exactly. Handle errors never downgrade routing.

## 技术方案
Tokens: ch_ plus 32 cryptographically random bytes encoded as 64 lowercase hex
characters. Store only the token digest, owner fingerprint, native identity,
internal routing key and expires_at_ms. Absolute TTL is 86400000 ms. Bound to 1024
live entries per profile. Expired handle-owned routing metadata is cleaned up;
native selections are not removed when optional native handles expire. Existing
bounded context caches continue their normal lifecycle; no process kill is added.

## Response and secrecy
Only conversation_bootstrap returns the raw conversation_handle, including
folder-choice responses. Handle-backed responses include conversation_handle_expires_at_ms and
responses identify conversation_identity_source=conversation_handle and whether a
platform conversation is bound. Decorate responses after execution telemetry, so
the routing handle does not enter logs even if normal redaction is disabled. Secret redaction
also recognizes the field and token syntax for accidental embeddings in text.
Arguments are removed before ordinary tool handlers and execution identity logic.
For clients that consume only one text block, serialize the complete merged
structured result and handle metadata into a single text block. Preserve all
non-text content blocks and the original isError/resultType status. If a tool has
only legacy text, preserve it in the merged payload's text field. Non-tool Task
or input-required envelopes without content/structuredContent stay unchanged.

## Protocol and compatibility
Expose the optional handle on every built-in MCP tool. Expose
create_conversation_handle only on bootstrap for native clients opting in. External
proxied tools and non-tool RPC methods are outside this initial argument contract.
Handle-backed identity-less calls use ordinary retained process-control tools;
disable automatic MCP Tasks projection because client protocol polling cannot
carry a tool argument. Existing native MCP Tasks behavior remains unchanged.
Actions omit these MCP-only arguments and remain explicitly folder-routed.

## Rollout
Test/build on D:, retain unrelated working-tree changes. Refresh generated Node
catalog/revisions from Rust. Do not restart the user's running server implicitly;
source/tests and any staged artifact are reported separately from live activation.

## 文件结构
Rust: mcp/conversation_handles.rs, mcp/listener.rs, tools/registry.rs, tools/hub.rs, tools/redaction.rs, actions/openapi.rs. Node: server/mcp/conversationHandles.ts, server/routes/mcp.ts, redaction.ts, generated catalog and tests.

## 需求追踪
FR-1 through FR-3 map to ingress, schema and handle storage; FR-4 through FR-6
map to verified ownership, native identity conflicts and expiry/capacity checks;
FR-7 maps to stripping and redaction; FR-8 maps to Rust/Node contracts and Actions
exclusion; FR-9 maps to native routing and process-control compatibility.
