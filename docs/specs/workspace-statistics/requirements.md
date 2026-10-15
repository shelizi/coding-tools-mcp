# 需求文档：workspace-statistics

## 功能概述

在每個工作區新增「統計」分頁，讓使用者以每分鐘、每小時、每日圖表查看 MCP 工具存取量與操作內容。Desktop 與 Node Agent 共用 UI，彙總完整留存時間窗而非最近明細樣本。

## 历史经验与坑（来自记忆库）

暫無記憶庫命中。現有 telemetry API 明細最多 200 筆，不能用其計算整段總量；async_session_finalized 是工作完成事件而非第二次呼叫。Devin 工具與編譯輸出留在 D:。

## 术语定义

- 工具存取：telemetry 的 tool_call 事件，沿用現有去重與工具排除規則。
- 時間窗：分鐘顯示最近 60 個分鐘桶、小時最近 24 個小時桶、日最近 30 個日桶，UTC 邊界，包含目前尚未結束的桶。
- 留存：目前與 rotated JSONL 日誌；已輪替刪除或未記錄的資料無法還原。

## 范围边界

In Scope：FR-1 至 FR-4、Desktop／Node parity、去敏與有界彙總。
Out of Scope：所有 HTTP 請求統計、原始 payload、任意自訂區間、新圖表相依套件、修改既有記錄與留存策略。

## 需求列表

### FR-1: 工作區統計分頁與時間粒度

優先級：Must。使用者需要在現有工作區分頁中查看存取趨勢。

#### 验收标准（EARS）

1. WHEN 使用者開啟統計分頁 THEN 系統 SHALL 顯示呼叫／錯誤總數、柱狀圖、操作排行與最近操作。
2. WHEN 使用者切換每分鐘／每小時／每日 THEN 系統 SHALL 查詢對應 60／24／30 桶，包含零值桶並標明 UTC 邊界。
3. WHEN 手動刷新 THEN 系統 SHALL 更新當前時間窗，並在請求期間顯示載入狀態。

### FR-2: 完整留存資料彙總

優先級：Must。使用者需要不受明細上限影響的總量。

#### 验收标准（EARS）

1. WHEN 留存時間窗內呼叫超過 200 筆 THEN 系統 SHALL 彙總全部符合篩選的呼叫，明細僅保留最多 200 筆。
2. IF 事件為 async_session_finalized 或 transport_event THEN 系統 SHALL 不將其計入工具呼叫柱狀圖。
3. WHEN 日誌包含重複 diagnostic_event_id THEN 系統 SHALL 沿用現有去重規則。
4. IF 時間戳缺失、無效或不在 [start, until) THEN 系統 SHALL 排除其統計。

### FR-3: 安全與狀態資訊

優先級：Must。使用者需要可信且安全的操作資訊。

#### 验收标准（EARS）

1. WHEN 顯示操作 THEN 系統 SHALL 僅使用已有去敏工具名稱、時間、結果、耗時等欄位，不增加 payload 洩漏面。
2. IF 無資料或後端失敗 THEN 系統 SHALL 顯示空狀態或可重試的錯誤，不以失敗冒充零次存取。
3. WHEN 切換工作區或快速切換粒度 THEN 系統 SHALL 不顯示前一工作區或過期請求的結果。
4. WHEN 顯示留存統計 THEN 系統 SHALL 說明只計入工具呼叫且受日誌留存限制。

### FR-4: 跨執行面相容與驗證

優先級：Must。Desktop 與 Node 使用者需要一致功能。

#### 验收标准（EARS）

1. WHEN Desktop 與 Node 收到相同固定截止時間與事件 THEN 系統 SHALL 回傳相同桶邊界、呼叫／錯誤數。
2. WHEN 舊 telemetry 查詢未要求統計 THEN 系統 SHALL 保留既有 scope、篩選與回應行为。
3. WHEN 驗證本功能 THEN 系統 SHALL 在 D: 執行測試、typecheck、build 與 parity 門禁。

## 非功能需求

- NFR-1：新增桶計數記憶體最多 60 桶，單次既有日誌掃描完成彙總，不逐桶重複讀取日誌。
- NFR-2：不讀取憑證、不擴充原始 payload、不更動安全政策。
- NFR-3：不新增前端相依套件；遵循 Svelte 5、現有 i18n 與 tx 視覺 token。

## 依赖关系

依賴既有 telemetry JSONL 掃描、管理 API、Tauri command、共用 backend 與分頁導覽。

## 检查清单

- [x] FR-1 至 FR-4 可驗證，包含零值、截斷、失敗與去重邊界。
- [x] 時间窗、留存與排除範圍明確。
