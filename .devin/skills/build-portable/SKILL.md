---
name: build-portable
description: 編譯 coding-tools-mcp 桌面版的 Windows portable 發布版（單一 exe 並壓成 zip 放到 dist-portable）。
---

# 編譯 portable 版

適用情境：需要把 coding-tools-mcp 桌面版打包成一個可解壓即用的 Windows portable 執行檔，不走 MSI/NSIS 安裝流程。

## 重點

- 必須用 `npx tauri build --no-bundle`，不要只用 `cargo build --release`。
- `cargo build --release` 不會把 Vite 生產前端嵌入 exe，執行時會去連 `localhost:1420` 而失敗。
- `tauri build --no-bundle` 會先跑 `npm run build`，再把前端靜態檔案包進 Rust 執行檔。

## 前置條件

- Node.js 與 npm 可用（若用 scoop 安裝，路徑通常不在預設 PATH）
- Rust toolchain（cargo / rustc）可用，路徑例如 `$env:USERPROFILE\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin`

## 編譯步驟

1. 設定 PATH（依實際安裝位置調整）：

```powershell
$env:PATH = "$env:USERPROFILE\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin;$env:USERPROFILE\scoop\apps\nodejs\current;C:\Windows\System32;C:\Windows"
```

2. 執行 tauri build（只產 exe，不產安裝包）：

```powershell
npx tauri build --no-bundle
```

3. 輸出檔案：`src-tauri\target\release\coding-tools-mcp-desktop.exe`

4. 打包成 portable zip：

```powershell
$version = (Select-String -Path "src-tauri\Cargo.toml" -Pattern '^version\s*=' | ForEach-Object { ($_ -split '"')[1] })
$base = "dist-portable\Coding.Tools.MCP_${version}_x64_portable"

Remove-Item -Path "dist-portable" -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $base -Force | Out-Null
Copy-Item -Path "src-tauri\target\release\coding-tools-mcp-desktop.exe" -Destination "$base\Coding Tools MCP.exe"
Compress-Archive -Path $base -DestinationPath "$base.zip" -Force
```

## 使用與注意

- 先把 zip 解壓到資料夾，再執行 `Coding Tools MCP.exe`；不要直接從 zip 裡點開。
- 目標電腦需有 WebView2 Runtime（Windows 11 / 多數 Windows 10 已內建）。
- `dist-portable` 是 build 產物，不要 commit 到 git。

## 如果需要安裝包

用 `npm run desktop:build` 或 `npx tauri build`（不加 `--no-bundle`），會額外產出 MSI 與 NSIS 安裝包。
