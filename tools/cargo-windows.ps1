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

$Linker = Join-Path $TcBin "x86_64-w64-mingw32-gcc.exe"
if (-not (Test-Path $Linker)) {
  $needDownload = $true
  if ((Test-Path $TcZip) -and ((Get-Item $TcZip).Length -eq $ZipSize)) { $needDownload = $false }
  if ($needDownload) {
    New-Item -ItemType Directory -Force -Path $ToolRoot | Out-Null
    Write-Host "Downloading LLVM-MinGW (~191MB, one time only)..."
    Invoke-WebRequest -Uri $ZipUrl -OutFile $TcZip -UseBasicParsing
  }
  Write-Host "Extracting MinGW toolchain..."
  Expand-Archive -Path $TcZip -DestinationPath $ToolDir -Force
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
$env:RUSTFLAGS = "-C linker=x86_64-w64-mingw32-gcc -C link-arg=-lunwind"

$RepoRoot = Split-Path $PSScriptRoot -Parent
Push-Location $RepoRoot
try {
  & cargo @CargoArgs
  exit $LASTEXITCODE
} finally {
  Pop-Location
}
