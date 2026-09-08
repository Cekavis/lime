param(
  [string]$OutputDirectory = "out/windows-x64/staging/rime"
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Import-Module (Join-Path $PSScriptRoot "common.psm1") -Force

$manifestPath = Resolve-RepoPath $repoRoot "third_party/rime/librime-1.17.0.json"
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
$outputRoot = Resolve-RepoPath $repoRoot $OutputDirectory
Assert-UnderOut $repoRoot $outputRoot
$cacheRoot = Resolve-RepoPath $repoRoot "out/windows-x64/downloads/rime"
$workRoot = Resolve-RepoPath $repoRoot "out/windows-x64/work/rime"
Assert-UnderOut $repoRoot $cacheRoot
Assert-UnderOut $repoRoot $workRoot
New-Item -ItemType Directory -Path $cacheRoot -Force | Out-Null

$librime = $manifest.librime
$ice = $manifest.rime_ice
$librimeBase = "https://github.com/rime/librime/releases/download/$($librime.version)"
$iceBase = "https://github.com/iDvel/rime-ice/releases/download/$($ice.version)"
$archives = @()
foreach ($asset in $librime.assets) {
  $archive = Join-Path $cacheRoot $asset.name
  Invoke-VerifiedDownload "$librimeBase/$($asset.name)" $archive $asset.sha256
  $archives += $archive
}
$iceArchive = Join-Path $cacheRoot $ice.asset
Invoke-VerifiedDownload "$iceBase/$($ice.asset)" $iceArchive $ice.sha256

$sevenZip = Resolve-7Zip
$librimeWork = Join-Path $workRoot "librime"
$depsWork = Join-Path $workRoot "deps"
$iceWork = Join-Path $workRoot "rime-ice"
Invoke-7ZipExtract $sevenZip $archives[0] $librimeWork
Invoke-7ZipExtract $sevenZip $archives[1] $depsWork
if (Test-Path -LiteralPath $iceWork) { Remove-Item -LiteralPath $iceWork -Recurse -Force }
Expand-Archive -LiteralPath $iceArchive -DestinationPath $iceWork -Force

$rimeDll = Get-ChildItem -LiteralPath $librimeWork -Recurse -File -Filter "rime.dll" | Select-Object -First 1
if (-not $rimeDll) { throw "The official librime archive does not contain rime.dll" }
$iceRoot = Get-ChildItem -LiteralPath $iceWork -Directory -ErrorAction SilentlyContinue |
  Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName "build/rime_ice.table.bin") } |
  Select-Object -First 1
if (-not $iceRoot -and (Test-Path -LiteralPath (Join-Path $iceWork "build/rime_ice.table.bin"))) {
  $iceRoot = Get-Item -LiteralPath $iceWork
}
if (-not $iceRoot) { throw "The official Rime-Ice archive does not contain build/rime_ice.table.bin" }

if (Test-Path -LiteralPath $outputRoot) { Remove-Item -LiteralPath $outputRoot -Recurse -Force }
New-Item -ItemType Directory -Path $outputRoot -Force | Out-Null
Copy-Item -LiteralPath $rimeDll.FullName -Destination (Join-Path $outputRoot "rime.dll") -Force
foreach ($dependency in Get-ChildItem -LiteralPath $depsWork -Recurse -File -Filter "*.dll") {
  Copy-Item -LiteralPath $dependency.FullName -Destination (Join-Path $outputRoot $dependency.Name) -Force
}
Get-ChildItem -LiteralPath $iceRoot.FullName -Force | ForEach-Object {
  Copy-Item -LiteralPath $_.FullName -Destination $outputRoot -Recurse -Force
}
$themePath = Resolve-RepoPath $repoRoot "third_party/rime/weasel.yaml"
Copy-Item -LiteralPath $themePath -Destination (Join-Path $outputRoot "weasel.yaml") -Force

[System.IO.File]::WriteAllText((Join-Path $outputRoot "VERSION"), "$($librime.version)`n", [System.Text.UTF8Encoding]::new($false))
[System.IO.File]::WriteAllText((Join-Path $outputRoot "RIME_ICE_VERSION"), "$($ice.version)`n", [System.Text.UTF8Encoding]::new($false))
Copy-Item -LiteralPath $manifestPath -Destination (Join-Path $outputRoot "librime-1.17.0.json") -Force
foreach ($required in $manifest.required_files) {
  $requiredPath = Join-Path $outputRoot $required
  if (-not (Test-Path -LiteralPath $requiredPath)) { throw "Prepared runtime is missing required file: $requiredPath" }
}
Write-Host "Prepared librime $($librime.version) and Rime-Ice $($ice.version) at $outputRoot"
