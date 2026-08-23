# Node Agent Rust parity roadmap

This directory tracks behavior implemented in the Rust Client that is missing or materially incomplete in the pure Node.js Agent.

The verified re-audit baseline is commit `a9ecac570119a38c5034845847e1aee8ac2e2a82` using the ancestor-anchor policy, Desktop Client `0.1.43`, Node Agent `0.29.13`, and client compatibility `0.1.43`.

## Commands

```powershell
npm run node-agent:parity:check
npm run node-agent:parity:complete
```

`parity:check` validates schema-v2 baseline metadata, source references, dependencies, status markers, assertion ownership, intentional divergence links, and 28 required behavioral assertions. Eleven high-risk assertion groups execute Node differential fixtures; generated Rust fixtures compare shared admission, conversation, Windows startup, MCP transport, and tunnel constants without source-text snapshots. `parity:complete` additionally fails while any item is `todo`, `in_progress`, or `blocked`; the current roadmap is complete.

## Status workflow

1. Pick the first ready item printed by the checker.
2. Change its manifest status and the `parity-status` marker in the matching Markdown file to `in_progress`.
3. Implement only the scoped behavior and acceptance tests in that file.
4. Run the listed verification plus `npm run node-agent:verify-repo`.
5. Mark the item `done` only after all acceptance checkboxes are complete.
6. Run the checker again; it will select the next dependency-ready item.

## Recommended execution phases

### Phase 0 — security and exposure

`NP-001` → `NP-002` → `NP-003`

### Phase 1 — core tool contracts

`NP-004`, `NP-005`, `NP-006`, `NP-007`, `NP-008`

### Phase 2 — runtime, persistence, and transport

`NP-009` → `NP-010`; plus `NP-011`, `NP-012`, `NP-013`, `NP-014`

### Phase 3 — platform and product boundary

`NP-015`; `NP-016` is resolved as an intentional MCP-only product exclusion. Actions/OpenAPI, static Bearer/no-auth MCP modes, legacy JSON transport, the Desktop history activity viewer, and the Desktop runtime/port supervisor remain outside the Node Agent boundary.

### Phase 4 — re-audit security and runtime gaps

1. `NP-017` canonical workspace containment.
2. `NP-018` folder-scoped runtime isolation.
3. `NP-020` and `NP-021` after folder isolation; `NP-022` after path containment.
4. `NP-019` Windows startup resilience and `NP-023` tunnel timeout alignment may proceed independently.
5. `NP-024` closes the phase with behavioral parity assertions.

The re-audit implementation sequence `NP-017` through `NP-024` is complete, including path containment, folder isolation, Windows startup resilience, dual-layer admission, workspace conversation contracts, MCP response/error contracts, tunnel timeout alignment, and the behavioral parity guard.

### Phase 5 — MCP HTTP transport synchronization

1. `NP-025` adds Origin, protocol-header, and JSON-RPC envelope validation.
2. `NP-026` adds immediate streaming headers and the bounded ten-second heartbeat path.
3. `NP-027` aligns HTTP methods, `Allow` headers, notifications, client responses, and transport errors.
4. `NP-028` promotes the planned assertions to required executable checks and exports Rust MCP transport constants.

Phase 5 first completed in Node Agent `0.28.3` and remains required parity coverage in `0.29.6`. Origin and protocol validation, strict JSON-RPC classification, streamable-HTTP heartbeats, HTTP method/client-response semantics, WSS streaming, and Rust-generated transport guards remain protected.

Management UI synchronization is tracked separately in `../node-agent-ui-parity/CHECKLIST.md` so desktop-only exclusions do not distort the shared MCP behavioral roadmap.

## Behavioral assertion guard

- `assertions.json` maps stable `PA-*` assertion IDs to owning `NP-*` roadmap items and test evidence.
- `planned` assertions may reserve future fixture IDs and paths without claiming implementation or test success; completed non-product items cannot retain them. No current assertion is planned.
- Required high-risk categories cannot be removed or downgraded to non-executable evidence without failing validation.
- Failed, skipped, missing, or unowned assertions report the exact roadmap item and assertion ID.
- The baseline commit is an audited ancestor anchor; versions must match exactly while later commits may add verified guard implementation.

## Scope rules

- A generated Rust catalog match proves names and schemas only; handler behavior still requires parity tests.
- Desktop-only features are documented in `INTENTIONAL-DIVERGENCES.md` and are not active implementation work.
- New Rust Client behavior affecting shared MCP contracts must update this manifest or the corresponding Node implementation in the same change.
