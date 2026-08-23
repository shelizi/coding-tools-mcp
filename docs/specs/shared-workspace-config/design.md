# Shared workspace config — design

## Decision

**Canonical document is the interchange and the long-term on-disk workspace unit.**
Each host keeps its own envelope and secret store. Do not merge Desktop Roaming `profiles.json` with Node `%LOCALAPPDATA%\CodingToolsMCPNode` in the first delivery.

```text
Workspace pack / canonical JSON
        │
        ├─ Desktop adapter  →  AppData envelope (profiles.json) + DPAPI secrets
        └─ Node adapter     →  workspace-profiles.json + agent.json + agent-secrets.enc.json
```

UI continues to speak `WorkspaceProfile`. Adapters map canonical ↔ host types. `src/lib/backend/node-map.ts` is replaced, not extended.

## Canonical document

JSON object, camelCase (align with Node files users already edit), `schemaVersion: 2`.

```json
{
  "schemaVersion": 2,
  "id": "…",
  "name": "repo",
  "folders": [
    { "id": "…", "name": "repo", "path": "E:\\\\work\\\\repo" }
  ],
  "activeFolderId": "",
  "bind": { "host": "127.0.0.1", "port": 3789 },
  "publicBaseUrl": "",
  "auth": { "type": "oauth", "oauthClientId": "chatgpt" },
  "toolProfile": "core",
  "permissionMode": "trusted",
  "securityPolicy": { "restrictToolCatalog": true },
  "policy": {
    "allowedCommands": [],
    "workspaceLocalEntries": true,
    "workspaceScriptExtensions": [],
    "maxPatchBytes": 1048576
  },
  "sandbox": {
    "enabled": false,
    "backend": "appcontainer",
    "externalPaths": [],
    "options": {}
  },
  "limits": {
    "blockingConcurrency": 128,
    "processConcurrency": 64,
    "globalBlockingConcurrency": 1024,
    "globalProcessConcurrency": 512,
    "activeSessionLimit": 512,
    "maxOutputBytes": 1048576,
    "commandTimeoutMaxMs": 0
  },
  "tunnel": {
    "builtin": { "enabled": true, "publicUrl": "" }
  },
  "skills": { "active": true, "disabled": [] },
  "extensions": {
    "hooks": { "active": true, "enabled": [] },
    "mcp": { "active": true, "enabled": [] }
  },
  "host": {
    "desktop": {},
    "node": {}
  }
}
```

Unknown properties at every object level are preserved on rewrite.

### Shared vs host-only

| Shared | `host.desktop` only | `host.node` only |
| --- | --- | --- |
| folders, bind, publicBaseUrl | `actions.*` | `dataDir` |
| OAuth `type=oauth` + clientId | FRP / Cloudflare tunnel fields | `management.enabled` |
| securityPolicy, toolProfile, permissionMode | `auth.type` bearer / none |  |
| command policy, sandbox, limits | `runtime.transportMode` |  |
| Built-in WSS enabled + publicUrl | `useSharedSecrets`, WSL `execution` on folders |  |
| skills / extensions toggles | software / proxy / download live in **app** envelope, not workspace |  |

If Desktop writes `auth.type: bearer`, Node load must keep OAuth-only runtime and leave the desktop field untouched in `host.desktop`.

Folder `execution.kind = wsl` is Desktop-authored. Node may keep the path string but must not invent a WSL supervisor.

## Secrets

Never in canonical JSON or export pack.

| Secret | Desktop | Node |
| --- | --- | --- |
| OAuth password / client secret / token secret | `workspace_secrets` in profiles.json (DPAPI) | `agent-secrets.enc.json` |
| Built-in enrollment URL | workspace/app secrets | `tunnelEnrollmentUrl` |
| Shared secret store | `shared_secrets` | same-machine drop: Rust-wrapped `secrets.json` next to `workspace.json` |

Export pack may include `"secretPresence": { "oauthPassword": true }` so the destination host knows to prompt or generate.

## Persistence after adapters exist

**Node (already close):**

- `workspace-profiles.json` stays the registry (`id`, `name`, `configPath`).
- Each `configPath` becomes schemaVersion 2 canonical JSON (migrate from agent.json v1).
- Secrets stay beside that workspace `dataDir`.

**Desktop:**

- Keep `%APPDATA%\coding-tools-mcp-desktop\data\profiles.json` as the **app envelope**:
  - `profiles[]` values become canonical documents (plus `host.desktop`)
  - `frp_profiles`, `download`, `proxy`, `last_workspace_id`, runtime-enabled ids stay envelope-only
  - secrets stay envelope-only and encrypted
- No requirement to split into one file per workspace in v1 of this spec.

**Same-machine optional later:** `%LOCALAPPDATA%\CodingToolsMCP\workspaces\<id>\workspace.json` as a shared drop folder. Not required to ship adapters.

## Mapping from today

| Canonical | Desktop `WorkspaceProfile` | Node `AgentConfigDocument` |
| --- | --- | --- |
| `bind.host` / `bind.port` | `runtime.bind_address` / `runtime.local_port` | `host` / `port` |
| `auth.oauthClientId` | `auth.oauth_client_id` | `oauth.clientId` |
| `tunnel.builtin.publicUrl` | `tunnel.public_url` when type is builtin | `tunnel.publicUrl` |
| `policy.allowedCommands` | `runtime.allowed_commands` string | `policy.allowedCommands` array |
| `limits.blockingConcurrency` | `runtime.blocking_admission_limit` | `limits.blockingConcurrency` |

`node-map.ts` already encodes this table. Move it into:

- Rust: `src-tauri/src/workspace/canonical.rs`
- TypeScript (UI + tests): `src/lib/backend/workspace-document.ts`
- `src/lib/backend/node-map.ts` maps Node snapshots through that module
- Tests: golden JSON fixtures under `docs/specs/shared-workspace-config/fixtures/`

Do not keep a third copy in the Svelte UI.

## Import / export

Pack file: `*.ctmcp-workspace.json` = canonical document without `host.node.dataDir` (absolute dataDir is machine-local). Rewrite folder paths only if the user confirms a new root; otherwise keep absolute paths and fail if missing.

Import on Node: `ApplicationConfigStore.addWorkspace` from document (already allocates port / config path).
Import on Desktop: `create_workspace` from document instead of empty defaults.

## Compatibility

- Node `schema_version: 1` agent.json → canonical v2 on first save.
- Desktop current profiles.json → each profile wrapped as canonical on first save; app envelope version bump if needed.
- Failed migrate: leave original file, write `*.bak`, surface error in UI.

## Product boundary

Canonical parse success does not enable Actions, FRP, Bearer, or Desktop supervisor on Node. Capabilities still gate the UI.
