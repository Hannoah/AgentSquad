<#
.SYNOPSIS
  Bumps version, runs CI checks, builds and deploys squad.exe locally, commits, tags, and pushes to trigger GitHub Release.

.DESCRIPTION
  Automates the full release pipeline for AgentSquad:
    1. Bumps version in Cargo.toml, Cargo.lock, README.md, and wiki/releases.md
    2. Runs cargo fmt, cargo fmt --check, cargo clippy --all-targets -- -D warnings, cargo test --locked, and cargo build --release --locked
    3. Deploys target\release\squad.exe to %LOCALAPPDATA%\AgentSquad\bin\squad.exe (moving aside a live binary if running)
    4. Commits all changes as "Release vX.Y.Z: <Summary>", creates tag vX.Y.Z, and pushes main + tag to origin

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File tools\release.ps1 -Summary "collapsible mini sidebar" -Notes "Release notes..."
  powershell -ExecutionPolicy Bypass -File tools\release.ps1 -Version 0.16.0 -Summary "feature" -Notes "..."
#>
[CmdletBinding()]
param(
  [string]$Summary = "",
  [string]$Notes = "",
  [string]$NotesFile = "",
  [string]$Version = "",
  [ValidateSet("patch", "minor", "major")]
  [string]$Bump = "patch",
  [switch]$NoInstall,
  [switch]$NoPush
)

$ErrorActionPreference = "Stop"

function Invoke-Checked {
  param(
    [string]$Label,
    [scriptblock]$Command
  )
  Write-Host "==> $Label" -ForegroundColor Cyan
  $global:LASTEXITCODE = 0
  & $Command
  if ($LASTEXITCODE -ne 0) {
    throw "$Label failed with exit code $LASTEXITCODE"
  }
}

$RepoRoot = Split-Path $PSScriptRoot -Parent
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)

