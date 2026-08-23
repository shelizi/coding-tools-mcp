# 技術棧

## 版本基線

| 元件 | 版本／要求 |
| --- | --- |
| Desktop Client | `0.1.43` |
| Node Agent | `0.29.12` |
| Node Agent portable format | `1.1.1` |
| Tunnel protocol/server | `0.2.0` |
| Rust | stable，edition 2021 |
| Node.js | Node Agent `>=22` |
| TypeScript | `~5.6.2` |

## Desktop

| 類別 | 技術 | 用途 |
| --- | --- | --- |
| Desktop shell | Tauri 2 | 視窗、Tray、IPC、portable/native bundle |
| Frontend | Svelte 5、SvelteKit、Vite 6、Tailwind CSS 4 | Desktop UI |
| Backend | Rust、Tokio | runtime、process、file、network concurrency |
| HTTP | Axum 0.8、tower-http | MCP、Actions、OAuth listeners |
| HTTP client/WSS | reqwest、tokio-tungstenite | health、download、Built-in tunnel |
| Data/serialization | serde、serde_json | profile、tool contract、protocol payload |
| Secret protection | Windows DPAPI；Unix AES-256-GCM | Desktop secrets at rest |
| File/image | ignore、walkdir、regex、glob、similar、image | workspace tools |
| Native Windows | `windows` crate | Job Object、process、network、DPAPI、shell |

## Node Agent

| 類別 | 技術 | 用途 |
| --- | --- | --- |
| Runtime | Node.js 22+、TypeScript ESM | Headless MCP Agent |
| HTTP/WSS | Node HTTP、`ws` | MCP、management API、Built-in tunnel |
| Web UI | 與 Desktop 共用 Svelte 5／SvelteKit（`CTMCP_UI_HOST=node`，`/ui/`） | Loopback management UI |
| Server state | Svelte 5 runes + `FrontendBackend` | Desktop `invoke`；Node `/admin/api/*` |
| Image | `jpeg-js`、`pngjs` | portable-friendly image inspection |
| Secret protection | Node crypto AES-256-GCM | encrypted secret file + key backup |

## Tunnel Server 與共用 crates

| 元件 | 技術 |
| --- | --- |
| Tunnel Server | Rust、Axum WebSocket、Tokio、rusqlite bundled、tracing |
| Authentication | Argon2、Ed25519、SHA-256、constant-time comparison |
| Shared protocol | `crates/tunnel-protocol` |
| Shared command policy | `crates/command-policy` |

## 開發與品質工具

| 類別 | 工具 |
| --- | --- |
| JS package/build | pnpm workspace、Vite、SvelteKit |
| Rust build/test | Cargo、rustfmt、Clippy |
| Repository hooks | `.githooks` + `scripts/precommit.mjs` |
| Contract/parity | `scripts/check-node-agent-parity.mjs`、UI parity checker、generated Rust catalog |
| Code graph | GitNexus 1.6.9，graph + PDG |
| Windows scripts | PowerShell、專案內 `cargo-local.ps1` |

## 發佈形態

- Desktop：Tauri installer 與 Windows portable ZIP；必須帶 `custom-protocol` feature。
- Node Agent：bundled-node 與 system-node 兩種 Windows portable ZIP。
- Tunnel Server：Rust binary 或 Docker image；資料與 logs 應掛載持久化目錄。

---
*返回索引：[../project-context.md](../project-context.md)*
