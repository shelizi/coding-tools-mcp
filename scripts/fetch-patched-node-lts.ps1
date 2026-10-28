[CmdletBinding()]
param(
    [ValidatePattern('^24\.[0-9]+\.[0-9]+$')]
    [string]$Version = '24.22.1',
    [switch]$VerifyOnly
)

if ([version]$Version -lt [version]'24.22.1') { throw 'Refusing to download vulnerable Node.js 24 build' }
# Verified Node LTS binaries live on D:, not in Dropbox or the global PATH.
$ErrorActionPreference = 'Stop'
$root = 'D:\coding-tools-mcp-build\cache\node-runtime'
$cache = Join-Path $root "v$Version"
$name = "node-v$Version-win-x64.zip"
$zip = Join-Path $cache $name
$sums = Join-Path $cache 'SHASUMS256.txt'
$nodeHome = Join-Path $cache "node-v$Version-win-x64"
$exe = Join-Path $nodeHome 'node.exe'
$baseUrl = "https://nodejs.org/dist/v$Version/"

if (-not $VerifyOnly) {
    New-Item -Path $cache -ItemType Directory -Force | Out-Null
    Invoke-WebRequest -UseBasicParsing -Uri ($baseUrl + 'SHASUMS256.txt') -OutFile $sums -TimeoutSec 120
}
if (-not (Test-Path -LiteralPath $sums -PathType Leaf)) {
    throw "Node official checksum manifest is missing: $sums"
}
$match = @(Get-Content -LiteralPath $sums | Where-Object {
    $_ -match "^[a-fA-F0-9]{64}\s+$([regex]::Escape($name))$"
})
if ($match.Count -ne 1) { throw "Expected exactly one official checksum entry for $name" }
$expected = $match[0].Substring(0, 64).ToLowerInvariant()

$zipValid = (Test-Path -LiteralPath $zip -PathType Leaf) -and
    ((Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant() -eq $expected)
if (-not $zipValid -and -not $VerifyOnly) {
    $tempZip = Join-Path $cache ("download-" + [guid]::NewGuid().ToString('N') + '.zip')
    try {
        Invoke-WebRequest -UseBasicParsing -Uri ($baseUrl + $name) -OutFile $tempZip -TimeoutSec 240
        $actual = (Get-FileHash -LiteralPath $tempZip -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $expected) { throw "Official Node archive SHA-256 mismatch" }
        Move-Item -LiteralPath $tempZip -Destination $zip -Force
    } finally {
        if (Test-Path -LiteralPath $tempZip) {
            Remove-Item -LiteralPath $tempZip -Force
        }
    }
}
if (-not (Test-Path -LiteralPath $zip -PathType Leaf) -or
    (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected) {
    throw "A valid Node.js archive is not available: $zip"
}
if (-not (Test-Path -LiteralPath $exe -PathType Leaf) -and -not $VerifyOnly) {
    Expand-Archive -LiteralPath $zip -DestinationPath $cache -Force
}
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
    throw "Node executable missing: $exe"
}
$probe = & $exe -p 'JSON.stringify({version:process.version,platform:process.platform,arch:process.arch})' | ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or $probe.version -ne "v$Version" -or $probe.platform -ne 'win32' -or $probe.arch -ne 'x64') {
    throw "Downloaded executable has unexpected runtime identity"
}
Write-Host "Verified official SHA-256: $expected"
Write-Output "NODE_EXE=$exe"
Write-Output "NODE_HOME=$nodeHome"
Write-Output "NODE_VERSION=$($probe.version)"
