param(
  [string]$OutputDirectory = "target/llama-runtime",
  [string]$SourceDirectory = "",
  [ValidateSet("cuda", "cpu")]
  [string]$Backend = "cuda",
  [ValidateSet("13.3", "12.4")]
  [string]$CudaVersion = "13.3"
)

$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path
$manifestFileName = if ($Backend -eq "cuda") {
  "llama-cpp-b10743-cuda-$CudaVersion.manifest.json"
} else {
  "llama-cpp-b10743.manifest.json"
}
$manifestPath = Join-Path $repoRoot "resources/runtime/$manifestFileName"
if (-not (Test-Path -LiteralPath $manifestPath)) {
  throw "Missing llama.cpp runtime manifest: $manifestPath"
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
  $prefix = $targetRoot.TrimEnd([char]92, [char]47) + [System.IO.Path]::DirectorySeparatorChar
  if (-not $fullPath.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Generated llama runtime path must remain under ${targetRoot}: $fullPath"
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
      Write-Host "Using verified llama.cpp cache: $Path"
      return
    }
    Remove-Item -LiteralPath $Path -Force
  }
  Write-Host "Downloading pinned llama.cpp runtime $($manifest.version)..."
  & curl.exe -L --fail --retry 3 --output $Path $Url
  if ($LASTEXITCODE -ne 0) {
    throw "Download failed with exit code ${LASTEXITCODE}: $Url"
  }
  $actual = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
  if ($actual -ne $Sha256.ToLowerInvariant()) {
    throw "SHA-256 mismatch for $Path (expected $Sha256, got $actual)"
  }
}

function Copy-RuntimeFiles([string[]]$SourceRoots, [string]$DestinationRoot) {
  $resolvedRoots = @($SourceRoots | ForEach-Object { (Resolve-Path -LiteralPath $_).Path })
  # Keep only the DLLs needed by the selected backend.  CUDA packages include the ggml CUDA
  # plugin and CUDA runtime libraries; CPU packages include the ggml CPU dispatch plugins.  The
  # same function accepts a local developer bundle or the two official release archives.
  $dlls = @($resolvedRoots | ForEach-Object {
    Get-ChildItem -LiteralPath $_ -Recurse -File -Filter "*.dll"
  } | Where-Object {
    if ($Backend -eq "cuda") {
      $_.Name -eq "llama.dll" -or
      $_.Name -eq "ggml.dll" -or
      $_.Name -eq "ggml-base.dll" -or
      $_.Name -eq "ggml-cuda.dll" -or
      $_.Name -like "cublas*.dll" -or
      $_.Name -like "cudart*.dll" -or
      $_.Name -like "nvrtc*.dll" -or
      $_.Name -like "nvjitlink*.dll" -or
      $_.Name -like "cusolver*.dll" -or
      $_.Name -like "cusparse*.dll" -or
      $_.Name -like "cufft*.dll" -or
      $_.Name -like "curand*.dll" -or
      $_.Name -like "npp*.dll" -or
      $_.Name -like "nvblas*.dll" -or
      $_.Name -like "ggml-cpu-*.dll" -or
      $_.Name -eq "libomp.dll"
    } else {
      $_.Name -eq "llama.dll" -or
      $_.Name -eq "ggml.dll" -or
      $_.Name -eq "ggml-base.dll" -or
      $_.Name -like "ggml-cpu-*.dll" -or
      $_.Name -eq "libomp.dll"
    }
  })
  if ($dlls.Count -eq 0) {
    throw "No llama.cpp $Backend DLLs found under $($resolvedRoots -join ', ')"
  }
  $names = @{}
  foreach ($file in $dlls) {
    $name = $file.Name.ToLowerInvariant()
    if ($names.ContainsKey($name)) {
      $existingPath = Join-Path $DestinationRoot $file.Name
      if (Test-Path -LiteralPath $existingPath -PathType Leaf) {
        $existingHash = (Get-FileHash -LiteralPath $existingPath -Algorithm SHA256).Hash
        $incomingHash = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash
        if ($existingHash -eq $incomingHash) {
          continue
        }
      }
      throw "Duplicate runtime DLL name '$($file.Name)' with different contents under $($resolvedRoots -join ', '); refusing to flatten ambiguous files"
    }
    $names[$name] = $true
    Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $DestinationRoot $file.Name) -Force
  }

  foreach ($license in @($manifest.license_files)) {
    $licenseFile = $resolvedRoots | ForEach-Object {
      Get-ChildItem -LiteralPath $_ -Recurse -File -Filter $license | Select-Object -First 1
    } | Select-Object -First 1
    if ($licenseFile) {
      Copy-Item -LiteralPath $licenseFile.FullName -Destination (Join-Path $DestinationRoot $license) -Force
    } else {
      Write-Warning "Optional llama.cpp runtime license file was not found: $license"
    }
  }
}

function Assert-Runtime([string]$Root) {
  foreach ($required in @($manifest.required_files)) {
    $requiredPath = Join-Path $Root $required
    if (-not (Test-Path -LiteralPath $requiredPath -PathType Leaf)) {
      throw "Prepared llama.cpp runtime is missing required file: $requiredPath"
    }
  }
  if ($manifest.required_patterns) {
    foreach ($pattern in @($manifest.required_patterns)) {
      $matches = @(Get-ChildItem -LiteralPath $Root -File -Filter $pattern)
      if ($matches.Count -eq 0) {
        throw "Prepared llama.cpp runtime is missing a DLL matching '$pattern' under $Root"
      }
    }
  }
  $library = Join-Path $Root "llama.dll"
  if ((Get-Item -LiteralPath $library).Length -lt 1MB) {
    throw "Prepared llama.cpp library is unexpectedly small: $library"
  }
}

