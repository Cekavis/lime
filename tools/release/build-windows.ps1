$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path
Set-Location $repoRoot

function Invoke-Checked([string]$FilePath, [string[]]$Arguments) {
  & $FilePath @Arguments
  if ($LASTEXITCODE -ne 0) {
    throw "$FilePath exited with code $LASTEXITCODE"
  }
}

function Write-Utf8NoBom([string]$Path, [string]$Content) {
  $encoding = New-Object System.Text.UTF8Encoding($false)
  [System.IO.File]::WriteAllText($Path, $Content, $encoding)
}

function Resolve-RepoPath([string]$Path) {
  if ([System.IO.Path]::IsPathRooted($Path)) { return [System.IO.Path]::GetFullPath($Path) }
  return [System.IO.Path]::GetFullPath((Join-Path $repoRoot $Path))
}

function Copy-RimePackage([string]$Source, [string]$Destination) {
  $sourceRoot = (Resolve-Path -LiteralPath $Source).Path
  foreach ($file in Get-ChildItem -LiteralPath $sourceRoot -Recurse -File) {
    $relative = $file.FullName.Substring($sourceRoot.Length).TrimStart([char]92, [char]47)
    $segments = $relative -split '[\\/]'
    $name = $file.Name
    if ($segments -contains 'trash' -or $segments -contains 'user' -or ($segments | Where-Object { $_ -like '*.userdb' })) { continue }
    if ($name -eq 'installation.yaml' -or $name -eq 'user.yaml' -or $name -like '*.custom.yaml') { continue }
    if ($file.Extension -eq '.bin' -and $segments -notcontains 'build') { continue }
    $target = Join-Path $Destination $relative
    $parent = Split-Path -Parent $target
    if (-not (Test-Path -LiteralPath $parent)) { New-Item -ItemType Directory -Path $parent -Force | Out-Null }
    Copy-Item -LiteralPath $file.FullName -Destination $target -Force
  }
}

$cmake = (Get-Command cmake -ErrorAction SilentlyContinue).Source
if (-not $cmake) {
  $cmakeCandidates = @(
    "C:\Program Files\Microsoft Visual Studio\18\Community\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe",
    "C:\Program Files\Microsoft Visual Studio\2022\Community\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe"
  )
  $cmake = $cmakeCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
}
if (-not $cmake) { throw "CMake 3.20+ was not found" }

$npm = (Get-Command npm.cmd -ErrorAction SilentlyContinue).Source
if (-not $npm) { throw "npm.cmd was not found; install Node.js 20+ first" }

Write-Host "Building Rust core service (release)..."
Invoke-Checked "cargo" @("build", "--release", "-p", "lime-core", "--bin", "lime-service")

Write-Host "Building Windows TSF adapter (x64 Release)..."
Invoke-Checked $cmake @("-S", "platform/windows/tsf", "-B", "build/tsf", "-A", "x64", "-DLIME_WITH_WEASEL_UI=ON", "-DLIME_WEASEL_UI_OPENMP=OFF")
Invoke-Checked $cmake @("--build", "build/tsf", "--config", "Release")

