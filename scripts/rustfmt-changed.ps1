param(
    [switch]$Check
)

$ErrorActionPreference = 'Stop'
$workspace = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$tracked = & git -C $workspace diff --name-only --diff-filter=ACMRT HEAD -- src-tauri
$untracked = & git -C $workspace ls-files --others --exclude-standard -- src-tauri
$files = @($tracked) + @($untracked) |
    Where-Object { $_ -and $_.EndsWith('.rs') } |
    Sort-Object -Unique |
    ForEach-Object { Join-Path $workspace $_ }

if ($files.Count -eq 0) {
    Write-Host 'No changed Rust files.'
    exit 0
}

$arguments = @('--edition', '2021', '--config', 'skip_children=true')
if ($Check) {
    $arguments += '--check'
}
$arguments += $files

Write-Host "rustfmt changed files: $($files.Count)"
& rustfmt @arguments
exit $LASTEXITCODE
