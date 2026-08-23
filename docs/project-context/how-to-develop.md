# 如何開發

## 基本流程

1. 先讀 `AGENTS.md`、本專案上下文與相關 spec。
2. 依 `.agents/skills/mcp-probe-kit/SKILL.md` 路由工作；大改先做 code insight／impact。
3. 修改任何 function、class 或 method 前，用 GitNexus `impact` 檢查 upstream blast radius。
4. 保持 Desktop、Node Agent 與 shared contract parity；不適用時更新 parity manifest 與對應 TODO。
5. 執行與風險相稱的驗證。
6. Commit 前執行 GitNexus `detect_changes`，只 stage 本次範圍。

## 開發環境

```powershell
pnpm install --frozen-lockfile
pnpm run hooks:install
pnpm run desktop
```

- `pnpm run desktop`：啟動 Vite 與 Tauri Desktop。
- `pnpm run dev`：只啟動前端，不能代替 Desktop runtime 驗證。
- `pnpm run node-agent:start`：啟動已建置的 Node Agent。
- Node Agent 需要 Node.js 22 或更新版本。

### 快速 reload 開發模式

Node Agent server 開發使用 health-checked hot restart：

```powershell
pnpm --filter @coding-tools/node-agent run dev:server
pnpm --filter @coding-tools/node-agent run dev:server:stop
```

`dev:server` 會監看 `packages/node-agent/src`、sandbox assets 與 server build inputs。變更後先執行 `build:server`；只有 build 成功才停止目前 Node Agent、啟動新 build 並等待所有已保存 Workspace 的 `/health` 恢復。Build 失敗時目前 Agent 保持運行，因此開發中的 MCP connector 不會因編譯錯誤被主動切掉。狀態與 supervisor/Agent logs 位於 `CodingToolsMCPNode` data dir。

Rust Desktop 使用 Tauri dev supervisor + Cargo incremental compilation：

```powershell
pnpm run desktop:dev:fast
pnpm run rust:dev:build
```

`desktop:dev:fast` 讓 Tauri CLI 負責 source watch、成功 rebuild 後重啟 dev Desktop；腳本固定 `CARGO_INCREMENTAL=1`，並將 workspace-specific `CARGO_TARGET_DIR` 放到 LocalAppData，避免把大量增量編譯 cache 放在 Dropbox worktree。`rust:dev:build` 可單獨暖 cache 或驗證一次 incremental Desktop build。

## 專案常用指令

```powershell
pnpm run check
pnpm run build
pnpm run rust:check
pnpm run rust:test
pnpm run node-agent:test
```

較完整驗證：

```powershell
pnpm run verify
pnpm run rust:test:full
pnpm run node-agent:verify-repo
```

## Rust／Node parity

當 Desktop Client 行為也存在於 `packages/node-agent`：

- 同一變更同步更新 Rust 與 Node；或
- 在 `docs/todo/node-agent-parity/manifest.json` 記錄為待辦／刻意差異。

Client 或 shared-contract 變更提交前至少執行：

```powershell
pnpm run version:check
pnpm run node-agent:parity:check
pnpm run node-agent:verify-repo
```

Rust tool catalog 是 Node generated catalog 的來源，不要手動重構 `packages/node-agent/src/rustCatalog.generated.ts`。Schema 變更應使用 Node Agent 的 `sync:rust-contract` 流程產生。

## 模組化原則

