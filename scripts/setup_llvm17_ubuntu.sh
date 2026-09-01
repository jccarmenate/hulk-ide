#!/usr/bin/env bash
# LLVM 17 toolchain setup for building hulk-codegen on Ubuntu/WSL (secondary dev environment).
# The compiler produces Linux x86_64 executables (same target as host when running on Linux).
# This script installs all required dependencies: Rust, GCC, LLVM 17, and system libraries.
set -euo pipefail

echo "HULK compiler setup (Ubuntu/WSL)"
echo ""

# ─── 1. Install system build tools (gcc, make, etc.) ─────────────────────

if ! command -v gcc >/dev/null 2>&1 || ! command -v make >/dev/null 2>&1; then
    echo "Installing build-essential (gcc, make, etc.)..."
    sudo apt-get update
    sudo apt-get install -y build-essential
fi

# ─── 2. Install Rust (via rustup) ──────────────────────────────────────────

if ! command -v cargo >/dev/null 2>&1; then
    echo "Installing Rust (via rustup)..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
    # Source the cargo environment for the current session
    source "$HOME/.cargo/env"
else
    echo "Rust already installed: $(rustc --version)"
fi

# Ensure the Rust toolchain is up to date (optional)
rustup update stable

# ─── 3. Install LLVM 17 and all development packages ──────────────────────

# Check if LLVM 17 is already installed via `llvm-config-17`.
if ! command -v llvm-config-17 >/dev/null 2>&1 && ! command -v llvm-config >/dev/null 2>&1; then
    echo "Installing LLVM 17 development files..."
    # Add the official LLVM repository for Ubuntu (if not already present)
    if ! grep -q "apt.llvm.org" /etc/apt/sources.list /etc/apt/sources.list.d/* 2>/dev/null; then
        wget -O - https://apt.llvm.org/llvm-snapshot.gpg.key | sudo apt-key add -
        sudo add-apt-repository "deb http://apt.llvm.org/$(lsb_release -sc)/ llvm-toolchain-$(lsb_release -sc)-17 main"
    fi
    sudo apt-get update
    # Install LLVM 17 and common development packages, plus Polly and compression libs
    sudo apt-get install -y \
        llvm-17 \
        llvm-17-dev \
        clang-17 \
        lld-17 \
        libpolly-17-dev \
        libzstd-dev \
        libtinfo-dev \
        libxml2-dev
fi

# Locate LLVM 17 installation prefix (default for apt.llvm.org packages)
LLVM_PREFIX="/usr/lib/llvm-17"
if [ ! -d "$LLVM_PREFIX" ]; then
    echo "error: LLVM 17 install prefix not found at $LLVM_PREFIX" >&2
    exit 1
fi

export LLVM_SYS_170_PREFIX="$LLVM_PREFIX"
VERSION=$("$LLVM_PREFIX/bin/llvm-config" --version)
echo "Found LLVM $VERSION at $LLVM_PREFIX"
echo ""

if [[ ! "$VERSION" =~ ^17 ]]; then
    echo "warning: hulk-codegen is pinned to LLVM 17.x, but this is $VERSION" >&2
fi

# ─── 4. Set up environment variables ──────────────────────────────────────

# Add LLVM_SYS_170_PREFIX to ~/.bashrc if not already present
if ! grep -q "LLVM_SYS_170_PREFIX" ~/.bashrc 2>/dev/null; then
    echo "export LLVM_SYS_170_PREFIX=$LLVM_PREFIX" >> ~/.bashrc
    echo "Added LLVM_SYS_170_PREFIX to ~/.bashrc"
fi

# Add cargo bin to PATH if not already present (rustup already does this)
# But ensure ~/.cargo/bin is in PATH for this session
if [[ ":$PATH:" != *":$HOME/.cargo/bin:"* ]]; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi

# ─── 5. Verify the setup ──────────────────────────────────────────────────

echo ""
echo "Setup complete! Environment variables are now set for this session."
echo "To make them permanent, restart your shell or run:"
echo "    source ~/.bashrc"
echo ""
echo "Verify the toolchain:"
echo "    rustc --version"
echo "    cargo --version"
echo "    llvm-config --version"
echo ""
echo "Then build the runtime and the smoke test:"
echo "    cargo build -p hulk-rt --release"
echo "    cargo run -p hulk-codegen --example smoke --release"
echo ""
echo "On Linux/WSL the smoke binary executes directly, printing SUCCESS."