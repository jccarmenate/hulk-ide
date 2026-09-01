//! Phase 1 end-to-end smoke test: builds the smoke module, emits it as a
//! Linux x86_64 object file, and links it against `hulk-rt` to produce a
//! Linux executable.
//!
//! This runs on Windows (the development machine) but produces a Linux
//! binary. The final execution test is deferred to WSL/Ubuntu, since
//! Windows cannot directly run ELF binaries — this is a cross-compilation
//! scenario in the strict sense.
//!
//! On Windows:
//!
//!     cargo build -p hulk-rt --release
//!     cargo run -p hulk-codegen --example smoke --release
//!
//! This emits `smoke` (an ELF Linux binary) and prints the path.
//! Copy that binary to your WSL/Ubuntu environment and run it there:
//!
//!     wsl /path/to/smoke
//!     # should exit 0 with no output
//!
//! The presence of a valid Linux ELF binary on Windows proves the
//! IR -> object file -> link chain works end to end. Execution validation
//! happens in the target environment (Linux/WSL).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use inkwell::context::Context;

use hulk_codegen::{emit, OptLevel};

fn main() {
    let work_dir = std::env::temp_dir().join("hulk_codegen_smoke");
    std::fs::create_dir_all(&work_dir).expect("create work dir");

    let context = Context::create();
    let ctx =
        hulk_codegen::build_smoke_module(&context).expect("smoke module should build and verify");

    let ll_path = work_dir.join("smoke.ll");
    hulk_codegen::emit_llvm_ir_to_file(&ctx, &ll_path).expect("write .ll");
    println!("wrote IR to {}", ll_path.display());

    emit::init_all_targets().expect("initialize LLVM targets");
    let machine = emit::linux_x86_64_target_machine(OptLevel::None).expect("create Linux x86_64 target machine");

    let obj_path = work_dir.join("smoke.o");
    emit::write_object_file(&machine, &ctx.module, &obj_path).expect("write object file");
    println!("wrote object file (Linux x86_64) to {}", obj_path.display());

    let exe_path = work_dir.join("smoke");
    let rt_lib_dir = locate_hulk_rt_lib_dir();

    // On Windows, use clang with explicit Linux target. On other platforms,
    // use cc (which will be gcc/clang that already understands the target).
    let (linker, target_flag) = if cfg!(windows) {
        ("clang", Some("-target=x86_64-unknown-linux-gnu"))
    } else {
        ("cc", None)
    };

    let mut cmd = Command::new(linker);
    cmd.arg(&obj_path)
        .arg("-L")
        .arg(&rt_lib_dir)
        .arg("-lhulk_rt");
    if let Some(flag) = target_flag {
        cmd.arg(flag);
    }
    cmd.arg("-o").arg(&exe_path);

    let status = cmd.status().unwrap_or_else(|e| {
        panic!(
            "failed to invoke linker `{linker}`. On Windows, ensure clang is installed \
             (part of LLVM 17 or available via vcpkg). Error: {e}"
        )
    });

    assert!(status.success(), "link step failed (linker: {linker})");
    println!("linked Linux x86_64 executable to {}", exe_path.display());

    // Validate that the resulting file is a valid ELF binary by checking its magic.
    validate_elf_binary(&exe_path).expect("executable should be valid ELF");

    println!();
    println!("SUCCESS: IR -> object -> link chain verified (cross-compilation to Linux x86_64).");
    println!();
    println!(
        "Next step: copy {} to your WSL/Ubuntu environment and run it:",
        exe_path.display()
    );
    println!("    wsl {}", exe_path.display());
    println!("    # should exit 0 with no output");
}

/// Locates the directory containing the compiled `hulk-rt` static library
/// under the workspace's `target` directory, in whichever build profile was
/// produced most recently.
///
/// `hulk-codegen` never depends on `hulk-rt` as a Cargo dependency — it only
/// needs to know its symbol names (declared as `extern "C"` in generated
/// IR) and, at link time, the location of its compiled artifact. This
/// mirrors how any native toolchain treats its runtime library: linked
/// against, not compiled against.
fn locate_hulk_rt_lib_dir() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_target = manifest_dir
        .parent() // crates/
        .and_then(Path::parent) // workspace root
        .map(|root| root.join("target"))
        .expect("locate workspace target directory");

    for profile in ["release", "debug"] {
        let candidate = workspace_target.join(profile);
        if candidate.join("libhulk_rt.a").exists() || candidate.join("hulk_rt.lib").exists() {
            return candidate;
        }
    }

    panic!(
        "could not find a built `hulk-rt` static library under {}. \
         Run `cargo build -p hulk-rt --release` first.",
        workspace_target.display()
    );
}

/// Validates that the file at `path` is a valid ELF binary by checking
/// the magic number (first four bytes: `\x7fELF`). This is a cheap smoke
/// test to ensure the linker produced a real Linux binary, not a Windows
/// PE or some other format.
fn validate_elf_binary(path: &Path) -> std::io::Result<()> {
    let mut file = std::fs::File::open(path)?;
    let mut header = [0u8; 4];
    file.read_exact(&mut header)?;

    if header == [0x7f, b'E', b'L', b'F'] {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("not an ELF binary; first bytes: {:02x?}", header),
        ))
    }
}