Write-Host "Staging librime and Rime/雾凇 runtime assets..."
$runtimeRootValue = $env:LIME_RIME_RUNTIME_DIR
if (-not $runtimeRootValue) {
  $runtimeRootValue = "target/rime-runtime"
  $prepareScript = Join-Path $repoRoot "tools/release/prepare-rime-runtime.ps1"
  if (-not (Test-Path -LiteralPath $prepareScript)) { throw "Missing runtime preparation script: $prepareScript" }
  & $prepareScript -OutputDirectory $runtimeRootValue
}
$runtimeRoot = Resolve-RepoPath $runtimeRootValue
$manifestPath = Join-Path $repoRoot "resources/runtime/librime-1.17.0.manifest.json"
if (-not (Test-Path $manifestPath)) { throw "Missing librime runtime manifest: $manifestPath" }
$manifest = Get-Content $manifestPath -Raw | ConvertFrom-Json
if ($manifest.librime.version -ne "1.17.0") { throw "Unexpected librime manifest version: $($manifest.librime.version)" }
$runtimeVersion = $env:LIME_LIBRIME_VERSION
if (-not $runtimeVersion) {
  $versionFile = Join-Path $runtimeRoot "VERSION"
  if (Test-Path $versionFile) { $runtimeVersion = (Get-Content $versionFile -Raw).Trim() }
}
if ($runtimeVersion -ne $manifest.librime.version) {
  throw "librime runtime must be version $($manifest.librime.version); run prepare-rime-runtime.ps1 or set LIME_RIME_RUNTIME_DIR to its output (got '$runtimeVersion')."
}
$iceVersion = $env:LIME_RIME_ICE_VERSION
if (-not $iceVersion) {
  $iceVersionFile = Join-Path $runtimeRoot "RIME_ICE_VERSION"
  if (Test-Path $iceVersionFile) { $iceVersion = (Get-Content $iceVersionFile -Raw).Trim() }
}
if ($iceVersion -ne $manifest.rime_ice.version) {
  throw "Rime-Ice runtime must be version $($manifest.rime_ice.version); run prepare-rime-runtime.ps1 or set LIME_RIME_RUNTIME_DIR to its output (got '$iceVersion')."
}
foreach ($required in $manifest.required_files) {
  $requiredPath = Join-Path $runtimeRoot $required
  if (-not (Test-Path -LiteralPath $requiredPath)) {
    throw "Rime runtime asset is missing: $requiredPath. Use prepare-rime-runtime.ps1 or set LIME_RIME_RUNTIME_DIR to its output."
  }
}
$packageRoot = Join-Path $repoRoot "target/rime-package"
if (Test-Path $packageRoot) { Remove-Item -LiteralPath $packageRoot -Recurse -Force }
New-Item -ItemType Directory -Path $packageRoot | Out-Null
Copy-RimePackage $runtimeRoot $packageRoot
Copy-Item $manifestPath (Join-Path $packageRoot "librime-1.17.0.manifest.json") -Force
$bundledWeaselTheme = Resolve-RepoPath "resources/rime/weasel.yaml"
if (Test-Path -LiteralPath $bundledWeaselTheme) {
  Copy-Item -LiteralPath $bundledWeaselTheme -Destination (Join-Path $packageRoot "weasel.yaml") -Force
}
Write-Host "Rime package staged at $packageRoot"

