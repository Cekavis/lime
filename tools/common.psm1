Set-StrictMode -Version Latest

function Resolve-RepoPath([string]$RepoRoot, [string]$Path) {
  if ([System.IO.Path]::IsPathRooted($Path)) { return [System.IO.Path]::GetFullPath($Path) }
  return [System.IO.Path]::GetFullPath((Join-Path $RepoRoot $Path))
}

function Assert-UnderOut([string]$RepoRoot, [string]$Path) {
  $outRoot = Resolve-RepoPath $RepoRoot "out"
  $fullPath = [System.IO.Path]::GetFullPath($Path)
  $prefix = $outRoot.TrimEnd([char]92, [char]47) + [System.IO.Path]::DirectorySeparatorChar
  if (-not $fullPath.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Generated path must remain under ${outRoot}: $fullPath"
  }
}

function Resolve-7Zip {
  foreach ($name in @("7z.exe", "7za.exe")) {
    $command = Get-Command $name -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
  }
  $candidates = @(
    "C:\Program Files\7-Zip\7z.exe",
    "C:\Program Files (x86)\7-Zip\7z.exe"
  )
  $found = $candidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
  if ($found) { return $found }
  throw "7-Zip was not found; install 7-Zip or put 7z.exe on PATH"
}

function Invoke-Checked([string]$FilePath, [string[]]$Arguments) {
  & $FilePath @Arguments
  if ($LASTEXITCODE -ne 0) { throw "$FilePath exited with code $LASTEXITCODE" }
}

function Invoke-VerifiedDownload([string]$Url, [string]$Path, [string]$Sha256) {
  $parent = Split-Path -Parent $Path
  New-Item -ItemType Directory -Path $parent -Force | Out-Null
  if (Test-Path -LiteralPath $Path) {
    $existing = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($existing -eq $Sha256.ToLowerInvariant()) {
      Write-Host "Using verified cache: $Path"
      return
    }
    Remove-Item -LiteralPath $Path -Force
  }
  Write-Host "Downloading $Url"
  Invoke-Checked "curl.exe" @("-L", "--fail", "--retry", "3", "--output", $Path, $Url)
  $actual = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
  if ($actual -ne $Sha256.ToLowerInvariant()) {
    throw "SHA-256 mismatch for $Path (expected $Sha256, got $actual)"
  }
}

function Invoke-7ZipExtract([string]$SevenZip, [string]$Archive, [string]$Destination) {
  if (Test-Path -LiteralPath $Destination) { Remove-Item -LiteralPath $Destination -Recurse -Force }
  New-Item -ItemType Directory -Path $Destination -Force | Out-Null
  Invoke-Checked $SevenZip @("x", "-y", "-o$Destination", $Archive)
}

Export-ModuleMember -Function Resolve-RepoPath, Assert-UnderOut, Resolve-7Zip, Invoke-Checked, Invoke-VerifiedDownload, Invoke-7ZipExtract
