# Computer Use 螢幕座標與截圖縮放

Windows Desktop／Node Agent 使用 `Per-Monitor DPI Aware V2`。螢幕矩形與滑鼠座標均為**實體像素**，因此 Windows 的 125%／150% 顯示縮放不需再次乘倍率。

## 向後相容（預設）

- `desktop_displays` 的 `x, y, width, height` 是各螢幕在虛擬桌面中的實體像素矩形。
- `desktop_screenshot` 預設保留全解析度，`width, height` 仍代表螢幕**實體尺寸**。
- 未傳 `coordinate_reference` 時，`desktop_input` 中的滑鼠動作 `x, y` 是指定螢幕的**區域實體像素**，不是 Windows DIPs。

## 從圖片像素操作（建議）

1. 呼叫 `desktop_screenshot`，必要時可傳 `max_width`、`max_height`，按比例縮小 JPEG（不放大）。
2. 使用回傳的 `image_width, image_height` 辨識圖片座標；`width, height` 則是原始螢幕尺寸。
3. 呼叫 `desktop_input`，在 `actions` 中放入 `click`、`drag`、`move` 或 `scroll`，**原封不動**傳入截圖的 `coordinate_reference`；此時 `x, y` 代表 JPEG 裡的圖片像素，服務端依原始螢幕尺寸換算後才操作滑鼠。
4. 在不同螢幕拖曳時，目標端另外傳 `to_coordinate_reference`（由目標螢幕的截圖取得）；同一螢幕則自動沿用起點的參照。

例如原始螢幕為 3840×2160，截圖設 `max_width: 1920` 後會回傳 `image_width: 1920`、`image_height: 1080`、`resized: true`。在縮圖上的 `(500, 300)` 點擊，對應原螢幕約 `(1000, 600)`。

### 操作後自動回傳截圖（減少一次 MCP 呼叫）

`desktop_input` 加上 `capture_after: true`，會先執行完整鍵盤／滑鼠批次並釋放所有按鍵，再擷取操作後畫面，**在同一次 MCP 工具回應內附上真正的圖片內容**；`structuredContent.screenshot` 保留 `image_width`、`image_height`、`coordinate_reference` 等資訊供下一輪操作使用。截圖失敗時回傳 `screenshot_error`，不會誤認為已取得有效畫面。預設 `capture_after: false`，避免大量自動截圖。

```json
{
  "actions": [{ "type": "move", "display_id": 0, "x": 300, "y": 200 }],
  "capture_after": true,
  "capture_display_id": 0,
  "capture_max_width": 1280,
  "capture_quality": 75
}
```

未指定 `capture_display_id` 時擷取主螢幕；若要看第二螢幕，指定對應的 ID。單次工具呼叫省去額外 MCP 往返，但系統仍需執行截圖操作。Windows 125% DPI 真實測試時，必須用 Per-Monitor V2 的游標查詢才能比較實體位置；一般不具 DPI Awareness 的 PowerShell 查詢可能把實體 `(600,400)` 顯示成邏輯 `(480,320)`。

### 安全保護與限制

- 若螢幕 ID、設備名稱、原點或解析度與截圖時不同，**拒絕操作**，要求重新截圖；不會猜測新的比例。
- 只接受圖片界限內的整數座標；縮圖兩側邊界會映射到螢幕實體邊界。
- `coordinate_reference` 的 `image_width, image_height` 是**實際傳回的 JPEG 像素大小**。如果 MCP 客戶端又二次縮放圖片，必須先把視覺／顯示座標轉回 JPEG 像素；服務端無法得知客戶端視窗實際渲染大小。
- DPI 改變可能造成應用程式重新排版，即使螢幕實體尺寸未變。重要操作前仍應重新截圖。
- 這是 opt-in 的安全座標路徑；未傳參照時仍採原本的實體像素語意。工具名稱則已合併成 `desktop_input`，使用舊 MCP 名稱的客戶端需要遷移。

## 統一輸入工具：3 個工具取代原 7 個

進階 Computer Use 對外只有 `desktop_displays`、`desktop_screenshot`、`desktop_input`。原本的 `desktop_click`、`desktop_drag`、`desktop_scroll`、`desktop_type`、`desktop_key` 已從公開 MCP 目錄移除（**工具名稱不向後相容**；舊 Agent 應改成 `desktop_input`）。

`desktop_input` 的 `actions` 接受最多 64 個有序動作。支援 `click`、`drag`（起終點或 `path` 多點路徑）、`scroll`、`move`、`type`、`key` 快捷鍵、`key_down`、`key_up`、`mouse_down`、`mouse_up` 與 `wait`。例如 Ctrl 多選：

```json
{
  "actions": [
    {"type": "key_down", "keys": ["CTRL"]},
    {"type": "click", "x": 160, "y": 210, "button": "left"},
    {"type": "click", "x": 160, "y": 250, "button": "left"},
    {"type": "key_up", "keys": ["CTRL"]}
  ]
}
```

單次組合操作亦可直接帶 `modifiers`：

```json
{"actions":[{"type":"drag","x":100,"y":100,"to_x":500,"to_y":400,"modifiers":["CTRL","SHIFT"],"duration_ms":600}]}
```

- 所有步驟在送出任何輸入事件前預先驗證；任一步驟有無效參數，整批不執行。
- 一次 Windows PowerShell bridge 執行完整順序，共用同一個跨程序輸入 Mutex，避免不同 Agent 的批次互相穿插。
- 發生一般執行錯誤、或使用者忘記配對的 `key_up`／`mouse_up` 時，`finally` 會盡力釋放批次按住的鍵與按鈕。若整個作業系統或橋接程序被強制終止，無法保證執行到釋放程序。
- `key` 快捷鍵最多 8 鍵，`drag` 最多 32 個路徑節點。拖曳＋等待時間總和最多 20 秒。

改動需維持 Rust `src-tauri/src/tools/desktop.rs` 與 Node `packages/node-agent/src/desktopTools.ts` 的輸入／輸出契約一致；共用工具 schema 在 `src-tauri/src/tools/registry_schemas/desktop.rs`，Node generated catalog 從 Rust 匯出。
