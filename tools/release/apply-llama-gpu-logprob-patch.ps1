param(
  [Parameter(Mandatory = $true)]
  [string]$SourceDirectory,
  [string]$PatchFile = "resources/runtime/llama.cpp-b10743-output-reorder.patch"
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path
function Resolve-RepoPath([string]$Path) {
  if ([System.IO.Path]::IsPathRooted($Path)) { return [System.IO.Path]::GetFullPath($Path) }
  return [System.IO.Path]::GetFullPath((Join-Path $repoRoot $Path))
}

$sourceRoot = Resolve-RepoPath $SourceDirectory
$patchPath = Resolve-RepoPath $PatchFile
if (-not (Test-Path -LiteralPath $sourceRoot -PathType Container)) { throw "llama.cpp source directory does not exist: $sourceRoot" }
if (-not (Test-Path -LiteralPath $patchPath -PathType Leaf)) { throw "llama.cpp patch file does not exist: $patchPath" }

$marker = Join-Path $sourceRoot ".lime-output-reorder-patched"
$patchHash = (Get-FileHash -LiteralPath $patchPath -Algorithm SHA256).Hash.ToLowerInvariant()
if (Test-Path -LiteralPath $marker -PathType Leaf) {
  $markerText = Get-Content -LiteralPath $marker -Raw
  if ($markerText -match "sha256=$patchHash") { Write-Host "llama.cpp output-reorder patch already applied: $sourceRoot"; exit 0 }
  throw "llama.cpp source has an incompatible Lime patch marker: $marker"
}

Push-Location $sourceRoot
try {
  & git apply --no-index --ignore-space-change --ignore-whitespace --check --unsafe-paths $patchPath
  if ($LASTEXITCODE -ne 0) { throw "llama.cpp output-reorder patch does not apply cleanly to $sourceRoot" }
  & git apply --no-index --ignore-space-change --ignore-whitespace --unsafe-paths $patchPath
  if ($LASTEXITCODE -ne 0) { throw "failed to apply llama.cpp output-reorder patch to $sourceRoot" }
} finally { Pop-Location }

[System.IO.File]::WriteAllText($marker, "patch=llama.cpp-b10743-output-reorder`nsha256=$patchHash`n", [System.Text.UTF8Encoding]::new($false))
Write-Host "Applied llama.cpp output-reorder patch to $sourceRoot"
