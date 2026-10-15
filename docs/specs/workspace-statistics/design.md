# 设计文档：workspace-statistics

## 概述

對應需求：FR-1、FR-2、FR-3、FR-4、NFR-1 至 NFR-3。新增 opt-in 統計彙總，沿用完整日誌掃描與去敏邊界，UI 不從截斷明細推估總量。

## 技术方案

Svelte 5 共用 StatisticsViewer，使用 SVG 柱狀圖與可存取的桶標籤，不新增圖表套件。Rust／Node 各新增小型統計 helper，處理固定時間窗與計數；原查詢只接入 helper，不重構現有大型 telemetry owner。

資料流：StatisticsViewer → telemetry backend → Desktop command／Node management route → 既有 JSONL query → opt-in StatisticsWindow → bounded buckets + 既有 aggregate／sanitized records。

## 数据模型

- statistics_period：minute／hour／day，可選；未提供不啟用。
- statistics_until_ts_ms：可選正整數 epoch 毫秒，預設 server 現在時間。
- statistics：period、interval_ms、start_ts_ms、until_ts_ms、buckets。
- bucket：started_ts_ms、calls、errors。UTC 對齊，最多 60 桶；採半開時間區間。

## API 设计

Frontend options 增加 statisticsPeriod 與 statisticsUntilTsMs。Desktop read_workspace_telemetry 轉成 snake_case query args，統計時採 all scope。Node 管理路由驗證參數並轉發，同步 telemetryView 與 mapTelemetry 回應。Statistics query 限縮有效時間窗，沿用既有錯誤定義與 diagnostic_event_id 去重，完整 aggregate 與圖表一致。明細上限仍 200，無統計請求維持原行為。

## 文件结构

- 新增 src/lib/components/StatisticsViewer.svelte。
- 新增 src-tauri/src/tools/tool_usage/statistics.rs 與 packages/node-agent/src/toolUsage/statistics.ts。
- 修改 src/routes/workspace/[id]/+page.svelte、src/lib/api/telemetry.ts、src/lib/backend/{types,tauri,node}.ts、src/lib/i18n/catalog.ts。
- 修改 src-tauri/src/commands/telemetry.rs、src-tauri/src/tools/tool_usage.rs、packages/node-agent/src/{toolUsage,managementObservability}.ts。
- 新增聚焦 Node／repository 測試；Rust helper 內建單元測試。

## 设计决策

1. FR-2：單次掃描彙總，而非前端 200 筆明細取樣；保留完整時段計數與既有去重。
2. FR-1／FR-4：採 UTC 桶邊界，避免 DST 固定長度日桶不一致；介面明确標示。
3. FR-3：只呈現已去敏 operation metadata，未記錄命令內容時顯示工具名稱，不補造細節。
4. FR-4：新 helper 與 opt-in args 保持原 telemetry 查詢預設，後端雙側同改。

## 测试策略

固定時間驗證分鐘、小時、日、零桶、邊界与無效時間戳；超過 200 筆與 async 不重計、去重、旋轉留存。管理 API 驗證無效 period／until，backend options／response mapping 與分頁 wiring 測試。D: 執行 targeted Rust／Node tests、Svelte check、UI build、version／parity／node-agent verify-repo。

## 风险评估

Rust query_tool_usage_for_profile GitNexus LOW，3 個直接引用、8 個上游符號，無已識別 execution flows。現有大型 telemetry owner 不搬移；新統計實作隔離。頁面、i18n、Node backend 已有未提交變更，僅精確增補。日誌輪替造成資料缺失以 UI 留存提示說明，不宣稱全歷史。

## 检查清单

- [x] 全部 FR 已對應架構、模型、API 與測試。
- [x] 不新增套件，不變更憑證／安全政策／預設留存。
