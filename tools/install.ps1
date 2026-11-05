<#
.SYNOPSIS
  Installs squad from GitHub Releases (Windows).

.DESCRIPTION
  Downloads squad-windows-x86_64.zip from the latest (or a pinned)
  GitHub Release, extracts squad.exe into %LOCALAPPDATA%\AgentSquad\bin
  and adds that folder to the user PATH.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File tools\install.ps1 -Repo myname/agentsquad
  powershell -ExecutionPolicy Bypass -File tools\install.ps1 -Repo myname/agentsquad -Version v0.1.0
#>
[CmdletBinding()]
param(
  [string]$Repo = "Hannoah/AgentSquad",
  [string]$InstallDir = (Join-Path $env:LOCALAPPDATA "AgentSquad\bin"),
  [string]$Version = "latest"
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($Repo)) {
  throw "Set -Repo to your GitHub repo, e.g. -Repo 'Hannoah/AgentSquad'."
}

$Asset = "squad-windows-x86_64.zip"
if ($Version -eq "latest") {
  $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest"
} else {
  $tag = $Version
  if (-not $tag.StartsWith("v")) { $tag = "v$tag" }
  $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/tags/$tag"
}

$dl = ($rel.assets | Where-Object { $_.name -eq $Asset }).browser_download_url
if (-not $dl) { throw "Asset '$Asset' not found in release $($rel.tag_name)." }

$tmp = Join-Path $env:TEMP "squad-install"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$zip = Join-Path $tmp $Asset
Write-Host "Downloading squad $($rel.tag_name)..."
Invoke-WebRequest -Uri $dl -OutFile $zip -UseBasicParsing
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Expand-Archive -Path $zip -DestinationPath $InstallDir -Force
Write-Host "Installed to $InstallDir"

if (($env:Path -split ";") -notcontains $InstallDir) {
  [Environment]::SetEnvironmentVariable("Path", $env:Path + ";$InstallDir", "User")
  # iex runs inside the current terminal, so refresh this session too.
  # New terminals pick up the User PATH on their own.
  $env:Path += ";$InstallDir"
  Write-Host "Added to user PATH."
}

& (Join-Path $InstallDir "squad.exe") --version
