<!-- parity-id: NP-028 -->
<!-- parity-status: done -->
# NP-028 — MCP transport parity guard expansion

- Priority: P2
- Area: verification
- Status: done
- Assertion: `PA-028-MCP-GUARD-COVERAGE`
- Test files:
  - `packages/node-agent/test/behavioralParity.test.mjs`
  - `tests/node-agent-parity.test.mjs`

## Gap

The behavioral parity guard currently proves only the contracts already registered in its 24 required assertions. It does not require executable coverage for MCP Origin validation, protocol-header and JSON-RPC envelope validation, response heartbeat behavior, or HTTP method/client-response semantics. It also does not export the Rust MCP protocol-version and streaming constants used by those contracts.

Without this item, `node-agent:parity:check` could remain green after the new implementation regresses or after a Rust transport constant changes.

## Rust evidence

- `src-tauri/src/mcp/listener.rs`
  - supported protocol versions
  - stream heartbeat interval
  - stream channel capacity
  - standard connection validation
  - HTTP method and response classification
- `src-tauri/src/mcp/telemetry.rs`
  - transport and protocol fields that must continue to report the negotiated values
- `src-tauri/examples/export_tool_catalog.rs`

## Node current state

- `scripts/check-node-agent-parity.mjs`
- `scripts/run-node-agent-parity-assertions.mjs`
- `tests/node-agent-parity.test.mjs`
- `docs/todo/node-agent-parity/assertions.json`
- `packages/node-agent/scripts/sync-rust-catalog.mjs`
- `packages/node-agent/src/rustCatalog.generated.ts`
- `packages/node-agent/test/behavioralParity.test.mjs`

The roadmap supports `planned` assertions so future fixture IDs and paths can be registered without being counted as passed. Completion of this item must promote all MCP transport assertions to required executable assertions.

## Required implementation

- Export host-neutral MCP transport constants from Rust through the existing generated contract path.
- Compare supported protocol versions, heartbeat interval, and channel capacity in the Node differential fixture.
- Promote `PA-025-MCP-HTTP-VALIDATION`, `PA-026-MCP-STREAMING`, and `PA-027-MCP-HTTP-SEMANTICS` from `planned` to required `node_test` assertions.
- Promote `PA-028-MCP-GUARD-COVERAGE` to required evidence or executable mode.
- Add the three MCP transport categories to `required_categories` only after their executable fixtures exist.
- Ensure a completed roadmap item cannot retain a planned assertion.
- Keep intentional exclusions for OAuth-only auth, streamable-HTTP-only transport, and other desktop host features explicit without suppressing standard transport checks.

## Acceptance checklist

- [x] Rust exports the complete supported MCP protocol-version list.
- [x] Rust exports the ten-second stream heartbeat interval.
- [x] Rust exports the stream channel capacity of two.
- [x] Generated Node fixtures contain the exported values and stale generation fails preflight.
- [x] Behavioral parity tests compare all exported MCP transport constants.
- [x] `mcp_http_validation` is a required executable category.
- [x] `mcp_streaming` is a required executable category.
- [x] `mcp_http_semantics` is a required executable category.
- [x] All three implementation assertions are required and executable.
- [x] Planned assertions are visible but non-passing while work is incomplete.
- [x] Completed or excluded non-product items cannot own a planned assertion.
- [x] Validator tests cover invalid planned/required combinations and promotion on completion.
- [x] CI diagnostics identify the exact NP and PA identifier on failure.

## Dependencies

Requires `NP-025`, `NP-026`, and `NP-027`; it is the final regression guard after the MCP transport behavior and tests are implemented.

## Verification

Rust now exports the complete MCP transport fixture through `export_behavioral_parity_fixtures`; generated Node metadata compares all protocol and streaming constants. The three transport assertions are required executable categories, `PA-028-MCP-GUARD-COVERAGE` is required evidence, and `PA-RUST-SHARED-CONSTANTS` owns this item. Validator tests cover planned-state rejection and final promotion. `npm run verify:repo` completed 209 Node tests with 208 passing and the explicitly gated live-WSL test skipped; native dependency, Rust contract, Desktop compatibility, and all 28 parity assertions passed. The targeted Rust standard-transport rejection test also passed 1/1.
