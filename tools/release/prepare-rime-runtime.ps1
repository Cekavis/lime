param(
  [string]$OutputDirectory = "target/rime-runtime"
)

$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path
$manifestPath = Join-Path $repoRoot "resources/runtime/librime-1.17.0.manifest.json"
if (-not (Test-Path -LiteralPath $manifestPath)) {
  throw "Missing runtime manifest: $manifestPath"
}
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json

function Resolve-RepoPath([string]$Path) {
  if ([System.IO.Path]::IsPathRooted($Path)) {
    return [System.IO.Path]::GetFullPath($Path)
  }
  return [System.IO.Path]::GetFullPath((Join-Path $repoRoot $Path))
}

function Assert-GeneratedTarget([string]$Path) {
  $targetRoot = [System.IO.Path]::GetFullPath((Join-Path $repoRoot "target"))
  $fullPath = [System.IO.Path]::GetFullPath($Path)
  if (-not $fullPath.StartsWith($targetRoot + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Generated runtime path must remain under ${targetRoot}: $fullPath"
  }
}

function Resolve-7Zip {
  $command = Get-Command 7z.exe -ErrorAction SilentlyContinue
  if ($command) { return $command.Source }
  $command = Get-Command 7za.exe -ErrorAction SilentlyContinue
  if ($command) { return $command.Source }
  $candidates = @(
    "C:\Program Files\7-Zip\7z.exe",
    "C:\Program Files (x86)\7-Zip\7z.exe"
  )
  $found = $candidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
  if ($found) { return $found }
  throw "7-Zip was not found; install 7-Zip or put 7z.exe on PATH"
}

function Invoke-VerifiedDownload([string]$Url, [string]$Path, [string]$Sha256) {
  if (Test-Path -LiteralPath $Path) {
    $existing = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($existing -eq $Sha256.ToLowerInvariant()) {
      Write-Host "Using verified cache: $Path"
      return
    }
    Remove-Item -LiteralPath $Path -Force
  }
  Write-Host "Downloading $Url"
  & curl.exe -L --fail --retry 3 --output $Path $Url
  if ($LASTEXITCODE -ne 0) {
    throw "Download failed with exit code ${LASTEXITCODE}: $Url"
  }
  $actual = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
  if ($actual -ne $Sha256.ToLowerInvariant()) {
    throw "SHA-256 mismatch for $Path (expected $Sha256, got $actual)"
  }
}

function Invoke-7ZipExtract([string]$SevenZip, [string]$Archive, [string]$Destination) {
  if (Test-Path -LiteralPath $Destination) {
    Remove-Item -LiteralPath $Destination -Recurse -Force
  }
  New-Item -ItemType Directory -Path $Destination -Force | Out-Null
  & $SevenZip x -y "-o$Destination" $Archive | Out-Host
  if ($LASTEXITCODE -ne 0) {
    throw "7-Zip extraction failed with exit code ${LASTEXITCODE}: $Archive"
  }
}

$outputRoot = Resolve-RepoPath $OutputDirectory
Assert-GeneratedTarget $outputRoot
$cacheRoot = Resolve-RepoPath "target/rime-downloads"
$workRoot = Resolve-RepoPath "target/rime-runtime-work"
Assert-GeneratedTarget $cacheRoot
Assert-GeneratedTarget $workRoot
New-Item -ItemType Directory -Path $cacheRoot -Force | Out-Null

$librime = $manifest.librime
$ice = $manifest.rime_ice
$librimeBase = "https://github.com/rime/librime/releases/download/$($librime.version)"
$iceBase = "https://github.com/iDvel/rime-ice/releases/download/$($ice.version)"
$librimeArchives = @()
foreach ($asset in $librime.assets) {
  $path = Join-Path $cacheRoot $asset.name
  Invoke-VerifiedDownload "$librimeBase/$($asset.name)" $path $asset.sha256
  $librimeArchives += $path
}
$iceArchive = Join-Path $cacheRoot $ice.asset
Invoke-VerifiedDownload "$iceBase/$($ice.asset)" $iceArchive $ice.sha256

$sevenZip = Resolve-7Zip
$librimeWork = Join-Path $workRoot "librime"
$depsWork = Join-Path $workRoot "deps"
$iceWork = Join-Path $workRoot "rime-ice"
Invoke-7ZipExtract $sevenZip $librimeArchives[0] $librimeWork
Invoke-7ZipExtract $sevenZip $librimeArchives[1] $depsWork
if (Test-Path -LiteralPath $iceWork) {
  Remove-Item -LiteralPath $iceWork -Recurse -Force
}
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

if (Test-Path -LiteralPath $outputRoot) {
  Remove-Item -LiteralPath $outputRoot -Recurse -Force
}
New-Item -ItemType Directory -Path $outputRoot -Force | Out-Null
Copy-Item -LiteralPath $rimeDll.FullName -Destination (Join-Path $outputRoot "rime.dll") -Force
foreach ($dependency in Get-ChildItem -LiteralPath $depsWork -Recurse -File -Filter "*.dll") {
  Copy-Item -LiteralPath $dependency.FullName -Destination (Join-Path $outputRoot $dependency.Name) -Force
}
Get-ChildItem -LiteralPath $iceRoot.FullName -Force | ForEach-Object {
  Copy-Item -LiteralPath $_.FullName -Destination $outputRoot -Recurse -Force
}

[System.IO.File]::WriteAllText((Join-Path $outputRoot "VERSION"), "$($librime.version)`n", [System.Text.UTF8Encoding]::new($false))
[System.IO.File]::WriteAllText((Join-Path $outputRoot "RIME_ICE_VERSION"), "$($ice.version)`n", [System.Text.UTF8Encoding]::new($false))
Copy-Item -LiteralPath $manifestPath -Destination (Join-Path $outputRoot "librime-1.17.0.manifest.json") -Force

foreach ($required in $manifest.required_files) {
  $path = Join-Path $outputRoot $required
  if (-not (Test-Path -LiteralPath $path)) {
    throw "Prepared runtime is missing required file: $path"
  }
}
Write-Host "Prepared official librime $($librime.version) and Rime-Ice $($ice.version) at $outputRoot"
