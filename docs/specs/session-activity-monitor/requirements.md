# 需求：session-activity-monitor

## 目標

將 ChatGPT MCP session 的即時活動整合進既有「歷史工作階段」檢視器，讓使用者不必在歷史與監控兩個頁面間切換，即可判斷 session 正在執行、最近活躍、已閒置或已完成，並查看安全化後的目前／最後動作。

## 功能需求

### FR-1：工具呼叫活動追蹤

1. WHEN 帶有 `_meta["openai/session"]` 的 MCP 工具呼叫開始 THEN 系統 SHALL 以 workspace 與 host session key 記錄開始時間、工具名稱、安全化動作摘要及進行中請求數。
2. WHEN 工具呼叫完成、失敗、取消或連線中斷 THEN 系統 SHALL 結束該活動，更新最後活動時間與結果，且不得永久留下錯誤的執行中狀態。
3. WHEN 工具呼叫不含 host session key THEN 系統 SHALL 保持既有行為且不建立可識別的 session 活動紀錄。

### FR-2：狀態判定

1. WHEN session 尚有一個以上進行中工具呼叫 THEN 系統 SHALL 回報 `running`。
2. WHEN session 沒有進行中工具呼叫且最後活動在 120 秒內 THEN 系統 SHALL 回報 `active`。
3. WHEN session 沒有進行中工具呼叫且最後活動超過 120 秒，或目前 runtime 沒有該 session 的活動證據 THEN 系統 SHALL 回報 `inactive`。
4. WHEN 歷史文件明確標記 `completed` THEN 系統 SHALL 回報 `completed`，且不得以 runtime 活動推論覆蓋。

### FR-3：整合歷史檢視器

1. WHEN 使用者開啟既有歷史工作階段頁籤 THEN 每筆 session SHALL 顯示活動狀態、最後活動時間與目前或最後動作。
2. WHEN 歷史頁保持開啟 THEN client SHALL 每 3 秒重新讀取 session 清單，以更新即時狀態，且元件卸載後停止輪詢。
3. WHEN 使用者正在閱讀某筆 session THEN 背景更新 SHALL 保留目前選取項與詳情，不得每次輪詢重置畫面。
4. WHEN 使用者使用 prefers-reduced-motion THEN 執行中狀態 SHALL 不使用持續脈衝動畫。

### FR-4：安全與相容性

1. 動作摘要 SHALL 僅包含工具名稱及白名單欄位，所有文字先經既有敏感資訊脫敏並限制長度。
2. 活動紀錄 SHALL 僅存在目前應用程式行程記憶體，不新增 host session key 持久化檔案。
3. 既有歷史 Markdown 格式、checkpoint 工具契約、telemetry JSONL 格式及其他 MCP 工具回應 SHALL 保持不變。

## 非功能需求

- NFR-1：追蹤器使用同步鎖的臨界區不得包含 I/O 或工具執行。
- NFR-2：同一 session 的並行請求完成順序不同時，仍須正確顯示剩餘的最新執行中動作。
- NFR-3：後端單元測試覆蓋 running、active、inactive、completed override、並行請求與取消清理。
- NFR-4：前端型別檢查、Svelte 檢查與既有 i18n 測試必須通過。