Push-Location $RepoRoot
try {
  if ([string]::IsNullOrWhiteSpace($Summary)) {
    throw "-Summary is required (e.g. -Summary 'short English description')."
  }

  $branch = (& git rev-parse --abbrev-ref HEAD).Trim()
  if ($LASTEXITCODE -ne 0) { throw "Failed to determine current git branch." }
  if (-not $NoPush -and $branch -ne "main") {
    throw "Refusing to push release from branch '$branch' (expected 'main')."
  }

  $cargoTomlPath = Join-Path $RepoRoot "Cargo.toml"
  $cargoLockPath = Join-Path $RepoRoot "Cargo.lock"
  $readmePath    = Join-Path $RepoRoot "README.md"
  $releasesPath  = Join-Path $RepoRoot "wiki\releases.md"

  $cargoToml = [System.IO.File]::ReadAllText($cargoTomlPath, $utf8NoBom)
  $verMatch = [regex]::Match($cargoToml, '(?m)^version\s*=\s*"(\d+)\.(\d+)\.(\d+)"')
  if (-not $verMatch.Success) {
    throw "Could not parse package version from Cargo.toml."
  }
  $curMajor = [int]$verMatch.Groups[1].Value
  $curMinor = [int]$verMatch.Groups[2].Value
  $curPatch = [int]$verMatch.Groups[3].Value
  $curVersion = "$curMajor.$curMinor.$curPatch"

  if ([string]::IsNullOrWhiteSpace($Version)) {
    switch ($Bump) {
      "major" { $targetVersion = "$($curMajor + 1).0.0" }
      "minor" { $targetVersion = "$curMajor.$($curMinor + 1).0" }
      "patch" { $targetVersion = "$curMajor.$curMinor.$($curPatch + 1)" }
    }
  } else {
    $clean = $Version.Trim().TrimStart('v')
    if ($clean -notmatch '^\d+\.\d+\.\d+$') {
      throw "Invalid -Version '$Version' (expected X.Y.Z)."
    }
    $targetVersion = $clean
  }

  $tag = "v$targetVersion"
  $existingTag = (& git tag -l $tag)
  if ($LASTEXITCODE -ne 0) { throw "git tag lookup failed." }
  if (-not [string]::IsNullOrWhiteSpace(($existingTag -join ""))) {
    throw "Git tag '$tag' already exists."
  }

  Write-Host "Preparing release $tag (from v$curVersion)..." -ForegroundColor Green

  if ($targetVersion -ne $curVersion) {
    $pkgVerRegex = [regex]'(?m)^version\s*=\s*"\d+\.\d+\.\d+"'
    $cargoTomlNew = $pkgVerRegex.Replace($cargoToml, "version = `"$targetVersion`"", 1)
    [System.IO.File]::WriteAllText($cargoTomlPath, $cargoTomlNew, $utf8NoBom)

    if (Test-Path $cargoLockPath) {
      $cargoLock = [System.IO.File]::ReadAllText($cargoLockPath, $utf8NoBom)
      $lockRegex = [regex]'(?m)(name\s*=\s*"agentsquad"\r?\nversion\s*=\s*")\d+\.\d+\.\d+(")'
      $cargoLockNew = $lockRegex.Replace($cargoLock, "`${1}$targetVersion`$2", 1)
      [System.IO.File]::WriteAllText($cargoLockPath, $cargoLockNew, $utf8NoBom)
    }
  }

  if (Test-Path $readmePath) {
    $readme = [System.IO.File]::ReadAllText($readmePath, $utf8NoBom)
    $readmeNew = [regex]::Replace($readme, '(-Version |SQUAD_VERSION=)v\d+\.\d+\.\d+', "`${1}$tag")
    if ($readmeNew -ne $readme) {
      [System.IO.File]::WriteAllText($readmePath, $readmeNew, $utf8NoBom)
    }
  }

  if (-not [string]::IsNullOrWhiteSpace($NotesFile)) {
    if (-not (Test-Path $NotesFile)) {
      throw "NotesFile not found: $NotesFile"
    }
    $Notes = [System.IO.File]::ReadAllText($NotesFile, $utf8NoBom)
  }

  if ((Test-Path $releasesPath) -and -not [string]::IsNullOrWhiteSpace($Notes)) {
    $wiki = [System.IO.File]::ReadAllText($releasesPath, $utf8NoBom)
    if ($wiki -notmatch [regex]::Escape("| $tag |")) {
      $nl = if ($wiki.Contains("`r`n")) { "`r`n" } else { "`n" }
      $cleanNotes = ($Notes -replace '\r?\n', ' ').Trim()
      # Build Korean literals via codepoints so PowerShell 5.1 parses this script cleanly even without BOM.
      $wordTag  = ([char]0xD0DC) + ([char]0xADF8)
      $wordNote = ([char]0xCC38) + ([char]0xACE0)
      $row = "| $tag | $cleanNotes | $wordTag ``$tag`` $wordNote |"
      $sepRegex = [regex]'(?m)^\|---\|---\|---\|\r*$'
      if (-not $sepRegex.IsMatch($wiki)) {
        throw "Could not find table separator in wiki/releases.md."
      }
      $wikiNew = $sepRegex.Replace($wiki, { "|---|---|---|" + $nl + $row }, 1)
      [System.IO.File]::WriteAllText($releasesPath, $wikiNew, $utf8NoBom)
    }
  }

  Invoke-Checked "cargo fmt" { cargo fmt }
  Invoke-Checked "cargo fmt --check" { cargo fmt --check }
  Invoke-Checked "cargo clippy --all-targets -- -D warnings" { cargo clippy --all-targets -- -D warnings }
  Invoke-Checked "cargo test --locked" { cargo test --locked }
  Invoke-Checked "cargo build --release --locked" { cargo build --release --locked }

  if (-not $NoInstall) {
    $installDir = Join-Path $env:LOCALAPPDATA "AgentSquad\bin"
    $builtExe   = Join-Path $RepoRoot "target\release\squad.exe"
    $targetExe  = Join-Path $installDir "squad.exe"

    if (-not (Test-Path $builtExe)) {
      throw "Release binary not found at $builtExe"
    }

    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    Get-ChildItem -Path $installDir -Filter "squad.exe.old*" -ErrorAction SilentlyContinue | ForEach-Object {
      Remove-Item -Path $_.FullName -Force -ErrorAction SilentlyContinue
    }

    $oldExe = $null
    $running = Get-Process -Name "squad" -ErrorAction SilentlyContinue
    if ($running -and (Test-Path $targetExe)) {
      $oldExe = "$targetExe.old-$PID"
      Move-Item -Path $targetExe -Destination $oldExe -Force
    }

    try {
      try {
        Copy-Item -Path $builtExe -Destination $targetExe -Force
      } catch {
        if ((Test-Path $targetExe) -and -not $oldExe) {
          $oldExe = "$targetExe.old-$PID"
          Move-Item -Path $targetExe -Destination $oldExe -Force
          Copy-Item -Path $builtExe -Destination $targetExe -Force
        } else {
          throw
        }
      }
    } catch {
      if ($oldExe -and (Test-Path $oldExe) -and -not (Test-Path $targetExe)) {
        Move-Item -Path $oldExe -Destination $targetExe -Force -ErrorAction SilentlyContinue
      }
      throw "Could not deploy squad.exe to $installDir ($($_.Exception.Message))."
    }

    $installedVer = (& $targetExe --version).Trim()
    if ($LASTEXITCODE -ne 0 -or $installedVer -ne "squad $targetVersion") {
      throw "Deployed binary version check failed: got '$installedVer', expected 'squad $targetVersion'."
    }
    Write-Host "Deployed $installedVer to $targetExe" -ForegroundColor Green
  }

  Invoke-Checked "git add -A" { git add -A }
  & git diff --cached --quiet
  if ($LASTEXITCODE -ne 0) {
    Invoke-Checked "git commit" { git commit -m "Release ${tag}: $Summary" }
  } else {
    Write-Host "No staged changes to commit." -ForegroundColor Yellow
  }

  Invoke-Checked "git tag $tag" { git tag $tag }

  if (-not $NoPush) {
    Invoke-Checked "git push origin $branch $tag" { git push origin $branch $tag }
    Write-Host "Pushed $branch and tag $tag. GitHub Actions release workflow will publish the release assets." -ForegroundColor Green
  } else {
    Write-Host "Skipped git push (-NoPush). Run 'git push origin $branch $tag' when ready." -ForegroundColor Yellow
  }
} finally {
  Pop-Location
}
