<#
.SYNOPSIS
  Build Embassy Studio on Windows.

.DESCRIPTION
  With the MSVC toolchain (Visual Studio Build Tools installed) a plain
  `cargo build --release` is all you need, and this script just does that.

  With the windows-gnu toolchain and no 64-bit MinGW binutils installed,
  rustc cannot create import libraries and GNU ld mis-links them. This script
  then uses only tools shipped with Rust:
    - llvm-ar from the `llvm-tools` component, exposed as `dlltool`
    - rust-lld in MinGW mode as the linker
  Nothing outside this repository's target folder is modified.

.EXAMPLE
  ./build.ps1            # release build -> target/release/embassy-studio.exe
  ./build.ps1 -Debug     # debug build
  ./build.ps1 -Run       # build and launch
#>
param([switch]$Debug, [switch]$Run)

$host_triple = (rustc -vV | Select-String '^host: (.+)$').Matches[0].Groups[1].Value
$profileArgs = if ($Debug) { @() } else { @('--release') }
$profileDir = if ($Debug) { 'debug' } else { 'release' }

if ($host_triple -like '*-windows-gnu') {
    $sysroot = rustc --print sysroot
    $bin = Join-Path $sysroot "lib\rustlib\$host_triple\bin"
    $selfContained = Join-Path $bin 'self-contained'
    $libSelfContained = Join-Path $sysroot "lib\rustlib\$host_triple\lib\self-contained"

    if (-not (Test-Path (Join-Path $bin 'llvm-ar.exe'))) {
        Write-Host 'Installing the llvm-tools rustup component...'
        rustup component add llvm-tools
    }
    $shim = Join-Path $PSScriptRoot 'target\toolshim'
    New-Item -ItemType Directory -Force $shim | Out-Null
    Copy-Item (Join-Path $bin 'llvm-ar.exe') (Join-Path $shim 'dlltool.exe') -Force

    # Keep 32-bit MinGW installs (e.g. C:\MinGW) from shadowing the tools above.
    $env:PATH = (@($shim, $selfContained) + ($env:PATH -split ';' | Where-Object { $_ -and $_ -notmatch '\\MinGW\\bin' })) -join ';'
    $lld = (Join-Path $bin 'rust-lld.exe') -replace '\\', '/'
    $env:RUSTFLAGS = "-C linker=$lld -C linker-flavor=ld.lld -C link-arg=-mi386pep -C link-arg=-L$($libSelfContained -replace '\\','/')"
}

cargo build $profileArgs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$exe = Join-Path $PSScriptRoot "target\$profileDir\embassy-studio.exe"
Write-Host "Built $exe"
if ($Run) { & $exe }
