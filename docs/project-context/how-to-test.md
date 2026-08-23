# 如何測試

## 驗證層級

| 層級 | 主要內容 | 入口 |
| --- | --- | --- |
| Desktop frontend | Svelte type/check、repository JS contracts、Vite build | root pnpm scripts |
| Rust Desktop/core | unit、integration、headless、security、tool contracts | `src-tauri` Cargo tests |
| Node Agent | MCP、management、process、edit、tunnel、security、PWA build | `packages/node-agent` scripts |
| Cross-runtime parity | catalog、tool contracts、UI、versions、shared constants | root parity scripts |
| Tunnel Server | enrollment、WSS worker、admin、SQLite | service Cargo tests |
| Portable | packaged binary、layout、startup smoke | project build skills/scripts |

目前原始碼約包含 440 個 Rust Desktop test attributes、286 個 Node Agent tests、47 個 repository-level Node tests，以及 35 個 Tunnel Server test attributes。這些數量只用於顯示測試面規模，不代替實際執行結果。

## 快速驗證

```powershell
pnpm run verify:fast
pnpm run rust:check
```

`verify:fast` 會平行執行 `pnpm run check` 與 root `node --test tests/*.test.mjs`。

## Desktop／Rust

```powershell
pnpm run rust:test
pnpm run rust:test:full
pnpm run rust:check:headless
pnpm run rust:fmt:check
```

- `rust:test`：`src-tauri` library tests。
- `rust:test:full`：完整 `src-tauri` Cargo tests。
- `rust:check:headless`：無 Desktop features 的 library/headless core 驗證。
- Windows 請使用 repo scripts，它們會透過 `cargo-local.ps1` 使用適合本機路徑的 target 目錄。

Exec identity／command specification／runner／result 邊界的聚焦回歸可執行：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/cargo-local.ps1 test --manifest-path src-tauri/Cargo.toml --no-default-features --lib tools::exec::tests
```

此 suite 的實作位於 `src-tauri/src/tools/exec/tests.rs`，但測試 namespace 維持 `tools::exec::tests`；其中包含 workspace/scope/request parsing、Cargo target lock、resolved-command-shape dedupe identity、operation reattachment/conflict/grace，以及經單一 `exec/lifecycle.rs` main-process owner、`session/construction.rs` 建構邊界、`session/attachment.rs` detach/reattach correlation、`session/process_lifecycle.rs` reader/waiter/termination extension 與共用 startup controller 執行 post-check、合併最終 command success 的回歸。零子程序 native diagnostic 的 public contract 另由 `call_tool_contract::native_diagnostics_support_pwd_and_ls_without_a_shell` 覆蓋；`pnpm run node-agent:ui-parity:check` 會固定 `exec/request.rs` 的 session-free request resolution、`exec/admission.rs` 的 process-start-free operation admission、`exec/result.rs` 的 capacity metadata、唯一 lifecycle owner，以及 session state/extensions 邊界，防止實作回流至 public facade 或建立第二份 process/session owner。

Session lifecycle／output／control 邊界的聚焦回歸可執行：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/cargo-local.ps1 test --manifest-path src-tauri/Cargo.toml --no-default-features --lib tools::session::tests
```

此 suite 的實作位於 `src-tauri/src/tools/session/tests.rs`，測試 namespace 維持 `tools::session::tests`；它同時覆蓋 `session/lifecycle.rs` 的 finalization active-slot release，以及 `session/snapshot.rs` 的 delta pagination、encoding boundary、retained output 與 sensitive-data redaction。

## Node Agent

```powershell
pnpm run node-agent:test
pnpm run node-agent:verify
pnpm run node-agent:verify-repo
```

`node-agent:verify-repo` 包含完整 server/UI build、測試、native dependency 檢查、Rust catalog、client version、parity TODO 與 UI parity。

Node management contract 或 config-store 邊界變更可先跑：

```powershell
pnpm --filter @coding-tools/node-agent run build:server
node --import ./test/setup.mjs --test test/management.test.mjs
```

`management runtime contracts remain independent from the config store` 會固定 `configStore.ts → runtimeContract.ts`、`types.ts → {runtimeContract.ts,configStore.ts}` 的單向結構，避免 aggregate management types 再次形成循環。

Node telemetry contract 邊界可先跑：

```powershell
pnpm --filter @coding-tools/node-agent run build:server
node --import ./test/setup.mjs --test test/toolUsage.test.mjs
```

