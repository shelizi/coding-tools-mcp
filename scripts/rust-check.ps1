$ErrorActionPreference = 'Stop'
& (Join-Path $PSScriptRoot 'cargo-local.ps1') check --manifest-path src-tauri/Cargo.toml
exit $LASTEXITCODE
