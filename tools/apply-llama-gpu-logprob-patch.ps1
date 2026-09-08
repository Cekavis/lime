param(
  [string]$SourceDirectory = "out/windows-x64/sources/llama/b10743"
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Import-Module (Join-Path $PSScriptRoot "common.psm1") -Force

$sourceRoot = Resolve-RepoPath $repoRoot $SourceDirectory
$manifestPath = Resolve-RepoPath $repoRoot "third_party/llama/b10743/source.json"
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
$patchPath = Join-Path (Split-Path -Parent $manifestPath) $manifest.patch.file
Assert-UnderOut $repoRoot $sourceRoot
if (-not (Test-Path -LiteralPath $sourceRoot -PathType Container)) { throw "llama.cpp source directory does not exist: $sourceRoot" }
if (-not (Test-Path -LiteralPath $patchPath -PathType Leaf)) { throw "llama.cpp patch file does not exist: $patchPath" }

$actualCommit = (& git -C $sourceRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $actualCommit -ne $manifest.source_commit) {
  throw "llama.cpp source commit is $actualCommit, expected $($manifest.source_commit)"
}
$marker = Join-Path $sourceRoot ".lime-output-reorder-patched"
$patchHash = (Get-FileHash -LiteralPath $patchPath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($patchHash -ne $manifest.patch.sha256.ToLowerInvariant()) {
  throw "llama.cpp patch hash is $patchHash, expected $($manifest.patch.sha256)"
}
if (Test-Path -LiteralPath $marker -PathType Leaf) {
  $provenance = Get-Content -LiteralPath $marker -Raw | ConvertFrom-StringData
  if ($provenance.source_commit -ne $actualCommit -or $provenance.patch_sha256 -ne $patchHash) {
    throw "llama.cpp source has an incompatible Lime patch marker: $marker"
  }
  Push-Location $sourceRoot
  try { Invoke-Checked "git" @("apply", "--reverse", "--check", "--whitespace=nowarn", $patchPath) }
  finally { Pop-Location }
  Write-Host "Verified existing llama.cpp output-reorder patch: $sourceRoot"
  return
}

Push-Location $sourceRoot
try {
  Invoke-Checked "git" @("apply", "--check", "--whitespace=nowarn", $patchPath)
  Invoke-Checked "git" @("apply", "--whitespace=nowarn", $patchPath)
} finally { Pop-Location }

[System.IO.File]::WriteAllText($marker, "source_commit=$actualCommit`npatch_sha256=$patchHash`n", [System.Text.UTF8Encoding]::new($false))
Write-Host "Applied llama.cpp output-reorder patch to $sourceRoot"
