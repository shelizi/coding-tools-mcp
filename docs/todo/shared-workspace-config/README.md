# Shared workspace config

**Status:** Phase 0–2 in progress. Parsers exist; Svelte `node-map.ts` reads/writes through canonical. Desktop `profiles.json` is not rewritten yet.
**Spec:** [docs/specs/shared-workspace-config](../../specs/shared-workspace-config/requirements.md)

Do not implement until Phase 0 fixtures are accepted. The shared Svelte UI still maps hosts through `src/lib/backend/node-map.ts`.

## Summary

Canonical **schemaVersion 2** JSON for one workspace (folders, bind, OAuth client id, policy, sandbox, limits, Built-in WSS URL). Secrets stay in each host store. Desktop keeps `profiles.json` as the app envelope; Node keeps the workspace registry. First delivery is adapters + migrate + export/import pack, not a single shared disk path.

Same-machine `%LOCALAPPDATA%\CodingToolsMCP\workspaces\<id>\` is Phase 6 optional.

## Out of scope

Actions, FRP/Cloudflare executables, Bearer/no-auth, Desktop runtime supervisor. Those stay `host.desktop` or capabilities.