- 公開 facade 穩定，先抽純函式或 storage/transaction/runner，再移動 orchestration。
- Rust patch tools 以 `tools/patch.rs` 作公開 facade，跨 domain 回歸測試集中在 `tools/patch/tests.rs`；apply/edit orchestration、file operations、parser、hunk、precise edit、proposal、transaction 與共用 support 各自保有單一模組責任。
- Rust session tools 以 `tools/session.rs` 保有唯一的 store/session state、finalization/status primitives 與 snapshot/public tool facades；`ExecSession` constructors、Windows process-tree attach 及 execution identity/active-slot/sensitive-output/telemetry builders 位於同型別 extension impl `tools/session/construction.rs`，harness correlation、operation identity getters 與 detach/reattach generation 位於 `tools/session/attachment.rs`，`SessionStore` registry/admission 位於 `tools/session/registry.rs`，工具請求控制位於 `tools/session/control.rs`，stdout/stderr readers、exit waiter、status recording、kill fallback、platform termination 與 bounded change waits 位於 `tools/session/process_lifecycle.rs`，retention pruning 與 finalization side effects 位於無狀態的 `tools/session/lifecycle.rs`，stream buffer/encoding primitives 位於 `tools/session/output.rs`，delta batching、snapshot payload 與 redaction read model 位於 `tools/session/snapshot.rs`，回歸 suite 位於 `tools/session/tests.rs`。attachment/construction/registry/process_lifecycle 不得定義第二份 `SessionStore`／`SessionRegistry`／`ExecSession`，其他 helper 也不得建立另一套 background process owner；公開 snapshot methods 留在 parent facade，helper 使用可被圖譜唯一解析的 `build_*`／`capture_*` 名稱。
- Rust exec workspace/scope/spec/post-check/output/runtime-option parsing 位於 session-free `tools/exec/request.rs`；operation lock、automatic dedupe grace、fingerprint conflict 與 retained-session reattachment 位於 process-start-free `tools/exec/admission.rs`；command fingerprint、operation dedupe identity、Cargo target/resource-lock derivation 位於純 `tools/exec/identity.rs`；resource-lock admission、main-process startup permit/loader retry、session registration、request detachment cleanup、stdin handoff、yield/final snapshot、timeout monitor、main-command skip 與 post-check completion 集中在唯一的 `tools/exec/lifecycle.rs`；allowlisted `pwd`／`ls`／`dir`／`which`／`echo` in-process diagnostics 與 workspace-safe directory resolution 位於零子程序 `tools/exec/native_diagnostic.rs`；bounded parallel post-check、單項 timeout、bounded output、startup diagnostics 與結果彙整位於 session-free `tools/exec/post_check.rs`，且必須走共用 `spawn_with_control`；command/post-check specification、PowerShell selection、program resolution 與 WSL path validation 位於 `tools/exec/spec.rs`；WSL invocation、stdio/environment、Windows process flags 與 script adapters 位於 `tools/exec/runner.rs`；session capacity metadata、process-start diagnostics、workspace error envelope、execution failure 與 post-check result merge 位於 `tools/exec/result.rs`；回歸 suite 位於 `tools/exec/tests.rs`。`tools/exec.rs` 只保留 public command/health facade、native fast path、request/admission/lifecycle delegation 與 response boundary metadata；其他模組或 facade 不得建立第二套 main-process 啟動重試、取消或 session lifecycle owner。
- Rust Built-in tunnel 的純 worker pool/policy 計算放在 `tunnel/builtin/pool_policy.rs`；public snapshot、availability derivation、原子 metrics、policy/pool/recycle/error 更新與 connected-worker RAII guard 放在 `tunnel/builtin/metrics.rs`，由 parent re-export `BuiltinTunnelSnapshot`；request DTO、路徑、local HTTP builder 與 response header 映射放在 `tunnel/builtin/request_mapping.rs`；WebSocket client types、control codec、heartbeat、bounded close handshake 與基本 control frame 收送放在 `tunnel/builtin/protocol_io.rs`；WSS headers／connect timeout／subprotocol negotiation／Challenge-Authenticate-HelloAck 與 initial policy validation 放在 `tunnel/builtin/connection.rs`；回歸 suite 放在 `tunnel/builtin/tests.rs` 並維持 `tunnel::builtin::tests` namespace。五個 production helper 都不得取得 enrollment、worker/channel/task/select-loop lifecycle ownership；metrics 不得建立 channel/task/transport、執行 forwarding 或持有 cancellation，policy/request mapping 不得引用 WebSocket，protocol I/O 不得引用 worker policy、forwarding 或 response streaming，connection 不得發布 policy、發送 Ready、設定 connected 狀態或執行 forwarding。`tunnel/builtin.rs` 保留唯一的 worker/transport/cancellation lifecycle owner 與認證後 policy → Ready → connected handoff；Rust 內部抽離不需複製到 Node，但行為仍須由 worker-policy/forwarding parity assertions 固定。
- Node management 的 hot-apply target 與 tunnel controller 介面只能由 `management/runtimeContract.ts` 定義；`management/types.ts` 可 re-export 並組合 `ConfigStore` options，但 `configStore.ts` 不得反向 import aggregate `management/types.ts`。新增 runtime contract 時優先保持 type-only、無 storage/router/runtime side effects。
- Node `ToolContext` 只能依賴 import-free `toolUsage/contract.ts` 的 store surface；`toolUsage.ts` 負責實作與相容 re-export。contract 不得反向 import `types.ts`、catalog、runtime metadata 或 persistence，避免重建 telemetry/catalog 循環。
- Node conversation 與 durable state 的 data/store surface 分別由無 import 的 `conversation/contract.ts`、`state/contract.ts` 定義；`conversation.ts`／`state.ts` 保留唯一 runtime state 與 persistence owner 並實作 contract，`types.ts` 只依賴 contract 且保留既有 state model re-export。domain handler 只能依賴 type-only `toolDispatch/contract.ts`，不得反向 import `toolDispatch.ts` registry；registry facade 保留 public type re-export。不要為縮行數再拆 conversation/state methods 或 dispatch registry lifecycle。
- MCP 與 Actions 共用 `tools::call_tool`／`call_tool_async`，不得新增旁路。
- Rust 與 Node 可有不同內部結構，但輸入、輸出、錯誤、限制與安全行為必須由 parity tests 固定。
- 長時間 process、retry、tunnel worker 與 cancellation 必須保持單一 lifecycle owner。
- UI mutation 只更新受影響的 Svelte store／畫面區塊；避免整頁重新載入。 Node 建置使用 `pnpm run ui:build:node`。

