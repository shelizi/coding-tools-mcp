# 任務：session-activity-monitor

- [x] 1. 新增 session activity tracker 與 running/active/inactive、並行、取消、脫敏測試。
- [x] 2. 在 MCP listener 工具呼叫生命週期接入 guard，不改既有回應與 telemetry 契約。
- [x] 3. 將 tracker snapshot 合併進歷史 session list/detail，保留 completed 優先級。
- [x] 4. 擴充 TypeScript 型別、HistoryViewer 狀態呈現、3 秒背景輪詢與 i18n 文案。
- [x] 5. 執行 Rust 針對性測試、前端 i18n 測試、Svelte check 與 build。
- [x] 6. 執行 GitNexus detect-changes；變更集中於 MCP listener 與歷史檢視流程。圖譜因共同歷史入口涵蓋 12 條正常／錯誤流程而評為 high，已以全量測試驗證。

## 規格檢查

- [x] 每個需求有可驗證的 EARS 條件。
- [x] 已定義狀態門檻、completed 優先級與無活動證據的行為。
- [x] 已定義取消清理、敏感資訊、並行請求與輪詢 cleanup。
- [x] 已鎖定既有 Markdown、MCP 與 telemetry 相容性。
- [x] requirements、design、tasks 互相可追溯，無占位內容。
