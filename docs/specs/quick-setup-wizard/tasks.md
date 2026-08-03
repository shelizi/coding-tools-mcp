# 任務清單：quick-setup-wizard

## 概述

擴充獨立 SvelteKit 快速設定路由，第一步先選三種反向代理，重用現有 software、FRP settings、tunnel、runtime 與完成值元件，以靜態契約、型別檢查與建置驗證。

## 交付物清单（Scope-lock）

- **預計文件數**：9 個
- **預計任務數**：5 個
- **預計新增／修改函數數**：約 14 個頁面／元件事件與驗證函數
- **交付物**：
  1. `docs/specs/quick-setup-wizard/requirements.md`
  2. `docs/specs/quick-setup-wizard/design.md`
  3. `docs/specs/quick-setup-wizard/tasks.md`
  4. `src/lib/components/quick-setup/QuickTunnelSetup.svelte`
  5. `src/routes/quick-setup/+page.svelte`
  6. `src/lib/i18n/catalog.ts`
  7. `tests/quick-setup-wizard.test.mjs`
  8. `src/lib/components/GptQuickCopy.svelte`
  9. `src/lib/components/AppShell.svelte` 與 `src/routes/+layout.svelte` 的既有入口維持不變

## 任务列表

### 階段 1：準備工作

- [x] 1.1 更新三種反向代理需求、資料契約與分支順序
  - **证据块**：`src/lib/components/TunnelConfigForm.svelte:49-99` 定義三種 tunnel secret 與狀態；`src-tauri/src/tunnel/supervisor.rs:808-887` 驗證 FRP 與 Cloudflare 必填值。
  - **涉及文件**：`docs/specs/quick-setup-wizard/*.md`，預算 390 行。
  - _需求：FR-1 至 FR-5_ ｜ _設計：技術方案、設計決策_

### 階段 2：核心實作

- [x] 2.1 新建來源專屬設定元件，整合軟體安裝與 FRP profile，≤500 行
  - **证据块**：`src/lib/api/software.ts:1-23` 提供安裝；`src/lib/api/settings.ts:1-24` 提供 FRP profile；`src/lib/components/TunnelConfigForm.svelte:348-530` 提供既有欄位契約。
  - **涉及文件**：`src/lib/components/quick-setup/QuickTunnelSetup.svelte`，預算 430 行。
  - _需求：FR-3、FR-4、NFR-2_ ｜ _設計：資料模型、API 設計_

- [x] 2.2 修改快速設定頁，將反代選擇設為第一步並依來源保存、測試及啟用，≤500 行
  - **证据块**：`src/routes/quick-setup/+page.svelte:24-184` 是既有四階段編排；`src-tauri/src/workspace/model.rs:88-122` 定義 MCP FRP 公開 URL；`src-tauri/src/tunnel/supervisor.rs:846-887` 定義 Cloudflare 模式。
  - **涉及文件**：`src/routes/quick-setup/+page.svelte`，預算 490 行。
  - _需求：FR-1、FR-2、FR-4、FR-5_ ｜ _設計：架構設計、設計決策 1 至 4_

- [x] 2.3 補齊三來源快速設定 i18n 文案，所有可見文案均透過翻譯 key
  - **证据块**：`src/lib/i18n/catalog.ts` 定義支援 locale 的翻譯；`tests/i18n.test.mjs` 以 Svelte AST 檢查所有語言的可見文字節點，不依賴特定語言字元。
  - **涉及文件**：`src/lib/i18n/catalog.ts`，預算新增 280 行。
  - _需求：FR-1、FR-3、FR-4、NFR-3、NFR-4_ ｜ _設計：測試策略_

- [x] 2.4 支援自訂工作區名稱、多資料夾選取與 MCP PKCE 精簡教學
  - **证据块**：`src/lib/api/workspaces.ts` 已提供具名稱參數的 `createWorkspace` 與 `addWorkspaceFolder`；MCP runtime 明確不要求 Client Secret。
  - **涉及文件**：`src/routes/quick-setup/+page.svelte`、`src/lib/components/GptQuickCopy.svelte`、`src/lib/i18n/catalog.ts`。
  - _需求：FR-2、FR-5、NFR-3_ ｜ _設計：架構設計、設計決策 4_

### 階段 3：整合測試

- [x] 3.1 擴充 wizard 靜態契約並執行 test、check、build
  - **驗收點**：FR-1 三來源第一步；FR-3 軟體與來源欄位；FR-4 三分支保存與啟動；FR-5 完成值。
  - **涉及文件**：`tests/quick-setup-wizard.test.mjs`，預算 110 行。
  - _需求：FR-1 至 FR-5、NFR-1 至 NFR-4_ ｜ _設計：測試策略_

## 檢查點

- [x] 階段 1：規格已通過 `check_spec`。
- [x] 階段 2：三種來源都有獨立必要條件、保存與測試分支；原入口與工作區頁未更動。
- [x] 階段 3：`npm test`、`npm run check`、`npm run build` 全部通過。

## 需求覆盖矩阵

| 需求 ID | 設計章節 | 任務編號 | 狀態 |
|---------|----------|----------|------|
| FR-1 | 架構設計、設計決策 1 | 1.1, 2.2, 2.3, 3.1 | 完成 |
| FR-2 | 架構設計、API 設計 | 2.2, 3.1 | 完成 |
| FR-3 | 資料模型、設計決策 2、3 | 2.1, 2.3, 3.1 | 完成 |
| FR-4 | API 設計、設計決策 3、4 | 2.1, 2.2, 3.1 | 完成 |
| FR-5 | 設計決策 4 | 2.2, 3.1 | 完成 |

## 文件变更清单

| 文件 | 操作 | 行數預算 | 說明 |
|------|------|----------|------|
| `src/lib/components/quick-setup/QuickTunnelSetup.svelte` | 新建 | 430 | 軟體、FRP、Cloudflare 分支設定 |
| `src/routes/quick-setup/+page.svelte` | 修改 | 490 | 五階段 wizard 與三來源啟用 |
| `src/lib/i18n/catalog.ts` | 修改 | +280 | i18n locale 文案 |
| `src/lib/components/GptQuickCopy.svelte` | 修改 | +15 | 快速流程專用 MCP 欄位模式 |
| `tests/quick-setup-wizard.test.mjs` | 修改 | 110 | 三來源流程契約 |

## 交付前自檢

- [x] 無占位符 / TODO / 省略註解
- [x] 交付物數量與 Scope-lock 一致
- [x] 每個程式檔 ≤500 行、每條任務回鏈 FR
