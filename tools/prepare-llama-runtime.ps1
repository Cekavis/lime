param(
  [string]$OutputDirectory = "out/windows-x64/staging/llama/cuda",
  [string]$SourceDirectory = "",
  [string]$PatchedLlamaPath = "",
  [ValidateSet("cuda", "cpu")]
  [string]$Backend = "cuda",
  [ValidateSet("13.3", "12.4")]
  [string]$CudaVersion = "13.3"
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Import-Module (Join-Path $PSScriptRoot "common.psm1") -Force

$manifestFileName = if ($Backend -eq "cuda") { "runtime-cuda-$CudaVersion.json" } else { "runtime-cpu.json" }
$manifestPath = Resolve-RepoPath $repoRoot (Join-Path "third_party/llama/b10743" $manifestFileName)
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
$sourceManifest = Get-Content -LiteralPath (Resolve-RepoPath $repoRoot "third_party/llama/b10743/source.json") -Raw | ConvertFrom-Json
$patchPath = Resolve-RepoPath $repoRoot "third_party/llama/b10743/output-reorder.patch"
$patchHash = (Get-FileHash -LiteralPath $patchPath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($patchHash -ne $sourceManifest.patch.sha256.ToLowerInvariant()) {
  throw "llama.cpp patch hash is $patchHash, expected manifest value $($sourceManifest.patch.sha256)"
}
$outputRoot = Resolve-RepoPath $repoRoot $OutputDirectory
Assert-UnderOut $repoRoot $outputRoot
$cacheRoot = Resolve-RepoPath $repoRoot "out/windows-x64/downloads/llama"
$workRoot = Resolve-RepoPath $repoRoot (Join-Path "out/windows-x64/work/llama" $Backend)
Assert-UnderOut $repoRoot $cacheRoot
Assert-UnderOut $repoRoot $workRoot
New-Item -ItemType Directory -Path $cacheRoot -Force | Out-Null

function Copy-RuntimeFiles([string[]]$SourceRoots, [string]$DestinationRoot) {
  $resolvedRoots = @($SourceRoots | ForEach-Object { (Resolve-Path -LiteralPath $_).Path })
  $dlls = @($resolvedRoots | ForEach-Object { Get-ChildItem -LiteralPath $_ -Recurse -File -Filter "*.dll" } | Where-Object {
    if ($Backend -eq "cuda") {
      $_.Name -eq "llama.dll" -or $_.Name -eq "ggml.dll" -or $_.Name -eq "ggml-base.dll" -or
      $_.Name -eq "ggml-cuda.dll" -or $_.Name -like "cublas*.dll" -or $_.Name -like "cudart*.dll" -or
      $_.Name -like "nvrtc*.dll" -or $_.Name -like "nvjitlink*.dll" -or $_.Name -like "cusolver*.dll" -or
      $_.Name -like "cusparse*.dll" -or $_.Name -like "cufft*.dll" -or $_.Name -like "curand*.dll" -or
      $_.Name -like "npp*.dll" -or $_.Name -like "nvblas*.dll" -or $_.Name -like "ggml-cpu-*.dll" -or
      $_.Name -eq "libomp.dll"
    } else {
      $_.Name -eq "llama.dll" -or $_.Name -eq "ggml.dll" -or $_.Name -eq "ggml-base.dll" -or
      $_.Name -like "ggml-cpu-*.dll" -or $_.Name -eq "libomp.dll"
    }
  })
  if ($dlls.Count -eq 0) { throw "No llama.cpp $Backend DLLs found under $($resolvedRoots -join ', ')" }
  $names = @{}
  foreach ($file in $dlls) {
    $name = $file.Name.ToLowerInvariant()
    if ($names.ContainsKey($name)) {
      $existingPath = Join-Path $DestinationRoot $file.Name
      if ((Test-Path -LiteralPath $existingPath) -and
          (Get-FileHash -LiteralPath $existingPath -Algorithm SHA256).Hash -eq (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash) { continue }
      throw "Duplicate runtime DLL name '$($file.Name)' with different contents"
    }
    $names[$name] = $true
    Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $DestinationRoot $file.Name) -Force
  }
  foreach ($license in @($manifest.license_files)) {
    $licenseFile = $resolvedRoots | ForEach-Object { Get-ChildItem -LiteralPath $_ -Recurse -File -Filter $license | Select-Object -First 1 } | Select-Object -First 1
    if ($licenseFile) { Copy-Item -LiteralPath $licenseFile.FullName -Destination (Join-Path $DestinationRoot $license) -Force }
  }
}

function Assert-Runtime([string]$Root) {
  foreach ($required in @($manifest.required_files)) {
    $requiredPath = Join-Path $Root $required
    if (-not (Test-Path -LiteralPath $requiredPath -PathType Leaf)) { throw "Prepared runtime is missing required file: $requiredPath" }
  }
  $requiredPatterns = @(
    if ($manifest.PSObject.Properties.Name -contains "required_patterns") {
      @($manifest.required_patterns)
    }
  )
  foreach ($pattern in $requiredPatterns) {
    if (@(Get-ChildItem -LiteralPath $Root -File -Filter $pattern).Count -eq 0) { throw "Prepared runtime is missing a DLL matching '$pattern'" }
  }
  $library = Join-Path $Root "llama.dll"
  if ((Get-Item -LiteralPath $library).Length -lt 1MB) { throw "Prepared llama.cpp library is unexpectedly small: $library" }
}

$sourceRoots = @()
if (-not [string]::IsNullOrWhiteSpace($SourceDirectory)) {
  $sourcePath = Resolve-RepoPath $repoRoot $SourceDirectory
  if (Test-Path -LiteralPath $sourcePath -PathType Leaf) { $sourceRoots = @(Split-Path -Parent $sourcePath) }
  elseif (Test-Path -LiteralPath $sourcePath -PathType Container) { $sourceRoots = @($sourcePath) }
  else { throw "Configured llama runtime path does not exist: $sourcePath" }
} else {
  if (Test-Path -LiteralPath $workRoot) { Remove-Item -LiteralPath $workRoot -Recurse -Force }
  New-Item -ItemType Directory -Path $workRoot -Force | Out-Null
  $sevenZip = Resolve-7Zip
  $specs = @(@{ asset = $manifest.asset; suffix = "main" })
  if ($Backend -eq "cuda" -and $manifest.cuda_runtime_asset) { $specs += @{ asset = $manifest.cuda_runtime_asset; suffix = "cuda-runtime" } }
  $index = 0
  foreach ($spec in $specs) {
    $archive = Join-Path $cacheRoot $spec.asset.name
    Invoke-VerifiedDownload $spec.asset.url $archive $spec.asset.sha256
    $extractRoot = Join-Path $workRoot ("archive-{0}-{1}" -f $index, $spec.suffix)
    Invoke-7ZipExtract $sevenZip $archive $extractRoot
    $sourceRoots += $extractRoot
    $index++
  }
}

if ($sourceRoots.Count -eq 0) { throw "No llama.cpp $Backend runtime source was selected" }
if (Test-Path -LiteralPath $outputRoot) { Remove-Item -LiteralPath $outputRoot -Recurse -Force }
New-Item -ItemType Directory -Path $outputRoot -Force | Out-Null
Copy-RuntimeFiles $sourceRoots $outputRoot

if (-not [string]::IsNullOrWhiteSpace($PatchedLlamaPath)) {
  $patched = Resolve-RepoPath $repoRoot $PatchedLlamaPath
  $patchedDll = if (Test-Path -LiteralPath $patched -PathType Leaf) { $patched } else { Join-Path $patched "llama.dll" }
  $marker = Join-Path (Split-Path -Parent $patchedDll) ".lime-output-reorder-patched"
  if (-not (Test-Path -LiteralPath $patchedDll -PathType Leaf)) { throw "Patched llama.dll not found: $patchedDll" }
  if (-not (Test-Path -LiteralPath $marker -PathType Leaf)) { throw "Patched llama.dll is missing its patch marker: $marker" }
  $provenance = Get-Content -LiteralPath $marker -Raw | ConvertFrom-StringData
  if ($provenance.source_commit -ne $sourceManifest.source_commit -or $provenance.patch_sha256 -ne $patchHash) {
    throw "Patched llama.dll marker does not match $patchPath or the pinned source commit"
  }
  $dllHash = (Get-FileHash -LiteralPath $patchedDll -Algorithm SHA256).Hash.ToLowerInvariant()
  if (-not $provenance.ContainsKey("dll_sha256") -or $provenance.dll_sha256 -ne $dllHash) {
    throw "Patched llama.dll hash does not match its provenance marker"
  }
  Copy-Item -LiteralPath $patchedDll -Destination (Join-Path $outputRoot "llama.dll") -Force
  Copy-Item -LiteralPath $marker -Destination (Join-Path $outputRoot ".lime-output-reorder-patched") -Force
}

Assert-Runtime $outputRoot
[System.IO.File]::WriteAllText((Join-Path $outputRoot "VERSION"), "$($manifest.version) $Backend" + [char]10, [System.Text.UTF8Encoding]::new($false))
Copy-Item -LiteralPath $manifestPath -Destination (Join-Path $outputRoot (Split-Path -Leaf $manifestPath)) -Force
Write-Host "Prepared llama.cpp $($manifest.version) $Backend runtime at $outputRoot"