Write-Host "Staging pinned llama.cpp CUDA and CPU runtimes..."
$llamaPackageRoot = Resolve-RepoPath "target/llama-runtime"
$prepareLlamaScript = Join-Path $repoRoot "tools/release/prepare-llama-runtime.ps1"
if (-not (Test-Path -LiteralPath $prepareLlamaScript)) {
  throw "Missing llama.cpp runtime preparation script: $prepareLlamaScript"
}
$llamaSourceOverride = $env:LIME_LLAMA_RUNTIME_DIR
$llamaCudaSourceOverride = $env:LIME_LLAMA_CUDA_RUNTIME_DIR
$llamaCpuSourceOverride = $env:LIME_LLAMA_CPU_RUNTIME_DIR
$packageFull = [System.IO.Path]::GetFullPath($llamaPackageRoot).TrimEnd([char]92, [char]47)
foreach ($override in @($llamaSourceOverride, $llamaCudaSourceOverride, $llamaCpuSourceOverride)) {
  if (-not $override) { continue }
  $sourceFull = [System.IO.Path]::GetFullPath((Resolve-RepoPath $override)).TrimEnd([char]92, [char]47)
  if ($sourceFull.Equals($packageFull, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Llama runtime source overrides must not point to the build output directory $llamaPackageRoot"
  }
  $sourcePrefix = $sourceFull + [System.IO.Path]::DirectorySeparatorChar
  $packagePrefix = $packageFull + [System.IO.Path]::DirectorySeparatorChar
  if ($sourcePrefix.StartsWith($packagePrefix, [System.StringComparison]::OrdinalIgnoreCase) -or
      $packagePrefix.StartsWith($sourcePrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Llama runtime source override must not overlap the build output directory ${llamaPackageRoot}: $sourceFull"
  }
}
if (Test-Path -LiteralPath $llamaPackageRoot) {
  Remove-Item -LiteralPath $llamaPackageRoot -Recurse -Force
}
New-Item -ItemType Directory -Path $llamaPackageRoot -Force | Out-Null
# Keep the output location deterministic so the Tauri resource map and the service's
# executable-adjacent runtime discovery resolve the same directory.  CUDA is the default;
# CPU remains bundled for explicit selection and automatic fallback. A shared
# LIME_LLAMA_RUNTIME_DIR override may provide a mixed directory; use
# LIME_LLAMA_CUDA_RUNTIME_DIR/LIME_LLAMA_CPU_RUNTIME_DIR for separate sources.
$llamaBackends = @("cuda", "cpu")
foreach ($backend in $llamaBackends) {
  $backendRoot = Join-Path $llamaPackageRoot $backend
  $sourceOverride = if ($backend -eq "cuda") { $llamaCudaSourceOverride } else { $llamaCpuSourceOverride }
  if (-not $sourceOverride) { $sourceOverride = $llamaSourceOverride }
  # Use a hashtable for script-parameter splatting. An array containing named
  # parameter tokens is bound positionally by PowerShell, which makes the
  # following `-CudaVersion` token get interpreted as the Backend value.
  $prepareParameters = @{
    Backend = $backend
    CudaVersion = "13.3"
    OutputDirectory = $backendRoot
  }
  if ($sourceOverride) { $prepareParameters.SourceDirectory = $sourceOverride }
  & $prepareLlamaScript @prepareParameters
  $manifestName = if ($backend -eq "cuda") {
    "llama-cpp-b10743-cuda-13.3.manifest.json"
  } else {
    "llama-cpp-b10743.manifest.json"
  }
  $llamaManifestPath = Join-Path $repoRoot "resources/runtime/$manifestName"
  if (-not (Test-Path -LiteralPath $llamaManifestPath)) {
    throw "Missing llama.cpp runtime manifest: $llamaManifestPath"
  }
  $llamaManifest = Get-Content -LiteralPath $llamaManifestPath -Raw | ConvertFrom-Json
  foreach ($required in @($llamaManifest.required_files)) {
    $requiredPath = Join-Path $backendRoot $required
    if (-not (Test-Path -LiteralPath $requiredPath -PathType Leaf)) {
      throw "llama.cpp $backend runtime asset is missing: $requiredPath"
    }
  }
  if ($llamaManifest.required_patterns) {
    foreach ($pattern in @($llamaManifest.required_patterns)) {
      $patternMatches = @(Get-ChildItem -LiteralPath $backendRoot -File -Filter $pattern)
      if ($patternMatches.Count -eq 0) {
        throw "llama.cpp $backend runtime is missing a DLL matching '$pattern' under $backendRoot"
      }
    }
  }
}
Write-Host "llama.cpp CUDA/CPU package staged at $llamaPackageRoot"

Write-Host "Installing frontend dependencies from lockfile..."
Invoke-Checked $npm @("--prefix", "frontend", "ci")

Write-Host "Building NSIS test installer..."
$tauriCli = Join-Path $repoRoot "frontend/node_modules/.bin/tauri.cmd"
if (-not (Test-Path $tauriCli)) { throw "Tauri CLI was not installed" }
$tauriRoot = Join-Path $repoRoot "src-tauri"
$previousCargoJobs = $env:CARGO_BUILD_JOBS
$env:CARGO_BUILD_JOBS = "1"
$configPath = Join-Path $tauriRoot "tauri.conf.json"
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$originalConfig = [System.IO.File]::ReadAllText($configPath, $utf8NoBom)
$releaseTag = $env:GITHUB_REF_NAME
$config = $originalConfig | ConvertFrom-Json
# Tauri runs the build hook with `frontend` as its working directory during
# `tauri build`, so the hook must not prepend another `frontend` path. Keep
# the repository config unchanged after the build; the dev hook has different
# working-directory behavior and remains configured separately.
$config.build.beforeBuildCommand = "npm run build"
if ($releaseTag -match '^v(?<version>\d+\.\d+\.\d+)$') {
  $config.version = $Matches.version
  Write-Host "Using release version $($Matches.version) from $releaseTag"
}
Write-Utf8NoBom $configPath ($config | ConvertTo-Json -Depth 20)
Push-Location $repoRoot
try {
  # Run from the repository root so resource paths remain relative to the
  # repository's src-tauri configuration.
  Invoke-Checked $tauriCli @("build", "--config", "src-tauri/tauri.conf.json", "--bundles", "nsis")
} finally {
  Pop-Location
  Write-Utf8NoBom $configPath $originalConfig
  $env:CARGO_BUILD_JOBS = $previousCargoJobs
}

$bundle = Get-ChildItem "src-tauri/target/release/bundle/nsis/*.exe" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $bundle) { throw "Tauri did not produce an NSIS installer" }
$hash = (Get-FileHash $bundle.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
$hashPath = "$($bundle.FullName).sha256"
"$hash  $($bundle.Name)" | Set-Content -NoNewline -Encoding ascii $hashPath
Write-Host "Installer: $($bundle.FullName)"
Write-Host "SHA-256:   $hash"
