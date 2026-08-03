# 設計：session-activity-monitor

## 決策

本功能整合既有 `HistoryViewer`，不新增獨立頁面。歷史文件仍是 session 內容與明確完成狀態的事實來源；新的記憶體 tracker 只提供當前 runtime 的短暫活動證據。

## 資料流

```text
MCP POST tools/call
  -> 讀取 openai/session
  -> SessionActivityGuard::begin(workspace, session, tool, safe summary)
  -> 執行既有 handle_request_async
  -> guard.complete(outcome) 或 Drop 自動標記 cancelled

HistoryViewer (3 秒 polling)
  -> list_history_sessions Tauri command
  -> history::list_for_ui + session activity snapshot
  -> 以 Markdown completed 優先，否則合併 running/active/inactive
```

## 後端模型

`mcp/session_activity.rs` 維護行程內全域 map，key 為 `(profile_id, host_session_key)`。每筆狀態包含：

- 最後開始／完成／活動時間
- 最後工具、最後安全化動作與結果
- 以 request sequence 為 key 的進行中請求集合

Guard 的 `Drop` 是取消保險：HTTP future 被中止時仍會移除 active request。snapshot 不回傳 host session key，只由歷史文件的 `Session key` 查找。

## 動作摘要

摘要格式為 `tool_name · detail`。detail 僅從以下欄位擇一：

- 檔案／搜尋工具：`path`、`query`、`pattern`
- workspace 路由：`folder_id`
- process 工具：`cmd`、`script`、`program`、`session_id`、`output_ref`
- 批次命令：命令數量

detail 經 `redact_sensitive_text`，敏感路徑只顯示 `[sensitive path]`，最後截斷為 120 個字元。`stdin`、env、token、完整回應內容不會進入 tracker。

## UI

歷史清單和詳情 header 共用四種活動狀態：

- `running`：綠色狀態點，顯示「目前：動作」
- `active`：靛藍狀態點，顯示「最近：動作」
- `inactive`：灰色狀態點，保留最後活動時間
- `completed`：完成標記，不受 runtime tracker 覆蓋

輪詢使用 `onMount` 建立 3 秒 interval、cleanup 清除。背景輪詢使用 silent 模式，不顯示 loading、不重載 detail，使用者手動 refresh 才完整重讀。

## 相容性

`list_for_ui` 增加可選 profile id，僅在 desktop command 傳入。既有測試與其他呼叫可傳 `None`。JSON 只新增欄位，不移除或改名既有欄位。
