param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$CargoArgs
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

$base = if ($env:LOCALAPPDATA) {
    $env:LOCALAPPDATA
} else {
    Join-Path $HOME '.cache'
}
$env:CARGO_TARGET_DIR = Join-Path $base "coding-tools-mcp\cargo-target\$hash"
New-Item -ItemType Directory -Path $env:CARGO_TARGET_DIR -Force | Out-Null

Write-Host "CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR"
& cargo @CargoArgs
exit $LASTEXITCODE
