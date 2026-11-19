<#
.SYNOPSIS
  Build helper for this repo on Windows with the GNU toolchain.

.DESCRIPTION
  The default Rust GNU toolchain on Windows needs an external MinGW
  linker (dlltool/gcc). Installing one requires admin rights, so this
  script bootstraps a portable LLVM-MinGW copy under $env:TEMP
  (downloaded once, reused afterwards) plus two tiny local shims, then
  forwards everything to cargo.

  Repo files are not modified by the setup part; only %TEMP% is used.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File tools\cargo-windows.ps1 build
  powershell -ExecutionPolicy Bypass -File tools\cargo-windows.ps1 test
#>
[CmdletBinding()]
param(
  [Parameter(ValueFromRemainingArguments = $true)]
  [string[]]$CargoArgs
)

$ErrorActionPreference = "Stop"

if (-not $CargoArgs -or $CargoArgs.Count -eq 0) { $CargoArgs = @("build") }

$ToolRoot = Join-Path $env:TEMP "agentsquad-tc"
$ToolDir  = Join-Path $ToolRoot "llvm-mingw"
$TcDir    = Join-Path $ToolDir "llvm-mingw-20261104-ucrt-x86_64"
$TcBin    = Join-Path $TcDir "bin"
$LibDir   = Join-Path $TcDir "x86_64-w64-mingw32\lib"
$TcZip    = Join-Path $ToolRoot "llvm-mingw.zip"
$ZipUrl   = "https://github.com/mstorsjo/llvm-mingw/releases/download/20261104/llvm-mingw-20261104-ucrt-x86_64.zip"
$ZipSize  = 191037046
# SHA256 of the upstream asset (GitHub API `digest` field). Verified on every
# run, including cached copies: a toolchain that builds every binary must not
# come from an unverified 191MB download.
$ZipSha256 = "552191b5754c49a5a20723276a6fc56541ea1922c4c50b7b669022333fc1b40f"

function Test-ToolchainZip {
  if (-not (Test-Path $TcZip)) { return $false }
  if ((Get-Item $TcZip).Length -ne $ZipSize) { return $false }
  return ((Get-FileHash -Path $TcZip -Algorithm SHA256).Hash.ToLowerInvariant() -eq $ZipSha256)
}

$Linker = Join-Path $TcBin "x86_64-w64-mingw32-gcc.exe"
if (-not (Test-Path $Linker)) {
  if (-not (Test-ToolchainZip)) {
    New-Item -ItemType Directory -Force -Path $ToolRoot | Out-Null
    Write-Host "Downloading LLVM-MinGW (~191MB, one time only)..."
    Invoke-WebRequest -Uri $ZipUrl -OutFile $TcZip -UseBasicParsing
    if (-not (Test-ToolchainZip)) {
      Remove-Item -Path $TcZip -Force -ErrorAction SilentlyContinue
      throw "Toolchain checksum mismatch; refusing to build with an unverified compiler. Delete $ToolRoot and retry."
    }
  }
  Write-Host "Extracting MinGW toolchain..."
  Expand-Archive -Path $TcZip -DestinationPath $ToolDir -Force
}

$dllImport = Join-Path $LibDir "libunwind.dll.a"
if (Test-Path $dllImport) {
  # Force static libunwind: something in the default link line also passes
  # -lunwind, which the linker would resolve to this DLL import library and
  # leave squad.exe dependent on libunwind.dll. Renaming it here (TEMP copy
  # only) makes -lunwind fall back to the static libunwind.a.
  Rename-Item -Path $dllImport -NewName "libunwind.dll.a.unused"
}

foreach ($lib in @("libgcc.a", "libgcc_eh.a")) {
  if (-not (Test-Path (Join-Path $LibDir $lib))) {
    $emptyObj = Join-Path $ToolRoot "empty.o"
    if (-not (Test-Path $emptyObj)) {
      Set-Content -Path (Join-Path $ToolRoot "empty.c") -Value "void agentsquad_shim_anchor(void) {}"
      & (Join-Path $TcBin "clang.exe") -c (Join-Path $ToolRoot "empty.c") -o $emptyObj
    }
    # llvm-mingw is clang-based and ships no libgcc; rustc's
    # windows-gnu target still asks for -lgcc/-lgcc_eh. An object
    # holding one unused symbol satisfies lookup; real unwinding and
    # builtins come from libunwind/compiler-rt (see RUSTFLAGS below).
    & (Join-Path $TcBin "llvm-ar.exe") rcs (Join-Path $LibDir $lib) $emptyObj
  }
}

$env:PATH = "$TcBin;" + $env:PATH
$env:RUSTFLAGS = "-C linker=x86_64-w64-mingw32-gcc -C link-arg=-l:libunwind.a"

$RepoRoot = Split-Path $PSScriptRoot -Parent
Push-Location $RepoRoot
try {
  & cargo @CargoArgs
  exit $LASTEXITCODE
} finally {
  Pop-Location
}