`tool usage context depends on a pure contract instead of the telemetry implementation` 會固定 `types.ts → toolUsage/contract.ts`、contract 無 imports，以及 concrete store 明確實作 contract；其餘 telemetry tests 驗證 persistence、rotation、redaction、aggregation 與 retained-process finalization。

Node conversation、durable state 與 domain dispatch contract 邊界可先跑：

```powershell
pnpm --filter @coding-tools/node-agent run build:server
node --import ./test/setup.mjs --test test/workspaceConversation.test.mjs test/harnessBaseline.test.mjs test/toolDispatch.test.mjs
```

三個結構測試會固定 `types.ts` 不反向依賴 concrete conversation/state store、concrete owners 明確實作 import-free contract、所有 domain handlers 不反向依賴 registry facade，以及 `conversation.ts`／`toolDispatch.ts`／`types.ts` 保留既有 public type re-export。完整驗證後還必須以 `gitnexus check --cycles --json` 確認 import graph 為零循環。

## Parity 與版本門禁

```powershell
pnpm run version:check
pnpm run node-agent:contract
pnpm run node-agent:parity:check
pnpm run node-agent:parity:complete
pnpm run node-agent:ui-parity:check
```

Client／shared-contract 變更不得只以單側測試通過作為完成證據。若行為刻意不同，必須由 parity manifest 的 `intentional_divergences` 明確記錄。

## Tunnel Server

```powershell
cargo test --manifest-path services/tunnel-server/Cargo.toml

# Desktop Built-in tunnel connection/mapping/policy/protocol-I/O/lifecycle regression
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/cargo-local.ps1 test --manifest-path src-tauri/Cargo.toml --no-default-features --lib tunnel::builtin
```

純 `metrics.rs`／`pool_policy.rs`／`request_mapping.rs`／`protocol_io.rs`／`connection.rs` 重構至少要通過上述 Desktop targeted suite、`pnpm run node-agent:parity:check` 與 `pnpm run node-agent:verify-repo`。metrics 抽離要固定 snapshot/availability、policy/pool counters、recycle/error counters 與 connected-worker guard，並以結構檢查防止 watch/mpsc channel、task、transport、forwarding、streaming、select loop 或 cancellation lifecycle 移入 helper；protocol I/O 抽離還要固定 control codec、heartbeat deadline、Ping/Pong、close handshake；authenticated connection 抽離還要固定 headers、connect timeout、subprotocol、Challenge／Authenticate／HelloAck 與 initial policy validation，並鎖定 parent 的 policy publication → Ready → connected handoff。若 Dropbox/Windows 對 generated `packages/node-agent/dist` 產生 `EBUSY`，先確認沒有同時執行兩個共用 `dist` 的 build，再以官方 clean/build 入口做有上限的序列重試；wrapper 尚未完整通過前不可視為成功。協定或 enrollment 變更還應分別驗證 HTTP enrollment、WSS handshake、worker policy、request forwarding、heartbeat 與 cancellation；Docker Compose 宣告不等於已部署 runtime 證據。

上述 17 個 parent-level tunnel 回歸案例位於 `src-tauri/src/tunnel/builtin/tests.rs`，其中 `availability_state_distinguishes_running_from_reconnecting`、`connected_worker_guard_keeps_live_count_exact` 與 `server_policy_updates_pool_metrics` 固定 metrics 行為；另有 protocol-I/O 與 request-mapping 各 1 個子模組測試，因此 targeted suite 總數維持 19。`pnpm run node-agent:ui-parity:check` 會防止回歸 suite 回流至 lifecycle owner、鎖定 metrics helper 的無 lifecycle 邊界，並確認三個 worker-pool integration tests 仍存在於外部子模組。

## Portable

Portable 發佈需使用對應 skill／腳本，並驗證：

- ZIP 內版本與檔名一致；
- Desktop 帶 `custom-protocol`，不連向 Vite development server；
- Node bundled/system editions 都通過 startup smoke；
- 版本化 ZIP 保留，stable 展開目錄只在成功後更新。

## 回報規則

- 明確區分「targeted tests passed」、「full suite passed」、「build passed」與「runtime smoke passed」。
- 既有失敗要標明，不得把部分 suite 說成全部通過。
- HTTP 200 不代表端到端成功；MCP、Tunnel 與 UI smoke 必須檢查 consumer 需要的 response/body/state。

---
*返回索引：[../project-context.md](../project-context.md)*
