# 任务清单：workspace-statistics

## 概述

所有程式碼保留 Dropbox；D: 同步 workspace 承擔測試與建置。規格已由使用者批准建立。

## 交付物清单（Scope-lock）

新增：3 份規格、StatisticsViewer、Rust／Node statistics helper、聚焦回歸測試。修改：工作區頁面、telemetry API/backend/types/map、i18n、Desktop command/query、Node query/management。新函式集中 helper，既有 owner 不搬移；必要測試接點隨實作核實。

## 任务列表

### 阶段 1: 准备工作

- [x] 1.1 恢復 MCP、更新 D: 圖譜並確認既有存取邊界。
  - 证据块：src-tauri/src/commands/telemetry.rs:30 `limit.unwrap_or(100).clamp(1, 200)`；packages/node-agent/src/managementObservability.ts:130 `limit: integer(searchParams.get('limit'), 100, 1, 200)`。
  - 涉及文件：既有 owner 僅閱讀；新 helper 各少於 250 行。
  - 需求：FR-2、FR-4；設計：技术方案、风险评估。

### 阶段 2: 核心实现

- [x] 2.1 新增兩側 bounded StatisticsWindow 並接入去重後單次掃描。
  - 证据块：src-tauri/src/tools/tool_usage.rs:400 `matched_lines += 1;`；packages/node-agent/src/toolUsage.ts:1821 `matchedLines += 1;`，現有掃描並非只讀回應明細。
  - 涉及文件：兩個 helper 少於 250 行；既有 2705／2016 行 owner 僅增補約 20 行接點，不擴大責任或搬移既有函式。
  - 需求：FR-2、FR-4；設計：数据模型、API 设计。
- [x] 2.2 串接統計 query options 與 response，保留舊 telemetry 預設。
  - 证据块：src/lib/backend/tauri.ts:99 `sinceTsMs: options.sinceTsMs`；src/lib/backend/node.ts:433 `return mapTelemetry(workspaceId, payload);`。
  - 涉及文件：command、management、api、backend、types、node-map，每個接點增補少於 50 行；大型既有 backend 與 management owner 不搬移，新統計 logic 在 helper。
  - 需求：FR-3、FR-4；設計：API 设计。
- [x] 2.3 新增 lazy 統計分頁、圖表、摘要、操作排行與最新去敏紀錄。
  - 证据块：src/routes/workspace/[id]/+page.svelte:832 `activeWorkspaceTab === "telemetry"`；TelemetryViewer.svelte:126 `{#if data && aggregate}`。
  - 涉及文件：StatisticsViewer 少於 350 行，既有頁面僅追加 lazy loader 與分支，不搬移既有 937 行 orchestration；i18n 增加自然四語文案。
  - 需求：FR-1、FR-3；設計：技术方案、设计决策。

### 阶段 3: 集成测试

- [x] 3.1 對照 FR 驗證桶邊界、200 筆截斷、事件排除、去重、空值與 parity。
  - 证据块：packages/node-agent/test/toolUsage.test.mjs:66 `function toolRecord(overrides = {})`；既有 query test 可重用固定 telemetry fixture。
  - 涉及文件：新增聚焦測試各少於 250 行；Rust helper 測試與邏輯同檔。
  - 需求：FR-1、FR-2、FR-3、FR-4；設計：测试策略。

## 检查点

- [x] MCP 與最新圖譜可用，原未提交變更已記錄。
- [x] check_spec 通過後才寫實作。
- [x] 雙側完整彙總與共用 UI。
- [x] D: targeted／typecheck／build／parity 驗證，失敗區分既有與新增。

## 需求覆盖矩阵

| 需求 ID | 設計章節 | 任務編號 | 狀態 |
| --- | --- | --- | --- |
| FR-1 | 技术方案 | 2.3, 3.1 | 已實作；聚焦驗證通過 |
| FR-2 | 数据模型 | 1.1, 2.1, 3.1 | 已實作；聚焦驗證通過 |
| FR-3 | API 设计、设计决策 | 2.2, 2.3, 3.1 | 已實作；聚焦驗證通過 |
| FR-4 | API 设计、测试策略 | 1.1, 2.1, 2.2, 3.1 | 已實作；聚焦驗證通過 |

## 文件变更清单

依 design.md 文件結構逐項交付；新增 UI 與 helpers 各自小於 500 行，既有大型檔僅精確接入，不重構原碼。

## 检查清单

- [x] 所有 FR 均有設計、實作任務與驗證。
- [x] 任務有原碼證據與行數邊界。
- [x] 完成後核對交付物、驗證證據與 Git diff。

## 驗證紀錄

- D: `pnpm run check`：0 errors／0 warnings；Desktop 與 Node UI build 通過，Rust library check／rustfmt check 通過。
- Rust telemetry 測試 20／20；Node statistics／telemetry 31／31；management API 28／28；前端聚焦 6／6。舊 query 不再多回傳 `statistics: null`，保留完整診斷契約。
- 隔離於 D: 的實際瀏覽器：分鐘／小時／日 60／24／30 桶；251／252 次測試呼叫不被 200 筆明細截斷。空狀態、錯誤重試、過期請求防護、繁體中文及不使用 inline chart style 均已驗證。
- Version check、Rust catalog check、native check 與 Node UI parity 7／7 通過。完整 behavioral parity 卡在 WSS tunnel 測試，已停止該驗證行程，未宣稱完整 parity 通過。
- 完整前端測試 79 通過／7 失敗：非統計項目的 build-script 斷言、能力 gating、TunnelConfigForm i18n、parity／tool-contract 數量與同步斷言、UI checklist 字元編碼。完整 Node 測試 596 通過／3 失敗／8 跳過：rustfmt 子行程的 rustup 預設環境、Windows libuv close assertion、WSS split-v5 配對斷言；統計專屬測試均通過。
- Git diff whitespace check 通過；GitNexus detect_changes 已執行，整體 HIGH 含既有 MCP route／dashboard 等其他未提交變更，未提交或重設原有變更。
- 預覽曾繼承正式 `CTMCP_DATA_DIR`，意外覆寫 legacy 日誌並補入測試 client secret。已立即告知並停止 fixture；取得各項明確同意後，以 canonical 副本恢復單一 legacy 日誌、移除匹配 fixture 的新增 `oauthClientSecret`，驗證其他秘密與 key 不變。沒有覆寫前快照，不能宣稱逐位元恢復原 legacy 檔；來源快照、加密備份及報告保留 D:。隔離與路徑檢查要求已寫入 AGENTS.md。
