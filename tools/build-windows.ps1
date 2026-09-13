$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $repoRoot
Import-Module (Join-Path $PSScriptRoot "common.psm1") -Force

$outRoot = Join-Path $repoRoot "out/windows-x64"
$stageRoot = Join-Path $outRoot "staging/app"
$cmakeRoot = Join-Path $outRoot "cmake/tsf"
$cargoRoot = Join-Path $repoRoot "out/cargo"
if (Test-Path -LiteralPath $stageRoot) { Remove-Item -LiteralPath $stageRoot -Recurse -Force }
New-Item -ItemType Directory -Path $stageRoot -Force | Out-Null

$cmake = (Get-Command cmake -ErrorAction SilentlyContinue).Source
if (-not $cmake) { throw "CMake 3.20+ was not found" }
$npm = (Get-Command npm.cmd -ErrorAction SilentlyContinue).Source
if (-not $npm) { throw "npm.cmd was not found; install Node.js 20+ first" }

$env:CARGO_TARGET_DIR = $cargoRoot
Write-Host "Building Rust service..."
Invoke-Checked "cargo" @("build", "--release", "--locked", "-p", "lime-service")

Write-Host "Building Windows TSF..."
Invoke-Checked $cmake @("-S", "platform/windows/tsf", "-B", $cmakeRoot, "-A", "x64", "-DLIME_WEASEL_UI_OPENMP=OFF")
Invoke-Checked $cmake @("--build", $cmakeRoot, "--config", "Release")

Write-Host "Preparing pinned third-party sources..."
& (Join-Path $PSScriptRoot "prepare-third-party-sources.ps1")
if ($LASTEXITCODE -ne 0) { throw "prepare-third-party-sources.ps1 failed" }

Write-Host "Preparing Rime runtime..."
& (Join-Path $PSScriptRoot "prepare-rime-runtime.ps1") -OutputDirectory "out/windows-x64/staging/app/rime"
if ($LASTEXITCODE -ne 0) { throw "prepare-rime-runtime.ps1 failed" }

$sourceRoot = "out/windows-x64/sources/llama/b10743"
& (Join-Path $PSScriptRoot "build-llama-patched.ps1") -Backend cpu -SourceDirectory $sourceRoot
if ($LASTEXITCODE -ne 0) { throw "CPU llama.cpp patch build failed" }
$patchedCpu = "out/windows-x64/patched/llama/b10743/cpu"

Write-Host "Preparing patched llama.cpp runtimes..."
& (Join-Path $PSScriptRoot "prepare-llama-runtime.ps1") -Backend cpu -OutputDirectory "out/windows-x64/staging/app/llama/cpu" -PatchedLlamaPath $patchedCpu
if ($LASTEXITCODE -ne 0) { throw "CPU llama runtime staging failed" }
& (Join-Path $PSScriptRoot "prepare-llama-runtime.ps1") -Backend cuda -CudaVersion "13.3" -OutputDirectory "out/windows-x64/staging/app/llama/cuda" -PatchedLlamaPath $patchedCpu
if ($LASTEXITCODE -ne 0) { throw "CUDA llama runtime staging failed" }

Copy-Item -LiteralPath (Join-Path $cargoRoot "release/lime-service.exe") -Destination (Join-Path $stageRoot "lime-service.exe") -Force
$tsf = Join-Path $cmakeRoot "Release/lime-tsf.dll"
if (-not (Test-Path -LiteralPath $tsf)) { throw "TSF build did not produce $tsf" }
Copy-Item -LiteralPath $tsf -Destination (Join-Path $stageRoot "lime-tsf.dll") -Force
$license = Join-Path $stageRoot "licenses/WeaselUI-GPL-3.0.txt"
New-Item -ItemType Directory -Path (Split-Path -Parent $license) -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $repoRoot "third_party/weasel-ui/LICENSE.txt") -Destination $license -Force

Write-Host "Installing frontend dependencies..."
Invoke-Checked $npm @("--prefix", "crates/lime-management", "ci")
Write-Host "Building management UI and installer..."
$tauriCli = Join-Path $repoRoot "crates/lime-management/node_modules/.bin/tauri.cmd"
if (-not (Test-Path -LiteralPath $tauriCli)) { throw "Tauri CLI was not installed" }
Invoke-Checked $tauriCli @("build", "--bundles", "nsis", "--config", "crates/lime-management/src-tauri/tauri.conf.json")

$bundle = Get-ChildItem (Join-Path $cargoRoot "release/bundle/nsis/*.exe") | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $bundle) { throw "Tauri did not produce an NSIS installer" }
$hash = (Get-FileHash $bundle.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
$hashPath = "$($bundle.FullName).sha256"
"$hash  $($bundle.Name)" | Set-Content -NoNewline -Encoding ascii $hashPath
Write-Host "Installer: $($bundle.FullName)"
Write-Host "SHA-256:   $hash"
