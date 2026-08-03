<p align="center">
  <img src="src-tauri/icons/128x128.png" width="96" alt="Coding Tools MCP 圖示">
</p>

<h1 align="center">Coding Tools MCP</h1>

<p align="center">
  把本機專案變成 AI 可直接開發、能跨對話延續上下文的持久工作區。
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Release-see%20releases-blue" alt="Releases">
  <img src="https://img.shields.io/badge/Windows-x64-0078D4?logo=windows" alt="Windows x64">
  <img src="https://img.shields.io/badge/macOS-Apple%20Silicon-000000?logo=apple" alt="macOS Apple Silicon">
  <a href="https://www.apache.org/licenses/LICENSE-2.0"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="Apache-2.0"></a>
</p>

<p align="center">
  <a href="README.md">繁體中文</a> · <a href="README.en.md">English</a> · Releases
</p>

Coding Tools MCP 是一套 Rust + Tauri 2 桌面應用。選好專案目錄並啟動服務後，AI Agent 就能透過 MCP 讀取檔案、修改程式碼、執行指令與測試、查看 Git 狀態，並把關鍵進度存成專案內的歷史工作階段。它比較像「AI 打開一個會記住開發進度的 IDE 工作區」；一般開發工具不需要先建立 Task，歷史工作階段則負責在新對話中還原上下文。

![Coding Tools MCP 工作區總覽](docs/images/workspace-overview.png)

*一個桌面端同時管理工作區、MCP 服務、連線資訊與工作階段還原提示詞。*

## 30 秒看懂怎麼用

```text
下載安裝桌面端
  → 新增專案目錄
  → 啟動 MCP 與公網隧道
  → 複製「公網 MCP 位址」
  → ChatGPT 開啟開發人員模式
  → 新建 MCP 外掛並貼上位址
  → 完成授權，在新對話中開始開發
```

第一次使用只要記住兩件事：**桌面端負責把專案變成 MCP 工作區，ChatGPT 負責透過公網 `/mcp` 位址連上它。**

