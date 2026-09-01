# LLVM 17 toolchain setup for building hulk-codegen on Windows (primary dev environment).
# The compiler is built on Windows but produces Linux x86_64 executables.

$ErrorActionPreference = "Stop"

Write-Host "HULK cross-compiler setup (Windows -> Linux x86_64)" -ForegroundColor Green
Write-Host ""

$llvmConfig = Get-Command "llvm-config.exe" -ErrorAction SilentlyContinue

if (-not $llvmConfig) {
    Write-Host "llvm-config.exe was not found on PATH."
    Write-Host ""
    Write-Host "Option 1: Official LLVM Windows release (may not ship llvm-config.exe)"
    Write-Host "  - Download LLVM 17 from https://github.com/llvm/llvm-project/releases"
    Write-Host "  - If it lacks llvm-config.exe, use Option 2 instead"
    Write-Host ""
    Write-Host "Option 2: vcpkg (recommended, includes llvm-config.exe)"
    Write-Host "  git clone https://github.com/microsoft/vcpkg"
    Write-Host "  .\vcpkg\bootstrap-vcpkg.bat"
    Write-Host "  .\vcpkg\vcpkg install llvm[core,target-x86]:x64-windows"
    Write-Host ""
    Write-Host "Then set the environment variable to the install path, e.g.:"
    Write-Host '  $env:LLVM_SYS_170_PREFIX = "C:\path\to\vcpkg\installed\x64-windows"'
    Write-Host ""
    exit 1
}

$version = & $llvmConfig.Source --version
Write-Host "Found llvm-config.exe reporting version $version"

if (-not $version.StartsWith("17")) {
    Write-Host "WARNING: hulk-codegen is pinned to LLVM 17.x, but this reports $version" -ForegroundColor Yellow
}

$prefix = Split-Path -Parent (Split-Path -Parent $llvmConfig.Source)
Write-Host ""
Write-Host "Set this environment variable (add to System Properties > Environment Variables):"
Write-Host ""
Write-Host "`$env:LLVM_SYS_170_PREFIX = `"$prefix`""
Write-Host ""
Write-Host "Verify the setup works (once the env var is set):"
Write-Host ""
Write-Host "  cargo build -p hulk-rt --release"
Write-Host "  cargo run -p hulk-codegen --example smoke --release"
Write-Host ""
Write-Host "This builds LLVM IR, emits it as a Linux x86_64 object file, and links it"
Write-Host "into a Linux executable (visible as valid ELF binary on Windows)."
Write-Host "Copy the printed binary path to WSL and run it to confirm execution:"
Write-Host ""
Write-Host "  wsl <binary-path>"
