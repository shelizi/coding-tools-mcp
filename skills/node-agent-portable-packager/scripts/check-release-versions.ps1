[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$RepositoryRoot
)

function Assert-SemVer {
    param(
        [Parameter(Mandatory = $true)][string]$Version,
        [Parameter(Mandatory = $true)][string]$Label
    )

    if ($Version -notmatch '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$') {
        throw "$Label is not a valid semantic version: $Version"
    }
}

$ErrorActionPreference = 'Stop'
$repository = (Resolve-Path -LiteralPath $RepositoryRoot).Path
$skillRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$agentPackage = Get-Content -Raw -LiteralPath (Join-Path $repository 'packages\node-agent\package.json') | ConvertFrom-Json
$portableMetadata = Get-Content -Raw -LiteralPath (Join-Path $repository 'packages\node-agent\portable-version.json') | ConvertFrom-Json
$skillVersion = (Get-Content -Raw -LiteralPath (Join-Path $skillRoot 'VERSION')).Trim()

$agentVersion = [string]$agentPackage.version
$portableVersion = [string]$portableMetadata.version
Assert-SemVer -Version $agentVersion -Label 'Node Agent version'
Assert-SemVer -Version $portableVersion -Label 'Portable version'
Assert-SemVer -Version $skillVersion -Label 'Skill version'

$baseName = "Coding.Tools.Node.Agent_${agentVersion}_portable-${portableVersion}"
[ordered]@{
    nodeAgentVersion = $agentVersion
    portableVersion = $portableVersion
    skillVersion = $skillVersion
    independent = $true
    editions = @('bundled-node', 'system-node')
    artifactNames = @(
        "${baseName}_bundled-node_win-x64.zip",
        "${baseName}_system-node_win-x64.zip"
    )
} | ConvertTo-Json