- [查看完整安裝與桌面端啟動步驟](#五分鐘開始使用)
- [直接查看 ChatGPT 外掛設定](#mcp-connector)

## 五分鐘開始使用

### 1. 安裝桌面用戶端

從目前程式碼託管平台的 Releases 頁面下載對應安裝包：

| 系統 | 安裝包 |
| --- | --- |
| Windows 10/11 x64 | `Coding.Tools.MCP_*_x64-setup.exe` |
| macOS Apple Silicon | `Coding Tools MCP_*_aarch64.dmg` |

macOS 安裝包目前尚未簽名。若系統阻擋首次開啟，請到「系統設定 → 隱私權與安全性」確認允許開啟。

### 2. 新增專案工作區

1. 點左側的「新增工作區」。
2. 選擇專案根目錄。
3. 設定工作區名稱、MCP 連接埠與驗證方式。
4. 儲存後，工作區會長期保留在左側清單中。

### 3. 設定公網隧道

若 AI 用戶端不在本機，需要把本機 MCP 暴露成 HTTPS 位址：

- 在「軟體管理」中安裝或辨識 `frpc` / `cloudflared`。
- 在「FRP 設定」中儲存伺服器、連接埠與 Token，或在工作區選擇 Cloudflare。
- 每個工作區填寫獨立子網域。應用程式會統一管理 FRP 程序與多條代理線路。

![FRP 設定頁面](docs/images/frp-configuration.png)

*FRP 伺服器設定集中保存，各工作區只要選設定檔並填自己的子網域。*

若還沒有可用的 FRPS 伺服端，可參考：[FRPS 伺服端安裝教學（微信公眾號）](https://mp.weixin.qq.com/s/kmpQhHsvmHlaLfj4rw3A0Q)。安裝完成後，把伺服端位址、連接埠與 Token 填入用戶端的「FRP 設定」即可。

### 4. 啟動 MCP

進入工作區並點 MCP 的「啟動」。用戶端會顯示：

- 本機 MCP 位址，例如 `http://127.0.0.1:28766/mcp`；
- 公網 HTTPS MCP 位址；
- ChatGPT 連線所需的驗證資訊；
- 即時日誌與健康檢查結果。

![MCP 本機、公網與 ChatGPT 連線資訊](docs/images/workspace-connection.png)

啟動後可直接檢查本機與公網端點、OAuth 中繼資料與 MCP 受保護資源：

![MCP 健康檢查結果](docs/images/health-check.png)

*健康檢查會逐項顯示連線與驗證中繼資料是否可用。*

遇到連線問題時，不必離開桌面端即可查看最近的 MCP 請求日誌：

![MCP 執行日誌](docs/images/runtime-logs.png)

*日誌可快速確認工具清單、歷史初始化與檢查點呼叫是否真的到達伺服端。*

### 5. 連接 AI 用戶端

支援 MCP 的用戶端使用介面中的公網 MCP URL。使用 OAuth 時，用戶端會依伺服端中繼資料進入授權流程；授權口令、Client ID 與 Secret 都可在桌面端集中產生與管理。目前版本使用預先設定的 OAuth 用戶端，建立 ChatGPT 外掛時應選擇靜態／手動 OAuth 憑證，不需要選擇 CIMD。

首次連線建議先呼叫歷史初始化，再檢查工作區：

```text
history_session_bootstrap
server_info
get_default_cwd
git_status
check_exec_environment
```

這樣 Agent 不必依賴聊天上下文去猜目前專案、工作目錄與執行能力。

## ChatGPT 的兩種接入方式

| 方式 | 適合情境 | 在用戶端使用什麼 |
| --- | --- | --- |
| MCP Connector | ChatGPT 直接使用檔案、指令與 Git 工具 | 工作區的公網 `/mcp` 位址 |
| GPT Actions | 在自訂 GPT 中匯入 OpenAPI 工具 | Actions 面板中的 `/openapi.json` 位址 |

### MCP Connector

設定前請先確認：

1. 工作區的 MCP 服務與公網隧道都在執行中。
2. 「健康檢查」中的公網 MCP 檢查通過；若使用 OAuth，再確認 OAuth 受保護資源與授權中繼資料檢查通過。
3. 從桌面端「GPT 設定」卡片複製「公網 MCP 位址」；若使用 OAuth，同時準備 OAuth Client ID、OAuth Client Secret 與授權口令。

> ChatGPT 必須使用公網 HTTPS `/mcp` 位址，不能使用 `http://127.0.0.1:28766/mcp` 這類本機位址。ChatGPT 的選單名稱可能隨版本與語言設定略有差異。

#### 1. 開啟 ChatGPT 開發人員模式

開啟 ChatGPT 設定，進入「帳戶安全與登入」，開啟「開發人員模式」。此開關允許新增未經驗證的 MCP 連接器。

![在 ChatGPT 中開啟開發人員模式](docs/images/gpt-config-1.png)

*開發人員模式權限較高，只應連線你自己部署或明確可信的 MCP 服務。*

#### 2. 建立 MCP 外掛

在 ChatGPT 左側進入「外掛」，點右上角 `+` 新建外掛，然後選擇 MCP（測試版）並填寫：

| ChatGPT 欄位 | 填寫內容 |
| --- | --- |
| 名稱 | 自訂一個容易辨識的名稱，例如 `Coding Tools MCP` |
| 描述 | 簡要說明它連線的專案或用途 |
| 連線 | 貼上桌面端「GPT 設定」中的公網 MCP 位址，URL 應以 `/mcp` 結尾 |
| 身分驗證 | 與桌面端保持一致；截圖以 OAuth 為例 |

![在 ChatGPT 中新建 MCP 外掛並填寫連線資訊](docs/images/gpt-config-2-detail.png)

使用 OAuth 時，展開「進階 OAuth 設定」，選擇靜態／手動 OAuth 憑證並填寫桌面端提供的 Client ID 與 Client Secret，不需要選擇 CIMD。儲存或連線後，ChatGPT 會開啟授權頁面；輸入桌面端「GPT 設定」卡片中的授權口令完成首次授權。

> Client Secret、授權口令與 Bearer Token 都屬於敏感資訊，不要貼到對話、Issue 或公開截圖中。若桌面端使用 Bearer 或未啟用驗證，請在 ChatGPT 中選擇目前介面提供的對應驗證方式。

#### 3. 驗證連線

建立一個已啟用該外掛的新對話，並傳送：

```text
請使用 Coding Tools MCP 呼叫 server_info、get_default_cwd 和 git_status，
告訴我目前連線的工作區、預設目錄和 Git 狀態。
```

若能回傳目前專案的資訊，代表「桌面端 → 公網隧道 → OAuth → ChatGPT → MCP 工具」鏈路已打通。首次正式開發時，再呼叫 `history_session_bootstrap` 初始化或還原專案歷史。

若 ChatGPT 仍顯示舊的工具清單，請中斷並重新連線外掛，或建立新對話後再驗證一次。

#### 常見問題

| 現象 | 優先檢查 |
| --- | --- |
| ChatGPT 無法連線 | 是否使用公網 HTTPS `/mcp` 位址，而不是 `127.0.0.1`；桌面端公網 MCP 健康檢查是否通過 |
| OAuth 授權失敗 | Client ID、Client Secret 與授權口令是否來自同一工作區；OAuth 中繼資料檢查是否通過 |
| 看不到新增工具 | 中斷並重新連線外掛，然後建立一個新對話 |
| 工具呼叫失敗 | 開啟桌面端「日誌」與「健康檢查」，確認請求是否到達 MCP 服務 |

### GPT Actions

1. 啟動工作區的 Actions 服務。
2. 複製 Actions 面板中的 OpenAPI URL。
3. 在 GPT 編輯器的 Actions 頁面匯入該 URL。
4. 依桌面端設定選擇 None、API Key 或 OAuth。

MCP 與 Actions 可以在同一工作區同時執行，也可以分別使用不同連接埠與子網域。

## 為什麼需要它

- **面向真實開發**：檔案、指令、Git、測試與長時間執行的程序都在同一個 Workspace 中。
- **跨對話持續開發**：新對話可以讀取全部歷史摘要與最近一次完整交接，不必反覆向 AI 解釋專案背景與目前進度。
- **進度可追溯**：每輪任務完成後可儲存結構化檢查點，決策、修改、測試結果與下一步都留在專案目錄中。
- **多工作區管理**：一個桌面用戶端可保存多個專案，並管理各自的 MCP、Actions 與公網位址。
- **連接 ChatGPT 更直接**：內建 Streamable HTTP、OAuth、Bearer Token、OpenAPI、FRP 與 Cloudflare 隧道。
- **預設工具面保持簡單**：穩定的核心工具預設可用，進階 Harness 能力可按需開啟。

## 讓專案記住每次對話

一般聊天紀錄適合回看交流內容，但不適合作為長期開發交接。Coding Tools MCP 會將工作階段進度寫入目前專案的 `docs/history-session/`，讓上下文跟著專案走，而不是困在某一個聊天視窗裡。

![ChatGPT 新工作階段啟動提示詞](docs/images/history-session-prompt.png)

*複製完整提示詞到新工作階段，即可初始化或還原歷史；每輪任務完成後再儲存檢查點。*

它提供三個互相配合的歷史工具：

| 工具 | 作用 |
| --- | --- |
| `history_session_bootstrap` | 新對話開始時初始化或還原專案工作階段；新檔會固化前序工作階段的壓縮摘要，並回傳穩定的 `session_key` 與 `current_path` |
| `history_session_checkpoint` | 每輪任務完成後依 bootstrap 回傳的穩定目標儲存結構化進度；目標不一致時拒絕寫入，避免寫到其他歷史檔 |
| `history_session_validate` | 檢查歷史編號、檔案與工作階段對應；必要時重建衍生索引，不刪除既有歷史 |

典型效果：

```text
對話 1：分析專案 → 修改程式碼 → 執行測試 → 儲存檢查點
                                      ↓
對話 2：讀取歷史摘要與最新交接 → 從上次進度繼續 → 儲存新檢查點
```

歷史檔使用可讀的 Markdown 格式，可隨專案備份或納入 Git，也方便開發者直接審閱與修訂。每個新檔頂部都帶有長度上限的「繼承的歷史摘要」，舊摘要不會遞迴複製；檢查點採冪等寫入，並要求回傳 `ok=true` 且工作階段目標一致後才確認儲存成功。

> 歷史持久化由 AI 呼叫 MCP 工具完成，並非桌面端在背景錄製聊天內容。若用戶端未觸發工具呼叫，伺服端無法憑空感知新的對話或任務進度。

## Agent 可以做什麼

預設 `core` profile 提供一組穩定、可組合的開發工具：

| 類別 | 主要工具 |
| --- | --- |
| 檔案讀取 | `read_file`、`list_dir`、`list_files`、`search_text`、`grep_text`、`view_image` |
| 檔案修改 | `apply_patch` |
| 指令執行 | `exec_command`、`write_stdin`、`read_output`、`kill_session` |
| Git | `git_status`、`git_diff`、`git_log`、`git_show`、`git_blame` |
| 環境 | `server_info`、`check_exec_environment`、`get_default_cwd`、`set_default_cwd` |
| 歷史工作階段 | `history_session_bootstrap`、`history_session_checkpoint`、`history_session_validate` |

典型開發流程：

```text
開啟 Workspace
  → 理解專案與 Git 狀態
  → 搜尋並讀取程式碼
  → 事務化套用 Patch
  → 執行指令與測試
  → 檢查 diff 並提交
```

進階 profile 仍保留專案狀態、操作紀錄等 Harness 能力，但一般檔案修改與指令執行不要求先建立 Task。

## 權限與還原模型

專案採用 Workspace-first 權限模型：

- Workspace 內一般檔案可以讀取、建立、修改、刪除與執行。
- Workspace 外允許完整唯讀：`read_file`、`list_dir`、`list_files`、`search_text`、`view_image`。
- Workspace 外寫入、刪除與執行會被阻擋。
- `.git` 與 `.github` 不能被一般檔案工具、Patch 或直譯器指令破壞。
- Patch 在單次操作內進行預檢與失敗還原；長期還原統一使用 Git，不建立全量 Workspace Snapshot。

> Windows 子程序目前仍是 `policy_only` 執行邊界，回傳中的 `sandbox_enforced: false` 是真實狀態。靜態指令策略不能等同於完整的作業系統檔案系統沙箱。

## 本機開發

環境需求：Node.js 20+、Rust stable，以及目前系統的 [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/)。

```bash
npm install
npm run desktop
```

常用驗證指令：

```bash
npm run check
npm run build
cd src-tauri && cargo test
cd src-tauri && cargo clippy --all-targets -- -D warnings
```

Windows 也可以雙擊 `dev-desktop.cmd`。不要只用 `npm run dev` 驗證桌面應用，它只會啟動 Vite，不會啟動 Tauri 外殼。

## 專案結構

| 路徑 | 作用 |
| --- | --- |
| `src-tauri/src/tools/` | 檔案、Patch、Exec、Git 等共用工具核心 |
| `src-tauri/src/mcp/` | MCP Streamable HTTP 服務 |
| `src-tauri/src/actions/` | ChatGPT Actions OpenAPI 閘道 |
| `src-tauri/src/tunnel/` | FRP / Cloudflare 隧道與程序管理 |
| `src/` | SvelteKit 桌面介面 |
| `old/` | Python 參考實作與相容性基準 |

## 來源倉庫與歸屬

本專案為上游 Coding Tools MCP 的衍生作品，並保留其著作權與授權聲明（見 [NOTICE](NOTICE)）：

- [xyTom/coding-tools-mcp](https://github.com/xyTom/coding-tools-mcp)
- [mybolide/coding-tools-mcp](https://github.com/mybolide/coding-tools-mcp)

## 致謝

感謝 [Linux.do](https://linux.do/) 社群對專案推廣與回饋的支持。

## License

本專案採用 [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0)。

- 完整授權文本：[LICENSE](LICENSE)
- 歸屬與 NOTICE 聲明：[NOTICE](NOTICE)

使用、修改或再散布本專案（含程式碼、文件、實質實作細節或衍生作品）時，須保留著作權聲明、授權聲明與 `NOTICE`，並清楚標註來源。
