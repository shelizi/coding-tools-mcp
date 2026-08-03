[CmdletBinding()]
param()

function Get-Sha256 {
    param([Parameter(Mandatory = $true)][string]$Path)

    $stream = [System.IO.File]::OpenRead($Path)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        return ([System.BitConverter]::ToString($sha.ComputeHash($stream))).Replace('-', '')
    } finally {
        $sha.Dispose()
        $stream.Dispose()
    }
}

$ErrorActionPreference = 'Stop'
$workspace = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$manifestPath = Join-Path $workspace 'src-tauri\Cargo.toml'
$packageJsonPath = Join-Path $workspace 'package.json'
$versionSyncScript = Join-Path $workspace 'scripts\sync-version.mjs'
$releaseExe = Join-Path $workspace 'src-tauri\target\release\coding-tools-mcp-desktop.exe'

Push-Location $workspace
try {
    Write-Host 'Synchronizing version metadata from package.json...'
    & node $versionSyncScript
    if ($LASTEXITCODE -ne 0) {
        throw "Version synchronization failed with exit code $LASTEXITCODE."
    }

    $packageJson = Get-Content -Raw -LiteralPath $packageJsonPath | ConvertFrom-Json
    $version = [string]$packageJson.version
    if ([string]::IsNullOrWhiteSpace($version)) {
        throw 'package.json does not define a version.'
    }

    Write-Host 'Building frontend assets...'
    & npm run build
    if ($LASTEXITCODE -ne 0) {
        throw "Frontend build failed with exit code $LASTEXITCODE."
    }

    Write-Host 'Building production Tauri executable with custom protocol...'
    & cargo build `
        --release `
        --manifest-path $manifestPath `
        --features custom-protocol `
        --bin coding-tools-mcp-desktop
    if ($LASTEXITCODE -ne 0) {
        throw "Rust release build failed with exit code $LASTEXITCODE."
    }
    if (-not (Test-Path -LiteralPath $releaseExe -PathType Leaf)) {
        throw "Release executable was not produced: $releaseExe"
    }

    $packageName = "Coding.Tools.MCP_${version}_x64_portable"
    $distRoot = Join-Path $workspace 'dist-portable'
    $packageDir = Join-Path $distRoot $packageName
    $portableExe = Join-Path $packageDir 'Coding Tools MCP.exe'
    $zipPath = Join-Path $distRoot "$packageName.zip"
    $stagingRoot = Join-Path ([System.IO.Path]::GetTempPath()) "coding-tools-mcp-portable-$([guid]::NewGuid().ToString('N'))"
    $stagingPackageDir = Join-Path $stagingRoot $packageName
    $stagingExe = Join-Path $stagingPackageDir 'Coding Tools MCP.exe'
    $stagingZip = Join-Path $stagingRoot "$packageName.zip"

    try {
        New-Item -ItemType Directory -Path $stagingPackageDir -Force | Out-Null
        Copy-Item -LiteralPath $releaseExe -Destination $stagingExe
        Compress-Archive `
            -LiteralPath $stagingPackageDir `
            -DestinationPath $stagingZip `
            -CompressionLevel Optimal
        Move-Item -LiteralPath $stagingZip -Destination $zipPath -Force

        New-Item -ItemType Directory -Path $packageDir -Force | Out-Null
        try {
            Copy-Item -LiteralPath $releaseExe -Destination $portableExe -Force
        } catch [System.IO.IOException] {
            Write-Warning "Portable folder was not updated because its executable is running. The ZIP is current: $zipPath"
        }
    } finally {
        $tempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
        $resolvedStaging = [System.IO.Path]::GetFullPath($stagingRoot)
        if ($resolvedStaging.StartsWith($tempRoot, [System.StringComparison]::OrdinalIgnoreCase) -and
            (Test-Path -LiteralPath $resolvedStaging)) {
            Remove-Item -LiteralPath $resolvedStaging -Recurse -Force
        }
    }

    $exeHash = Get-Sha256 -Path $releaseExe
    $zipHash = Get-Sha256 -Path $zipPath
    $exeInfo = Get-Item -LiteralPath $releaseExe
    $zipInfo = Get-Item -LiteralPath $zipPath

    Write-Host "Release executable: $($exeInfo.FullName)"
    Write-Host "Executable bytes: $($exeInfo.Length)"
    Write-Host "Executable SHA-256: $exeHash"
    Write-Host "Portable ZIP: $($zipInfo.FullName)"
    Write-Host "ZIP bytes: $($zipInfo.Length)"
    Write-Host "ZIP SHA-256: $zipHash"
} finally {
    Pop-Location
}
