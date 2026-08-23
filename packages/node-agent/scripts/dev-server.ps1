[CmdletBinding()]
param(
    [string]$ConfigPath = '',
    [int]$DebounceMilliseconds = 350,
    [int]$HealthTimeoutMilliseconds = 30000,
    [switch]$Stop,
    [switch]$Once,
    [switch]$BuildOnly
)

$ErrorActionPreference = 'Stop'
$packageRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$node = (Get-Command node.exe -ErrorAction Stop).Source
$dataDir = if ($env:CTMCP_DATA_DIR) { [System.IO.Path]::GetFullPath($env:CTMCP_DATA_DIR) } else { Join-Path $env:LOCALAPPDATA 'CodingToolsMCPNode' }
if ([string]::IsNullOrWhiteSpace($ConfigPath)) {
    $ConfigPath = if ($env:CTMCP_CONFIG_FILE) { $env:CTMCP_CONFIG_FILE } else { Join-Path $dataDir 'agent.json' }
}
$ConfigPath = [System.IO.Path]::GetFullPath($ConfigPath)
$statusPath = Join-Path $dataDir 'dev-server-status.json'
$supervisorLog = Join-Path $dataDir 'dev-server-supervisor.log'
$agentLog = Join-Path $dataDir 'dev-server-agent.log'
$script = Join-Path $PSScriptRoot 'dev-server.mjs'

function Stop-Tree([int]$ProcessId) {
    if ($ProcessId -le 0) { return }
    & taskkill.exe /PID $ProcessId /T /F 2>$null | Out-Null
}

function Read-Status {
    if (-not (Test-Path -LiteralPath $statusPath -PathType Leaf)) { return $null }
    try { return Get-Content -Raw -LiteralPath $statusPath | ConvertFrom-Json } catch { return $null }
}

if ($Stop) {
    $status = Read-Status
    if ($null -eq $status) {
        Write-Host 'Node Agent dev supervisor is not running (no status file).'
        exit 0
    }
    if ($status.agentPid) { Stop-Tree ([int]$status.agentPid) }
    if ($status.supervisorPid) { Stop-Tree ([int]$status.supervisorPid) }
    Write-Host "Stopped Node Agent dev supervisor pid=$($status.supervisorPid)."
    exit 0
}

New-Item -ItemType Directory -Path $dataDir -Force | Out-Null
$args = @(
    $script,
    '--config', $ConfigPath,
    '--status', $statusPath,
    '--agent-log', $agentLog,
    '--debounce-ms', [string]$DebounceMilliseconds,
    '--health-timeout-ms', [string]$HealthTimeoutMilliseconds
)
if ($Once) { $args += '--once' }
if ($BuildOnly) { $args += '--build-only' }

if ($Once) {
    & $node @args
    exit $LASTEXITCODE
}

$status = Read-Status
if ($null -ne $status -and $status.supervisorPid) {
    $existing = Get-Process -Id ([int]$status.supervisorPid) -ErrorAction SilentlyContinue
    if ($existing) {
        Write-Host "Node Agent dev supervisor is already running pid=$($status.supervisorPid)."
        Write-Host "Status: $statusPath"
        exit 0
    }
}

function Quote-Arg([string]$Value) {
    return '"' + ($Value -replace '"', '\"') + '"'
}

$parts = @((Quote-Arg $node)) + @($args | ForEach-Object { Quote-Arg ([string]$_) })
$commandLine = 'cmd.exe /d /s /c "' + ($parts -join ' ') + ' >> ' + (Quote-Arg $supervisorLog) + ' 2>&1"'
$created = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = $commandLine }
if ([int]$created.ReturnValue -ne 0 -or [int]$created.ProcessId -le 0) {
    throw "Failed to start detached Node Agent dev supervisor: $($created.ReturnValue)"
}
Write-Host "Node Agent dev supervisor scheduled pid=$($created.ProcessId)."
Write-Host "Status: $statusPath"
Write-Host "Supervisor log: $supervisorLog"
Write-Host "Agent log: $agentLog"
