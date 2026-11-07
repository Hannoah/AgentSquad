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
  [string]$Version = "latest",
  [string]$Token = ""
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($Repo)) {
  throw "Set -Repo to your GitHub repo, e.g. -Repo 'Hannoah/AgentSquad'."
}

$Asset = "squad-windows-x86_64.zip"
$shaAsset = "$Asset.sha256"
$apiHeaders = @{}
if (-not [string]::IsNullOrWhiteSpace($Token)) {
  $apiHeaders["Authorization"] = "Bearer $Token"
}
$dlSha = $null
if ($Version -eq "latest") {
  # No API call here: the /latest/download redirect needs no
  # authentication and is not subject to API rate limits.
  $tagLabel = "latest"
  $dl = "https://github.com/$Repo/releases/latest/download/$Asset"
  $dlSha = "https://github.com/$Repo/releases/latest/download/$shaAsset"
} else {
  $tag = $Version
  if (-not $tag.StartsWith("v")) { $tag = "v$tag" }
  $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/tags/$tag" -Headers $apiHeaders
  $tagLabel = $rel.tag_name
  $dl = ($rel.assets | Where-Object { $_.name -eq $Asset }).browser_download_url
  if (-not $dl) { throw "Asset '$Asset' not found in release $tagLabel." }
  $dlSha = ($rel.assets | Where-Object { $_.name -eq $shaAsset }).browser_download_url
}

$tmp = Join-Path $env:TEMP "squad-install"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$zip = Join-Path $tmp $Asset
Write-Host "Downloading squad $tagLabel..."
Invoke-WebRequest -Uri $dl -OutFile $zip -UseBasicParsing

# Verify SHA256 if .sha256 asset is available
if ($dlSha) {
  try {
    $shaFile = Join-Path $tmp $shaAsset
    Invoke-WebRequest -Uri $dlSha -OutFile $shaFile -UseBasicParsing -ErrorAction SilentlyContinue
    if (Test-Path $shaFile) {
      $expectedHash = ((Get-Content $shaFile -Raw).Trim() -split '\s+')[0].ToLowerInvariant()
      if ($expectedHash.Length -eq 64) {
        $actualHash = (Get-FileHash -Path $zip -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actualHash -ne $expectedHash) {
          throw "SHA256 checksum mismatch! Expected $expectedHash, got $actualHash."
        }
        Write-Host "SHA256 verified: $actualHash"
      }
    }
  } catch [System.Management.Automation.RuntimeException] {
    if ($_.Exception.Message -like "*checksum mismatch*") { throw }
  } catch {
    if ($_.Exception.Message -like "*checksum mismatch*") { throw }
  }
}
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$running = Get-Process -Name "squad" -ErrorAction SilentlyContinue
if ($running) {
  $pids = ($running | Select-Object -ExpandProperty Id) -join ", "
  throw "squad is running (PID $pids) and Windows cannot replace a running program. Quit every squad manager (q in each window) and run the installer again."
}
try {
  Expand-Archive -Path $zip -DestinationPath $InstallDir -Force
} catch {
  throw "Could not write to $InstallDir ($($_.Exception.Message)). If squad is running, quit it first and run the installer again."
}
Write-Host "Installed to $InstallDir"

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (($userPath -split ";") -notcontains $InstallDir) {
  # Append to the *user* PATH only; $env:Path also holds machine entries.
  $newUserPath = if ([string]::IsNullOrEmpty($userPath)) { $InstallDir } else { $userPath.TrimEnd(";") + ";$InstallDir" }
  [Environment]::SetEnvironmentVariable("Path", $newUserPath, "User")
  # iex runs inside the current terminal, so refresh this session too.
  # New terminals pick up the User PATH on their own.
  $env:Path += ";$InstallDir"
  Write-Host "Added to user PATH."
}

# A freshly downloaded exe can fail its first launch while antivirus
# finishes scanning it, so retry before giving up. Files are already in
# place at this point, so a persistent failure warns instead of throwing.
$verified = $false
for ($i = 0; $i -lt 3 -and -not $verified; $i++) {
  if ($i -gt 0) { Start-Sleep -Seconds 3 }
  try {
    & (Join-Path $InstallDir "squad.exe") --version
    $verified = $true
  } catch {
    if ($i -eq 2) {
      Write-Warning "Installed to $InstallDir, but the launch check failed: $($_.Exception.Message). Run 'squad --version' yourself."
    }
  }
}
