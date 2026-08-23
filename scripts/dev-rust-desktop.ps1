[CmdletBinding()]
param(
    [switch]$OnceBuild,
    [switch]$DryRun,
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$TauriArgs
)

$ErrorActionPreference = 'Stop'
$workspace = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

$sha = [System.Security.Cryptography.SHA256]::Create()
try {
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($workspace.ToLowerInvariant())
    $hash = ([System.BitConverter]::ToString($sha.ComputeHash($bytes))).Replace('-', '').Substring(0, 16).ToLowerInvariant()
} finally {
    $sha.Dispose()
}

$base = if ($env:LOCALAPPDATA) { $env:LOCALAPPDATA } else { Join-Path $HOME '.cache' }
$env:CARGO_TARGET_DIR = Join-Path $base "coding-tools-mcp\cargo-target\$hash"
$env:CARGO_INCREMENTAL = '1'
New-Item -ItemType Directory -Path $env:CARGO_TARGET_DIR -Force | Out-Null

$manifest = Join-Path $workspace 'src-tauri\Cargo.toml'
$tauri = if ($IsWindows -or $env:OS -eq 'Windows_NT') {
    Join-Path $workspace 'node_modules\.bin\tauri.cmd'
} else {
    Join-Path $workspace 'node_modules/.bin/tauri'
}

Write-Host "CARGO_INCREMENTAL=$env:CARGO_INCREMENTAL"
Write-Host "CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR"

if ($DryRun) {
    [pscustomobject]@{
        ok = $true
        cargoIncremental = $env:CARGO_INCREMENTAL
        cargoTargetDir = $env:CARGO_TARGET_DIR
        onceBuild = [bool]$OnceBuild
        command = if ($OnceBuild) { "cargo build --manifest-path $manifest --features desktop" } else { "$tauri dev $($TauriArgs -join ' ')".Trim() }
    } | ConvertTo-Json -Compress
    exit 0
}

if ($OnceBuild) {
    & cargo build --manifest-path $manifest --features desktop
    exit $LASTEXITCODE
}

if (-not (Test-Path -LiteralPath $tauri -PathType Leaf)) {
    throw "Tauri CLI is not installed. Run pnpm install first: $tauri"
}

# Tauri CLI is the process supervisor here: it watches Rust sources, invokes
# Cargo incrementally, and restarts the dev desktop after successful rebuilds.
& $tauri dev @TauriArgs
exit $LASTEXITCODE
