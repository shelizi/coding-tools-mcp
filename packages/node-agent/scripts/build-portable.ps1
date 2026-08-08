[CmdletBinding()]
param(
    [switch]$SkipVerify,
    [string]$OutputDirectory,
    [string]$NodeExecutable,
    [ValidateSet('all', 'bundled-node', 'system-node')]
    [string]$Edition = 'all'
)

function Get-Sha256 {
    param([Parameter(Mandatory = $true)][string]$Path)

    $stream = [System.IO.File]::OpenRead($Path)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        return ([System.BitConverter]::ToString($sha.ComputeHash($stream))).Replace('-', '').ToLowerInvariant()
    } finally {
        $sha.Dispose()
        $stream.Dispose()
    }
}

function Write-Utf8NoBom {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Content
    )

    $normalized = $Content.Replace("`r`n", "`n").Replace("`n", "`r`n")
    [System.IO.File]::WriteAllText($Path, $normalized, [System.Text.UTF8Encoding]::new($false))
}

function Assert-SemVer {
    param(
        [Parameter(Mandatory = $true)][string]$Version,
        [Parameter(Mandatory = $true)][string]$Label
    )

    if ($Version -notmatch '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$') {
        throw "$Label is not a valid semantic version: $Version"
    }
}

function Write-PackageChecksums {
    param([Parameter(Mandatory = $true)][string]$PackagePath)

    $packagePrefix = $PackagePath.TrimEnd('\') + '\'
    $sumLines = Get-ChildItem -LiteralPath $PackagePath -Recurse -File |
        Where-Object { $_.Name -ne 'SHA256SUMS.txt' } |
        Sort-Object FullName |
        ForEach-Object {
            $fullName = [System.IO.Path]::GetFullPath($_.FullName)
            if (-not $fullName.StartsWith($packagePrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
                throw "Staged file escaped the portable root: $fullName"
            }
            $relative = $fullName.Substring($packagePrefix.Length).Replace('\', '/')
            "$(Get-Sha256 -Path $fullName)  $relative"
        }
    Write-Utf8NoBom -Path (Join-Path $PackagePath 'SHA256SUMS.txt') -Content (($sumLines -join "`n") + "`n")
}

function Publish-PortablePackage {
    param(
        [Parameter(Mandatory = $true)][string]$PackagePath,
        [Parameter(Mandatory = $true)][string]$OutputDirectory,
        [Parameter(Mandatory = $true)][string]$Edition,
        [Parameter(Mandatory = $true)][string]$ExpandedName
    )

    $packageName = Split-Path -Leaf $PackagePath
    $zipPath = Join-Path $OutputDirectory "$packageName.zip"
    $expandedPath = Join-Path $OutputDirectory $ExpandedName
    $expandedStagingPath = Join-Path $OutputDirectory ".$ExpandedName.next-$([guid]::NewGuid().ToString('N'))"

    if (Test-Path -LiteralPath $zipPath) {
        Remove-Item -LiteralPath $zipPath -Force
    }
    Compress-Archive -LiteralPath $PackagePath -DestinationPath $zipPath -CompressionLevel Optimal

    try {
        Copy-Item -LiteralPath $PackagePath -Destination $expandedStagingPath -Recurse
        if (Test-Path -LiteralPath $expandedPath) {
            Remove-Item -LiteralPath $expandedPath -Recurse -Force
        }
        Move-Item -LiteralPath $expandedStagingPath -Destination $expandedPath
    } catch [System.UnauthorizedAccessException] {
        Write-Warning "Expanded portable folder could not be replaced, usually because it is running. The ZIP is current: $zipPath"
    } catch [System.IO.IOException] {
        Write-Warning "Expanded portable folder could not be replaced, usually because it is running. The ZIP is current: $zipPath"
    } finally {
        if (Test-Path -LiteralPath $expandedStagingPath) {
            Remove-Item -LiteralPath $expandedStagingPath -Recurse -Force
        }
    }

    $zipInfo = Get-Item -LiteralPath $zipPath
    [pscustomobject]@{
        edition = $Edition
        packageName = $packageName
        expandedPath = $expandedPath
        zipPath = $zipInfo.FullName
        bytes = $zipInfo.Length
        sha256 = Get-Sha256 -Path $zipPath
    }
}

$ErrorActionPreference = 'Stop'
$packageRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$repositoryRoot = (Resolve-Path (Join-Path $packageRoot '..\..')).Path
$packageJsonPath = Join-Path $packageRoot 'package.json'
$packageLockPath = Join-Path $packageRoot 'package-lock.json'
$portableVersionPath = Join-Path $packageRoot 'portable-version.json'

$packageJson = Get-Content -Raw -LiteralPath $packageJsonPath | ConvertFrom-Json
$portableMetadata = Get-Content -Raw -LiteralPath $portableVersionPath | ConvertFrom-Json
$appVersion = [string]$packageJson.version
$portableVersion = [string]$portableMetadata.version
$minimumNodeMajor = [int]$portableMetadata.minimumNodeMajor
Assert-SemVer -Version $appVersion -Label 'Node Agent version'
Assert-SemVer -Version $portableVersion -Label 'Portable version'

$gitExecutable = (Get-Command git.exe -ErrorAction Stop).Source
$gitCommit = [string](& $gitExecutable -C $repositoryRoot rev-parse HEAD)
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($gitCommit)) {
    throw 'Unable to resolve the repository HEAD commit.'
}
$gitCommit = $gitCommit.Trim()
$statusLines = @(& $gitExecutable -C $repositoryRoot status --porcelain --untracked-files=normal)
if ($LASTEXITCODE -ne 0) {
    throw 'Unable to inspect the repository worktree status.'
}
if ($statusLines.Count -gt 0) {
    throw "Node portable release packaging requires a clean worktree.`n$($statusLines -join "`n")"
}
$gitTag = "node-agent-v${appVersion}-portable-v${portableVersion}"
$tagType = [string](& $gitExecutable -C $repositoryRoot cat-file -t "refs/tags/$gitTag" 2>$null)
if ($LASTEXITCODE -ne 0 -or $tagType.Trim() -ne 'tag') {
    throw "Expected annotated Node portable release tag was not found: $gitTag"
}
$tagCommit = [string](& $gitExecutable -C $repositoryRoot rev-parse "$gitTag^{commit}" 2>$null)
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($tagCommit)) {
    throw "Unable to resolve release tag commit: $gitTag"
}
$tagCommit = $tagCommit.Trim()
if (-not $tagCommit.Equals($gitCommit, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Release tag $gitTag resolves to $tagCommit but HEAD is $gitCommit."
}

if ([string]::IsNullOrWhiteSpace($NodeExecutable)) {
    $nodeCommand = Get-Command node.exe -ErrorAction Stop
    $NodeExecutable = $nodeCommand.Source
}
$NodeExecutable = (Resolve-Path -LiteralPath $NodeExecutable).Path
$npmExecutable = (Get-Command npm.cmd -ErrorAction Stop).Source

$nodeInfo = & $NodeExecutable -p "JSON.stringify({version:process.version,major:Number(process.versions.node.split('.')[0]),platform:process.platform,arch:process.arch})" | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) {
    throw 'Unable to inspect the selected Node.js executable.'
}
if ([string]$nodeInfo.platform -ne 'win32' -or [string]$nodeInfo.arch -ne 'x64') {
    throw "Portable builds require a win32 x64 Node.js runtime; selected runtime is $($nodeInfo.platform) $($nodeInfo.arch)."
}
if ([int]$nodeInfo.major -lt $minimumNodeMajor) {
    throw "Node.js $minimumNodeMajor or later is required; selected runtime is $($nodeInfo.version)."
}

if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $repositoryRoot 'dist-node-portable'
} elseif (-not [System.IO.Path]::IsPathRooted($OutputDirectory)) {
    $OutputDirectory = Join-Path $repositoryRoot $OutputDirectory
}
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null

$basePackageName = "Coding.Tools.Node.Agent_${appVersion}_portable-${portableVersion}"
$editionDefinitions = @()
if ($Edition -eq 'all' -or $Edition -eq 'bundled-node') {
    $editionDefinitions += [pscustomobject]@{
        edition = 'bundled-node'
        packageName = "${basePackageName}_bundled-node_win-x64"
        expandedName = 'Coding.Tools.Node.Agent_portable_bundled-node_win-x64'
        bundled = $true
    }
}
if ($Edition -eq 'all' -or $Edition -eq 'system-node') {
    $editionDefinitions += [pscustomobject]@{
        edition = 'system-node'
        packageName = "${basePackageName}_system-node_win-x64"
        expandedName = 'Coding.Tools.Node.Agent_portable_system-node_win-x64'
        bundled = $false
    }
}

$stagingRoot = Join-Path ([System.IO.Path]::GetTempPath()) "coding-tools-node-agent-portable-$([guid]::NewGuid().ToString('N'))"
$commonPackage = Join-Path $stagingRoot 'common'
$appDirectory = Join-Path $commonPackage 'app'
$nodeLicense = $null

if ($editionDefinitions.bundled -contains $true) {
    $nodeHome = Split-Path -Parent $NodeExecutable
    $nodeLicense = @(
        (Join-Path $nodeHome 'LICENSE'),
        (Join-Path $nodeHome 'LICENSE.txt')
    ) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
    if (-not $nodeLicense) {
        throw "Node.js license was not found next to the runtime: $nodeHome"
    }
}

Push-Location $packageRoot
try {
    if ($SkipVerify) {
        Write-Host 'Building Node Agent (verification skipped by request)...'
        & $npmExecutable run build
    } else {
        Write-Host 'Verifying and building Node Agent...'
        & $npmExecutable run verify
    }
    if ($LASTEXITCODE -ne 0) {
        throw "Node Agent build or verification failed with exit code $LASTEXITCODE."
    }

    New-Item -ItemType Directory -Path $appDirectory -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $commonPackage 'data') -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $commonPackage 'logs') -Force | Out-Null

    Copy-Item -LiteralPath (Join-Path $packageRoot 'dist') -Destination (Join-Path $appDirectory 'dist') -Recurse
    Copy-Item -LiteralPath $packageJsonPath -Destination (Join-Path $appDirectory 'package.json')
    Copy-Item -LiteralPath $packageLockPath -Destination (Join-Path $appDirectory 'package-lock.json')
    Copy-Item -LiteralPath (Join-Path $packageRoot 'README.md') -Destination (Join-Path $appDirectory 'README.md')
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'LICENSE') -Destination (Join-Path $commonPackage 'LICENSE.txt')

    Push-Location $appDirectory
    try {
        Write-Host 'Installing production-only dependencies into portable staging...'
        & $npmExecutable ci --omit=dev --ignore-scripts --no-audit --no-fund
        if ($LASTEXITCODE -ne 0) {
            throw "Production dependency installation failed with exit code $LASTEXITCODE."
        }
        & $npmExecutable ls --omit=dev --all
        if ($LASTEXITCODE -ne 0) {
            throw "Production dependency validation failed with exit code $LASTEXITCODE."
        }
        & $NodeExecutable -e "for (const name of ['ws','pngjs','jpeg-js']) require.resolve(name); console.log('portable production dependencies resolved')"
        if ($LASTEXITCODE -ne 0) {
            throw 'Portable production dependency resolution failed.'
        }
    } finally {
        Pop-Location
    }

    $openBat = @'
@echo off
setlocal EnableExtensions
if not defined CTMCP_PORT set "CTMCP_PORT=3789"
start "" "http://127.0.0.1:%CTMCP_PORT%/ui"
'@

    $startBatTemplate = @'
@echo off
setlocal EnableExtensions
chcp 65001 >nul
title Coding Tools MCP Node Agent

for %%I in ("%~dp0.") do set "PORTABLE_ROOT=%%~fI"
set "AGENT_ENTRY=%PORTABLE_ROOT%\app\dist\cli.js"
__RUNTIME_SETUP__
if not exist "%AGENT_ENTRY%" (
  echo ERROR: Node Agent entry point was not found:
  echo   %AGENT_ENTRY%
  goto :failed
)

if not defined CTMCP_DATA_DIR (
  if defined LOCALAPPDATA (
    set "CTMCP_DATA_DIR=%LOCALAPPDATA%\CodingToolsMCPNode"
  ) else (
    set "CTMCP_DATA_DIR=%USERPROFILE%\AppData\Local\CodingToolsMCPNode"
  )
)
if not defined CTMCP_PORT set "CTMCP_PORT=3789"
if not exist "%CTMCP_DATA_DIR%" mkdir "%CTMCP_DATA_DIR%" >nul 2>nul
if not exist "%PORTABLE_ROOT%\logs" mkdir "%PORTABLE_ROOT%\logs" >nul 2>nul

set "OPEN_BROWSER=1"
if /I "%~1"=="--no-browser" (
  set "OPEN_BROWSER=0"
  shift
)

echo [Coding Tools MCP] Node Agent portable
echo Edition: __EDITION__
echo Runtime: %NODE_EXE%
echo Data:    %CTMCP_DATA_DIR%
echo MCP:     http://127.0.0.1:%CTMCP_PORT%/mcp
echo UI:      http://127.0.0.1:%CTMCP_PORT%/ui
echo Press Ctrl+C to stop.
echo.

if "%OPEN_BROWSER%"=="1" start "" powershell.exe -NoProfile -WindowStyle Hidden -Command "$port=$env:CTMCP_PORT; for($i=0;$i -lt 80;$i++){try{$r=Invoke-WebRequest -UseBasicParsing -TimeoutSec 1 ('http://127.0.0.1:'+$port+'/health'); if($r.StatusCode -eq 200){Start-Process ('http://127.0.0.1:'+$port+'/ui'); exit 0}}catch{}; Start-Sleep -Milliseconds 250}"

:run_agent
"%NODE_EXE%" "%AGENT_ENTRY%" --restart-supervised %*
set "EXIT_CODE=%ERRORLEVEL%"
if "%EXIT_CODE%"=="75" (
  echo.
  echo Restart requested from Web UI. Starting Node Agent again...
  ping 127.0.0.1 -n 2 >nul
  goto :run_agent
)
if not "%EXIT_CODE%"=="0" (
  echo.
  echo Node Agent exited with code %EXIT_CODE%.
  pause
)
exit /b %EXIT_CODE%

:failed
echo.
pause
exit /b 1
'@

    $bundledRuntimeSetup = @'
set "NODE_EXE=%PORTABLE_ROOT%\runtime\node.exe"
if not exist "%NODE_EXE%" (
  echo ERROR: Bundled Node.js runtime was not found:
  echo   %NODE_EXE%
  goto :failed
)
'@

    $systemRuntimeSetup = @"
set "NODE_EXE="
for /f "delims=" %%I in ('where node.exe 2^>nul') do if not defined NODE_EXE set "NODE_EXE=%%~fI"
if not defined NODE_EXE (
  echo ERROR: Node.js $minimumNodeMajor or later for Windows x64 was not found on PATH.
  goto :failed
)
"%NODE_EXE%" -e "const major=Number(process.versions.node.split('.')[0]); if (process.platform !== 'win32' || process.arch !== 'x64' || major < $minimumNodeMajor) { console.error('Node.js $minimumNodeMajor or later for win32 x64 is required; found ' + process.version + ' ' + process.platform + ' ' + process.arch); process.exit(1); }"
if errorlevel 1 goto :failed
"@

    $artifacts = @()
    foreach ($definition in $editionDefinitions) {
        $editionPackage = Join-Path $stagingRoot $definition.packageName
        Copy-Item -LiteralPath $commonPackage -Destination $editionPackage -Recurse

        if ($definition.bundled) {
            $runtimeDirectory = Join-Path $editionPackage 'runtime'
            New-Item -ItemType Directory -Path $runtimeDirectory -Force | Out-Null
            Copy-Item -LiteralPath $NodeExecutable -Destination (Join-Path $runtimeDirectory 'node.exe')
            Copy-Item -LiteralPath $nodeLicense -Destination (Join-Path $runtimeDirectory 'NODE-LICENSE.txt')
            $runtimeSetup = $bundledRuntimeSetup
            $runtimeSummary = "Bundled Node.js: $($nodeInfo.version) win-x64"
            $runtimeInstructions = 'This edition includes Node.js and does not require system Node.js or npm after extraction.'
            $runtimeVersion = [string]$nodeInfo.version
        } else {
            $runtimeSetup = $systemRuntimeSetup
            $runtimeSummary = "Runtime requirement: Node.js $minimumNodeMajor or later, win32 x64, available as node.exe on PATH"
            $runtimeInstructions = 'This edition does not include Node.js. Install a supported Windows x64 Node.js runtime before launch; npm is not required after extraction.'
            $runtimeVersion = $null
        }

        $startBat = $startBatTemplate.Replace('__RUNTIME_SETUP__', $runtimeSetup).Replace('__EDITION__', [string]$definition.edition)
        Write-Utf8NoBom -Path (Join-Path $editionPackage 'start-node-agent.bat') -Content $startBat
        Write-Utf8NoBom -Path (Join-Path $editionPackage 'open-management-ui.bat') -Content $openBat

        $readme = @"
Coding Tools MCP Node Agent Portable
====================================

Node Agent version: $appVersion
Portable package version: $portableVersion
Release tag: $gitTag
Git commit: $gitCommit
Edition: $($definition.edition)
$runtimeSummary

Quick start
-----------
1. Extract the complete ZIP.
2. Double-click start-node-agent.bat.
3. The local Management UI opens after the health endpoint becomes ready.

$runtimeInstructions

Runtime data defaults to %LOCALAPPDATA%\CodingToolsMCPNode so settings, encrypted secrets, tunnel identity, and history are reused across extracted upgrades. Set CTMCP_DATA_DIR before launching to override this path, including using the data directory beside this file for package-local isolation.

Useful launch options
---------------------
start-node-agent.bat --no-browser
start-node-agent.bat --no-browser --no-ui

Default endpoints
-----------------
MCP: http://127.0.0.1:3789/mcp
UI:  http://127.0.0.1:3789/ui

Version policy
--------------
The Node Agent version and Portable package version are independent. This release is bound to $gitTag at commit $gitCommit. The ChatGPT packaging Skill also has its own independent VERSION file and is not embedded in this archive.
"@
        Write-Utf8NoBom -Path (Join-Path $editionPackage 'README-PORTABLE.txt') -Content $readme

        $manifest = [ordered]@{
            schemaVersion = 3
            nodeAgentVersion = $appVersion
            portableVersion = $portableVersion
            edition = [string]$definition.edition
            nodeRuntimeBundled = [bool]$definition.bundled
            nodeRuntimeVersion = $runtimeVersion
            buildNodeVersion = [string]$nodeInfo.version
            platform = 'win32'
            architecture = 'x64'
            minimumNodeMajor = $minimumNodeMajor
            archiveName = "$($definition.packageName).zip"
            gitCommit = $gitCommit
            gitTag = $gitTag
            builtAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
            versions = [ordered]@{
                nodeAgent = 'packages/node-agent/package.json#version'
                portable = 'packages/node-agent/portable-version.json#version'
                skill = 'skills/node-agent-portable-packager/VERSION (not bundled)'
            }
        }
        Write-Utf8NoBom -Path (Join-Path $editionPackage 'portable-manifest.json') -Content ($manifest | ConvertTo-Json -Depth 5)
        Write-PackageChecksums -PackagePath $editionPackage
        $artifacts += Publish-PortablePackage `
            -PackagePath $editionPackage `
            -OutputDirectory $OutputDirectory `
            -Edition $definition.edition `
            -ExpandedName $definition.expandedName
    }

    Write-Host "Node Agent version: $appVersion"
    Write-Host "Portable version: $portableVersion"
    Write-Host "Release tag: $gitTag"
    Write-Host "Git commit: $gitCommit"
    Write-Host "Build Node.js: $($nodeInfo.version) win-x64"
    foreach ($artifact in $artifacts) {
        Write-Host "Edition: $($artifact.edition)"
        Write-Host "Portable ZIP: $($artifact.zipPath)"
        Write-Host "ZIP bytes: $($artifact.bytes)"
        Write-Host "ZIP SHA-256: $($artifact.sha256)"
        Write-Host "Expanded portable: $($artifact.expandedPath)"
    }
} finally {
    Pop-Location
    $tempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
    $resolvedStaging = [System.IO.Path]::GetFullPath($stagingRoot)
    if ($resolvedStaging.StartsWith($tempRoot, [System.StringComparison]::OrdinalIgnoreCase) -and (Test-Path -LiteralPath $resolvedStaging)) {
        Remove-Item -LiteralPath $resolvedStaging -Recurse -Force
    }
}
