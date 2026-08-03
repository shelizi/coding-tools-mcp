$ErrorActionPreference = 'Stop'
& (Join-Path $PSScriptRoot 'cargo-local.ps1') test --manifest-path src-tauri/Cargo.toml --lib runtime_benchmark -- --ignored --nocapture --test-threads=1
exit $LASTEXITCODE
