# Coding Tools MCP 專案上下文

> 本文件是目前專案架構、開發流程與圖譜文件的入口。

## 專案概覽

| 屬性 | 目前狀態 |
| --- | --- |
| 專案名稱 | Coding Tools MCP |
| Desktop Client | `0.1.43` |
| Node Agent | `0.29.12`，相容 Desktop Client `0.1.43` |
| Node Portable 格式 | `1.1.1` |
| Tunnel protocol/server | `0.2.0` |
| 主要語言 | Rust、TypeScript |
| 桌面技術 | Tauri 2、Svelte 5 |
| Node Web UI | 與 Desktop 共用 Svelte 5（`CTMCP_UI_HOST=node`） |
| 產品形態 | 桌面用戶端、Headless Node Agent、內建 WSS Tunnel Server |

Coding Tools MCP 將本機專案暴露成可持續工作的 MCP Workspace。Desktop 與 Node Agent 共用工具契約、權限、歷史、遙測與 Built-in WSS 行為；Tauri 桌面另外提供 Actions、FRP、Cloudflare、Tray 與原生程序管理。

## 文件導航

- [技術棧](./project-context/tech-stack.md)：目前使用的語言、框架、儲存與建置工具。
- [架構設計](./project-context/architecture.md)：Desktop、Node Agent、Tunnel Server 與共用契約邊界。
- [如何開發](./project-context/how-to-develop.md)：專案工作流、版本與 parity 規則。
- [如何測試](./project-context/how-to-test.md)：分層驗證指令與提交門禁。
- [最新圖譜洞察](./graph-insights/latest.md)：GitNexus 索引狀態、核心呼叫鏈與結構風險。
- [設計系統](./design-system.md)：桌面 UI 視覺與互動規範。
- [Node Agent parity manifest](./todo/node-agent-parity/manifest.json)：Rust／Node 行為對齊與刻意差異。
- [Shared workspace config（規劃）](./todo/shared-workspace-config/README.md)：canonical schema 與遷移；規格在 [docs/specs/shared-workspace-config](./specs/shared-workspace-config/requirements.md)。

## 主要程式入口

| 執行面 | 入口 |
| --- | --- |
| Tauri Desktop | `src-tauri/src/main.rs`、`src-tauri/src/lib.rs` |
| Desktop Web UI | `src/routes/+layout.svelte`、`src/routes/workspace/[id]/+page.svelte` |
| Node Agent | `packages/node-agent/src/cli.ts`、`packages/node-agent/src/server.ts` |
| Node Agent Web UI | Shared Svelte UI in `src/` built with `CTMCP_UI_HOST=node` to `packages/node-agent/dist/ui/` |
| Tunnel Server | `services/tunnel-server/src/main.rs` |
| Shared Rust contracts | `crates/command-policy/`、`crates/tunnel-protocol/` |

## 開發快速開始

```powershell
pnpm install --frozen-lockfile
pnpm run hooks:install
pnpm run desktop
```

最小驗證：

```powershell
pnpm run verify:fast
pnpm run rust:check
pnpm run version:check
pnpm run node-agent:parity:check
```

Client 或共用契約改動還必須執行 `pnpm run node-agent:verify-repo`。Portable 發佈必須使用專案的 `build-portable` skill／腳本，不能以裸 `cargo build --release` 取代。

## 歷史參考

`old/` 保存舊版 Python／PySide6 實作與相容性資料，只作行為參考，不是目前執行路徑。新功能應以現行 Rust catalog、Node parity manifest、可執行測試與規格文件為準。

---
*更新時間：2026-08-11*
*本文件索引資料快照：2026-08-11 18:52:42（UTC+08:00）*
*依據：GitNexus 1.6.9 graph + PDG 索引、目前原始碼與版本來源。*
