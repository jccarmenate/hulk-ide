//! Native object-file emission.
//!
//! HULK executables always target `x86_64-unknown-linux-gnu`. The LLVM
//! target machine is always initialized for the Linux triple, the CPU
//! generic (Haswell as a reasonable default for compatibility), and
//! position-independent code (to support modern linker hardening).

use std::path::Path;

use inkwell::module::Module;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine, TargetTriple,
};

use crate::error::CodegenError;
use crate::options::OptLevel;

const TARGET_TRIPLE: &str = "x86_64-unknown-linux-gnu";

/// Initializes all LLVM backends. Must be called once before any
/// `TargetMachine` is created.
pub fn init_all_targets() -> Result<(), CodegenError> {
    Target::initialize_all(&InitializationConfig::default());
    Ok(())
}

/// Builds a `TargetMachine` for the Linux x86_64 target at the requested
/// optimization level.
///
/// The optimization level affects machine-code quality (instruction selection,
/// register allocation, scheduling) independently of the IR-level passes.
pub fn linux_x86_64_target_machine(opt: OptLevel) -> Result<TargetMachine, CodegenError> {
    let triple = TargetTriple::create(TARGET_TRIPLE);
    let target = Target::from_triple(&triple).map_err(|e| {
        CodegenError::target_emission(format!("could not find Linux x86_64 target: {e}"))
    })?;

    target
        .create_target_machine(
            &triple,
            "haswell",
            "+cmov",
            opt.to_inkwell(),
            RelocMode::PIC,
            CodeModel::Small,
        )
        .ok_or_else(|| {
            CodegenError::target_emission(
                "could not create a target machine for x86_64-unknown-linux-gnu",
            )
        })
}

/// Writes `module` as a relocatable object file at `path`, using `machine`.
pub fn write_object_file(
    machine: &TargetMachine,
    module: &Module,
    path: &Path,
) -> Result<(), CodegenError> {
    machine
        .write_to_file(module, FileType::Object, path)
        .map_err(|e| CodegenError::target_emission(e.to_string()))
}
