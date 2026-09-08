param(
  [string]$OutputDirectory = "out/windows-x64/sources"
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Import-Module (Join-Path $PSScriptRoot "common.psm1") -Force

function Sync-PinnedRepository([string]$Name, [string]$Url, [string]$Ref, [string]$Commit, [string]$Destination) {
  Assert-UnderOut $repoRoot $Destination
  if (Test-Path -LiteralPath $Destination) {
    $actual = (& git -C $Destination rev-parse HEAD).Trim()
    if ($actual -eq $Commit) {
      Write-Host "Using pinned $Name source at $actual"
      return
    }
    Remove-Item -LiteralPath $Destination -Recurse -Force
  }
  New-Item -ItemType Directory -Path (Split-Path -Parent $Destination) -Force | Out-Null
  Invoke-Checked "git" @("clone", "--filter=blob:none", "--no-checkout", "--no-tags", $Url, $Destination)
  Invoke-Checked "git" @("-C", $Destination, "fetch", "--depth", "1", "origin", $Ref)
  Invoke-Checked "git" @("-C", $Destination, "checkout", "--detach", $Commit)
  $actual = (& git -C $Destination rev-parse HEAD).Trim()
  if ($actual -ne $Commit) {
    throw "$Name source resolved to $actual, expected pinned commit $Commit"
  }
}

$rimeManifest = Get-Content -LiteralPath (Resolve-RepoPath $repoRoot "third_party/rime/librime-1.17.0.json") -Raw | ConvertFrom-Json
$llamaManifest = Get-Content -LiteralPath (Resolve-RepoPath $repoRoot "third_party/llama/b10743/source.json") -Raw | ConvertFrom-Json
$root = Resolve-RepoPath $repoRoot $OutputDirectory
Assert-UnderOut $repoRoot $root

Sync-PinnedRepository "librime" "https://github.com/rime/librime.git" $rimeManifest.librime.tag $rimeManifest.librime.source_commit (Join-Path $root "rime/librime")
Sync-PinnedRepository "rime-ice" "https://github.com/iDvel/rime-ice.git" $rimeManifest.rime_ice.version $rimeManifest.rime_ice.source_commit (Join-Path $root "rime/rime-ice")
Sync-PinnedRepository "llama.cpp" "https://github.com/ggml-org/llama.cpp.git" $llamaManifest.source_commit $llamaManifest.source_commit (Join-Path $root "llama/b10743")

Write-Host "Pinned third-party sources are available under $root"
