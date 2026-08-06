<!-- parity-id: NP-013 -->
<!-- parity-status: done -->
# NP-013 — OAuth runtime isolation

- Priority: P1
- Area: authentication
- Status: done

## Gap

Resolved in Node Agent 0.9.0. Each Agent runtime now owns an OAuthRuntime with an isolated pending authorization-code store and clears it when the HTTP server closes.

## Rust evidence

- `src-tauri/src/auth/oauth_flow.rs`
- `src-tauri/src/auth/oauth.rs`

## Node current state

- `packages/node-agent/src/oauth.ts`
- `packages/node-agent/src/server.ts`
- `packages/node-agent/test/oauth.test.mjs`

## Implementation scope

Implemented an OAuthRuntime instance per Agent runtime and routed authorization, token exchange, bearer-token verification, metadata, and lifecycle disposal through it. The implementation remains limited to Rust's fixed-client Authorization Code plus PKCE feature set.

## Acceptance checklist

- [x] Pending authorization codes are isolated by Agent runtime.
- [x] A code issued by runtime A is rejected by runtime B.
- [x] Runtime shutdown clears pending state.
- [x] Expired authorization codes are removed when a new code is issued.
- [x] Existing PKCE, redirect, issuer, audience, and client-secret checks continue to pass.

## Verification

Covered by `packages/node-agent/test/oauth.test.mjs`, `packages/node-agent/test/server.test.mjs`, and the full `npm run verify:repo` suite.
