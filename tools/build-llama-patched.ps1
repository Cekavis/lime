param(
  [ValidateSet("cpu", "cuda")]
  [string]$Backend = "cpu",
  [string]$SourceDirectory = "out/windows-x64/sources/llama/b10743",
  [string]$OutputDirectory = "out/windows-x64/patched/llama/b10743"
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Import-Module (Join-Path $PSScriptRoot "common.psm1") -Force

$sourceRoot = Resolve-RepoPath $repoRoot $SourceDirectory
$patchPath = Resolve-RepoPath $repoRoot "third_party/llama/b10743/output-reorder.patch"
$manifestPath = Resolve-RepoPath $repoRoot "third_party/llama/b10743/source.json"
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
$outputRoot = Resolve-RepoPath $repoRoot (Join-Path $OutputDirectory $Backend)
$buildRoot = Resolve-RepoPath $repoRoot (Join-Path "out/windows-x64/builds/llama/b10743" $Backend)
Assert-UnderOut $repoRoot $sourceRoot
Assert-UnderOut $repoRoot $outputRoot
Assert-UnderOut $repoRoot $buildRoot

if (-not (Test-Path -LiteralPath $sourceRoot -PathType Container)) {
  throw "Missing llama.cpp source. Run prepare-third-party-sources.ps1 first: $sourceRoot"
}
$actualCommit = (& git -C $sourceRoot rev-parse HEAD).Trim()
if ($actualCommit -ne $manifest.source_commit) {
  throw "llama.cpp source commit is $actualCommit, expected $($manifest.source_commit)"
}

$patchHash = (Get-FileHash -LiteralPath $patchPath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($patchHash -ne $manifest.patch.sha256.ToLowerInvariant()) {
  throw "llama.cpp patch hash is $patchHash, expected manifest value $($manifest.patch.sha256)"
}
$applyScript = Join-Path $PSScriptRoot "apply-llama-gpu-logprob-patch.ps1"
& $applyScript -SourceDirectory $SourceDirectory
if ($LASTEXITCODE -ne 0) { throw "apply-llama-gpu-logprob-patch.ps1 failed" }

$cmakeArgs = @(
  "-S", $sourceRoot,
  "-B", $buildRoot,
  "-A", "x64",
  "-DLLAMA_BUILD_TESTS=OFF",
  "-DLLAMA_BUILD_EXAMPLES=OFF",
  "-DLLAMA_BUILD_SERVER=OFF",
  "-DLLAMA_CURL=OFF",
  "-DGGML_NATIVE=OFF",
  "-DBUILD_SHARED_LIBS=ON",
  "-DGGML_BACKEND_DL=ON",
  "-DGGML_CUDA=OFF",
  "-DGGML_CPU=ON",
  "-DCMAKE_RUNTIME_OUTPUT_DIRECTORY=$(Join-Path $buildRoot 'bin')"
)
Invoke-Checked "cmake" $cmakeArgs
Invoke-Checked "cmake" @("--build", $buildRoot, "--config", "Release", "--target", "llama")

$builtPath = Join-Path $buildRoot "bin/Release/llama.dll"
if (-not (Test-Path -LiteralPath $builtPath -PathType Leaf)) {
  throw "Patched llama.cpp build did not produce the expected Release DLL: $builtPath"
}
$built = Get-Item -LiteralPath $builtPath
if (Test-Path -LiteralPath $outputRoot) { Remove-Item -LiteralPath $outputRoot -Recurse -Force }
New-Item -ItemType Directory -Path $outputRoot -Force | Out-Null
Copy-Item -LiteralPath $built.FullName -Destination (Join-Path $outputRoot "llama.dll") -Force
$dllHash = (Get-FileHash -LiteralPath $built.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
[System.IO.File]::WriteAllText((Join-Path $outputRoot ".lime-output-reorder-patched"), "source_commit=$actualCommit`npatch_sha256=$patchHash`ndll_sha256=$dllHash`n", [System.Text.UTF8Encoding]::new($false))
Write-Host "Built patched llama.cpp $Backend runtime: $($built.FullName)"
