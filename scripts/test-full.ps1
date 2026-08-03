$ErrorActionPreference = 'Stop'
$args = @('test', '--manifest-path', 'src-tauri/Cargo.toml')
& (Join-Path $PSScriptRoot 'cargo-local.ps1') @args
exit $LASTEXITCODE
