//! Build-time toolchain check.
//!
//! `inkwell`'s `llvm17-0` feature (selected in `Cargo.toml`) binds this crate
//! to LLVM 17.x at compile time through its `llvm-sys` dependency. If the
//! LLVM development files available on the machine don't match, the real
//! failure otherwise surfaces deep inside `llvm-sys`'s own build script with
//! a much less actionable message. This script runs first and fails fast
//! with a clear explanation whenever that mismatch is detectable up front.
//!
//! This deliberately does NOT replace `llvm-sys`'s own discovery logic
//! (the `LLVM_SYS_170_PREFIX` environment variable, `llvm-config` on `PATH`,
//! versioned binary names such as `llvm-config-17`, and so on) — it only
//! adds an earlier, friendlier diagnostic layer in front of it. See
//! `README.md` in this crate for the setup steps this assumes, on both
//! Ubuntu/WSL and Windows.

use std::env;
use std::path::PathBuf;
use std::process::Command;

const REQUIRED_MAJOR: &str = "17";

fn main() {
    println!("cargo:rerun-if-env-changed=LLVM_SYS_170_PREFIX");

    let llvm_config = match locate_llvm_config() {
        Some(path) => path,
        None => {
            println!(
                "cargo:warning=hulk-codegen: could not locate an `llvm-config` binary on PATH \
                 or via LLVM_SYS_170_PREFIX. This is only a soft warning here — llvm-sys runs \
                 its own, more thorough discovery next and will fail the build with full detail \
                 if LLVM 17 truly isn't reachable. See crates/hulk-codegen/README.md for setup \
                 instructions."
            );
            return;
        }
    };

    match read_llvm_config_version(&llvm_config) {
        Some(version) if version.starts_with(REQUIRED_MAJOR) => {
            println!(
                "cargo:warning=hulk-codegen: using LLVM {version} via {}",
                llvm_config.display()
            );
        }
        Some(version) => {
            panic!(
                "hulk-codegen is pinned to LLVM {REQUIRED_MAJOR}.x (inkwell feature \
                 \"llvm17-0\"), but `{}` reports version {version}. Either point \
                 LLVM_SYS_170_PREFIX at an LLVM 17 install, or change the `inkwell` feature \
                 flag in crates/hulk-codegen/Cargo.toml to match the LLVM version actually \
                 installed on this machine.",
                llvm_config.display()
            );
        }
        None => {
            println!(
                "cargo:warning=hulk-codegen: found `{}` but could not read its version; \
                 continuing and letting llvm-sys's own discovery validate it.",
                llvm_config.display()
            );
        }
    }
}

fn locate_llvm_config() -> Option<PathBuf> {
    if let Ok(prefix) = env::var("LLVM_SYS_170_PREFIX") {
        for name in ["llvm-config", "llvm-config.exe"] {
            let candidate = PathBuf::from(&prefix).join("bin").join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    for name in ["llvm-config-17", "llvm-config", "llvm-config.exe"] {
        if Command::new(name).arg("--version").output().is_ok() {
            return Some(PathBuf::from(name));
        }
    }

    None
}

fn read_llvm_config_version(llvm_config: &PathBuf) -> Option<String> {
    let output = Command::new(llvm_config).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