## 版本與 Portable 發佈

Desktop 版本變更使用：

```powershell
pnpm run version:patch
pnpm run version:sync
pnpm run version:check
```

`version:sync` 會同步 Desktop 版本源，以及 Node Agent 的 `codingTools.clientVersion` 與 generated client version。

Portable build 必須使用 repo 的 `build-portable` skill／腳本：

```powershell
pnpm run desktop:portable
pnpm run node-agent:portable:bundled
pnpm run node-agent:portable:system
```

不要用裸 `cargo build --release` 代替 Desktop portable build，否則可能漏掉 `custom-protocol` 並在封裝後連向 `localhost:1420`。

## GitNexus 維護

```powershell
node .gitnexus/run.cjs status
node .gitnexus/run.cjs analyze --force --pdg --index-only --wal-checkpoint-threshold 67108864
$workspacePath = (Get-Location).Path
node .gitnexus/run.cjs check --cycles --json --repo $workspacePath
```

使用 `--index-only` 可避免分析器改寫使用者尚未提交的 `AGENTS.md`／`CLAUDE.md`。`--force` 可避免 GitNexus 1.6.9 增量分析對已搬移 Rust functions 留下不一致 UID；若本機同時索引多個同名 worktree，cycle check 必須以 `--repo` 指定目前 checkout。Windows 環境的 graph/PDG 可用，但 LadybugDB FTS extension 無法載入；精確 `context`／`impact` 可用，關鍵字 `query` 會降級。

若分析期間出現 LadybugDB WAL checkpoint 輪替失敗，使用分析器建議的較高 threshold 重試；未完成旗標會讓下一次執行自動採 full rebuild 恢復一致索引：

```powershell
node .gitnexus/run.cjs analyze --force --pdg --index-only --wal-checkpoint-threshold 67108864
```

---
*返回索引：[../project-context.md](../project-context.md)*