$outputRoot = Resolve-RepoPath $OutputDirectory
Assert-GeneratedTarget $outputRoot
$cacheRoot = Resolve-RepoPath "target/llama-downloads"
$workRoot = Resolve-RepoPath "target/llama-runtime-work"
Assert-GeneratedTarget $cacheRoot
Assert-GeneratedTarget $workRoot
New-Item -ItemType Directory -Path $cacheRoot -Force | Out-Null

if ([string]::IsNullOrWhiteSpace($SourceDirectory)) {
  $SourceDirectory = $env:LIME_LLAMA_RUNTIME_DIR
}

$sourceRoots = @()
if (-not [string]::IsNullOrWhiteSpace($SourceDirectory)) {
  $sourcePath = Resolve-RepoPath $SourceDirectory
  if (Test-Path -LiteralPath $sourcePath -PathType Leaf) {
    if ((Split-Path -Leaf $sourcePath) -ne "llama.dll") {
      throw "LIME_LLAMA_RUNTIME_DIR must point to a runtime directory or llama.dll: $sourcePath"
    }
    $sourceRoots = @(Split-Path -Parent $sourcePath)
  } elseif (Test-Path -LiteralPath $sourcePath -PathType Container) {
    $sourceRoots = @($sourcePath)
  } else {
    throw "Configured llama runtime path does not exist: $sourcePath"
  }
} else {
  if (Test-Path -LiteralPath $workRoot) {
    Remove-Item -LiteralPath $workRoot -Recurse -Force
  }
  New-Item -ItemType Directory -Path $workRoot -Force | Out-Null
  $sevenZip = Resolve-7Zip
  $archiveSpecs = @(
    @{ asset = $manifest.asset; suffix = "main" }
  )
  if ($Backend -eq "cuda" -and $manifest.cuda_runtime_asset) {
    $archiveSpecs += @{ asset = $manifest.cuda_runtime_asset; suffix = "cuda-runtime" }
  }
  $archiveIndex = 0
  foreach ($spec in $archiveSpecs) {
    $asset = $spec.asset
    $archive = Join-Path $cacheRoot $asset.name
    Invoke-VerifiedDownload $asset.url $archive $asset.sha256
    $extractRoot = Join-Path $workRoot ("archive-{0}-{1}" -f $archiveIndex, $spec.suffix)
    New-Item -ItemType Directory -Path $extractRoot -Force | Out-Null
    & $sevenZip x -y "-o$extractRoot" $archive | Out-Host
    if ($LASTEXITCODE -ne 0) {
      throw "7-Zip extraction failed with exit code ${LASTEXITCODE}: $archive"
    }
    $sourceRoots += $extractRoot
    $archiveIndex++
  }
}

if ($sourceRoots.Count -eq 0) {
  throw "No llama.cpp $Backend runtime source was selected"
}

foreach ($sourceRoot in $sourceRoots) {
  $sourceFull = [System.IO.Path]::GetFullPath($sourceRoot).TrimEnd([char]92, [char]47)
  $outputFull = [System.IO.Path]::GetFullPath($outputRoot).TrimEnd([char]92, [char]47)
  if ([System.StringComparer]::OrdinalIgnoreCase.Equals($sourceFull, $outputFull)) {
    continue
  }
  $sourcePrefix = $sourceFull + [System.IO.Path]::DirectorySeparatorChar
  $outputPrefix = $outputFull + [System.IO.Path]::DirectorySeparatorChar
  if ($outputPrefix.StartsWith($sourcePrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Output directory must not be nested inside the source runtime directory: $outputRoot"
  }
  if ($sourcePrefix.StartsWith($outputPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Source runtime directory must not be nested inside the output directory: $sourceRoot"
  }
}

if (Test-Path -LiteralPath $outputRoot) {
  if ($sourceRoots.Count -eq 1 -and [System.StringComparer]::OrdinalIgnoreCase.Equals(
      [System.IO.Path]::GetFullPath($sourceRoots[0]).TrimEnd([char]92, [char]47),
      [System.IO.Path]::GetFullPath($outputRoot).TrimEnd([char]92, [char]47))) {
    # A previously prepared directory is already in the exact requested shape.  Validate it
    # in place instead of deleting the source before it can be copied.
    Assert-Runtime $outputRoot
    Write-Host "Using existing prepared llama.cpp runtime at $outputRoot"
    exit 0
  }
  Remove-Item -LiteralPath $outputRoot -Recurse -Force
}
New-Item -ItemType Directory -Path $outputRoot -Force | Out-Null
Copy-RuntimeFiles $sourceRoots $outputRoot
Assert-Runtime $outputRoot

[System.IO.File]::WriteAllText(
  (Join-Path $outputRoot "VERSION"),
  "$($manifest.version) $Backend`n",
  [System.Text.UTF8Encoding]::new($false)
)
Copy-Item -LiteralPath $manifestPath -Destination (Join-Path $outputRoot (Split-Path -Leaf $manifestPath)) -Force
Write-Host "Prepared llama.cpp $($manifest.version) $Backend runtime at $outputRoot"
