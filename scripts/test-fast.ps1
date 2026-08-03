param(
    [Parameter(Position = 0)]
    [string]$Filter = ''
)

$ErrorActionPreference = 'Stop'
$args = @('test', '--manifest-path', 'src-tauri/Cargo.toml', '--lib')
if ($Filter) {
    $args += $Filter
}
& (Join-Path $PSScriptRoot 'cargo-local.ps1') @args
exit $LASTEXITCODE
