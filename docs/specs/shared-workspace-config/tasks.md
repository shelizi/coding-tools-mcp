# Shared workspace config — tasks

Do not start until this spec is accepted. Current shared-UI merge is not blocked.

## Phase 0 — fixtures

- [x] Add golden canonical v2 JSON fixtures (minimal, full shared fields, desktop-only extras, node-only extras).
- [x] Add v1 Node `agent.json` and current Desktop profile snippets as migrate-from fixtures.

## Phase 1 — parsers

- [x] Node `workspaceDocument.ts`: parse/serialize canonical v2; migrate schema_version 1.
- [x] Rust canonical module: parse/serialize the same fixtures; migrate current `WorkspaceProfile`.
- [x] Roundtrip tests: fixture → host type → fixture; shared fields equal.
- [x] Unknown-field preserve test.

## Phase 2 — adapters (no disk layout change)

- [x] Replace `src/lib/backend/node-map.ts` overlays with canonical parse of Node config snapshots.
- [x] Desktop save/load maps `AppData.profiles[]` through canonical internally (needs lossless `host.desktop` restore first).
- [x] Management GET/PUT still work; Node public JSON can remain v1 on disk until Phase 3.

## Phase 3 — Node on-disk v2

- [x] Write canonical v2 to each workspace `agent.json` (or renamed workspace.json).
- [x] Keep `workspace-profiles.json` as registry.
- [x] Secrets remain in `agent-secrets.enc.json`.

## Phase 4 — Desktop profile embedding

- [x] Persist each Desktop profile as canonical JSON inside `profiles.json`.
- [x] App envelope retains FRP list, proxy, download, last workspace, runtime-enabled ids, secrets.

## Phase 5 — pack

- [x] Export workspace pack (no secrets, `secretPresence` only).
- [x] Import pack on Desktop and Node; allocate host-local port/dataDir; prompt/generate missing OAuth secrets.

## Phase 6 — optional same-machine root

- [x] Only if needed: `%LOCALAPPDATA%\CodingToolsMCP\workspaces\<id>\workspace.json` as a drop target both hosts can open. Not required for Phases 0–5.
- [x] Same-machine drop also shares OAuth secrets in Rust-wrapped `secrets.json`; `workspace.json` is wrapped with the same DPAPI/AES helper.

## Verification

```powershell
pnpm test
pnpm run node-agent:verify-repo
pnpm run rust:test
```
