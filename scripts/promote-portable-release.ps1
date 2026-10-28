[CmdletBinding()]
param(
 [Parameter(Mandatory)][ValidateSet('ctmcp-win64','ctnode-win64')][string]$Edition,
 [Parameter(Mandatory)][string]$Archive,
 [Parameter(Mandatory)][string]$Destination,
 [string]$ExpectedCommit='',
 [string]$ExpectedExeHash=''
)
$ErrorActionPreference='Stop'
$Archive=(Resolve-Path -LiteralPath $Archive).Path
$Destination=[IO.Path]::GetFullPath($Destination)
New-Item -ItemType Directory -Path $Destination -Force|Out-Null
$name=[IO.Path]::GetFileName($Archive)
$sha=(Get-FileHash $Archive -Algorithm SHA256).Hash
$canonical=Join-Path $Destination $Edition
$zip=Join-Path $Destination $name
$id=[guid]::NewGuid().ToString('N')
$temp=Join-Path $Destination ('.release-'+$id)
$zipTemp=Join-Path $Destination ('.archive-'+$id)
$backup=Join-Path $Destination ('previous\'+$id)
$dirMoved=$false;$zipMoved=$false;$dirInstalled=$false;$zipInstalled=$false
try {
 Expand-Archive -LiteralPath $Archive -DestinationPath $temp
 $items=@(Get-ChildItem -LiteralPath $temp -Force)
 $src=$temp
 if($items.Count -eq 1 -and $items[0].PSIsContainer){$src=$items[0].FullName}
 if($Edition -eq 'ctnode-win64'){
  $manifest=Get-Content -LiteralPath (Join-Path $src 'portable-manifest.json') -Raw|ConvertFrom-Json
  if($manifest.edition -ne 'bundled-node' -or $manifest.archiveName -ne $name){throw 'Node package identity mismatch'}
  if($ExpectedCommit -and $manifest.gitCommit -ne $ExpectedCommit){throw 'Node Git commit mismatch'}
  $hashes=@{}
  foreach($line in Get-Content -LiteralPath (Join-Path $src 'SHA256SUMS.txt')){if($line -match '^([a-fA-F0-9]{64})  (.+)$'){$hashes[$Matches[2]]=$Matches[1]}}
  foreach($item in @('portable-manifest.json','start-node-agent.bat','runtime/node.exe','app/dist/cli.js','app/dist/server.js','app/dist/ctmcp-protect.exe')){
   $file=Join-Path $src $item
   if(-not $hashes.ContainsKey($item) -or -not(Test-Path $file) -or (Get-FileHash $file -Algorithm SHA256).Hash -ne $hashes[$item]){throw "Node artifact checksum failed: $item"}
  }
 } else {
  $exe=Join-Path $src 'ctmcp.exe'
  if(-not(Test-Path $exe)){throw 'Rust executable missing'}
  if($ExpectedExeHash -and (Get-FileHash $exe -Algorithm SHA256).Hash -ne $ExpectedExeHash){throw 'Rust staged executable mismatch'}
 }
 Copy-Item -LiteralPath $Archive -Destination $zipTemp
 if((Get-FileHash $zipTemp -Algorithm SHA256).Hash -ne $sha){throw 'Archive copy mismatch'}
 try {
  if(Test-Path $canonical){New-Item -ItemType Directory -Force -Path $backup|Out-Null;Move-Item -LiteralPath $canonical -Destination (Join-Path $backup $Edition);$dirMoved=$true}
  Move-Item -LiteralPath $src -Destination $canonical;$dirInstalled=$true
  if(Test-Path $zip){New-Item -ItemType Directory -Force -Path $backup|Out-Null;Move-Item -LiteralPath $zip -Destination (Join-Path $backup $name);$zipMoved=$true}
  Move-Item -LiteralPath $zipTemp -Destination $zip;$zipInstalled=$true
 }catch{
  $failure=$_.Exception.Message
  $restore=@()
  try{if($zipInstalled){Remove-Item -LiteralPath $zip -Force};if($zipMoved){Move-Item -LiteralPath (Join-Path $backup $name) -Destination $zip}}catch{$restore+=("archive "+$_.Exception.Message)}
  try{if($dirInstalled){Remove-Item -LiteralPath $canonical -Recurse -Force};if($dirMoved){Move-Item -LiteralPath (Join-Path $backup $Edition) -Destination $canonical}}catch{$restore+=("directory "+$_.Exception.Message)}
  throw "Publication failed: $failure; restore errors: $($restore -join '; ')"
 }
 [pscustomobject]@{ok=$true;archive=$zip;canonical=$canonical;sha256=$sha;backup=$(if($dirMoved -or $zipMoved){$backup}else{$null})}
}finally{
 if(Test-Path $temp){Remove-Item -LiteralPath $temp -Recurse -Force -ErrorAction SilentlyContinue}
 if(Test-Path $zipTemp){Remove-Item -LiteralPath $zipTemp -Force -ErrorAction SilentlyContinue}
}
