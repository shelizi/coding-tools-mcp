@echo off
REM Desktop dev launcher (uses @tauri-apps/cli from node_modules)
cd /d "%~dp0"
where pnpm >nul 2>nul
if errorlevel 1 (
  echo ERROR: pnpm was not found in PATH.
  echo Enable Corepack or install pnpm 10.19.0, then run this file again.
  exit /b 1
)
if not exist "node_modules\@tauri-apps\cli" (
  echo Installing pnpm workspace dependencies...
  call pnpm install --frozen-lockfile
  if errorlevel 1 exit /b 1
)
call pnpm run desktop:dev:fast
