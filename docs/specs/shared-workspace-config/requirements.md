# Shared workspace config — requirements

## Goal

Desktop（Rust）與 Node Agent 對同一個 Workspace 讀寫同一份設定文件，讓 Workspace 可以在兩邊搬移，不必再靠 `node-map.ts` 欄位對照。

Secrets 仍走各自主機的 secret store，不進共用文件。

## Current split

| | Desktop | Node Agent |
| --- | --- | --- |
| 位置 | `%APPDATA%\coding-tools-mcp-desktop\data\profiles.json` | `%LOCALAPPDATA%\CodingToolsMCPNode\agent.json` + `workspace-profiles.json` |
| 信封 | 一個檔包多個 profile + app settings + secrets | registry 指向多份 agent.json |
| Secrets | 同檔 DPAPI / AES | `agent-secrets.enc.json` |
| JSON 風格 | snake_case | camelCase |

Svelte UI 已用 `WorkspaceProfile`。Node 後端仍是 `AgentConfigDocument`。

## Functional requirements

1. 定義 versioned canonical workspace document（一個 Workspace 一份）。
2. 共用欄位：folders、bind、OAuth client id、security policy、command policy、sandbox、limits、Built-in WSS public URL、skills/extensions 開關。
3. Host-only 欄位放 `host.desktop` / `host.node`；對方必須忽略未知欄位。
4. 文件內不得出現 password、token、client secret、enrollment URL、cloudflare token。
5. Desktop 繼續用現有 app 目錄；Node 繼續用現有 data dir。第一波不強迫兩邊寫同一個磁碟路徑。
6. 兩邊都能匯出／匯入一份 workspace pack（無 secrets），匯入後缺的 secret 由該 host 既有 store 產生或提示填入。
7. 現有 `profiles.json` 與 `agent.json` schema_version 1 必須能遷移到 canonical document。
8. `FrontendCapabilities` 產品邊界不變：Actions、FRP/Cloudflare exe、Bearer/no-auth、runtime supervisor 不因共用文件而在 Node 上發明出來。

## Non-functional

- 遷移失敗時保留原檔，寫 `.bak`。
- Roundtrip：Desktop → canonical → Node → canonical 後共用欄位相等。
- 手動編輯 JSON 仍可用；未知欄位 roundtrip 時保留。

## Acceptance

- Canonical schema 有 fixture 與 Rust/Node parser。
- `node-map.ts` 不再新增 overlay 欄位；讀寫改走 canonical adapter。
- 匯出 pack 不含 secrets；匯入後 MCP OAuth 仍能在目標 host 啟動（secret 由該 host 補齊）。
- Node 忽略 `host.desktop`；Desktop 忽略 `host.node`。
- `pnpm test`、`pnpm run node-agent:verify-repo`、相關 Rust 測試通過。
