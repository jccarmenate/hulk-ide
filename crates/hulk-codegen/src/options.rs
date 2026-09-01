//! Compilation options threaded through `hulk_codegen::compile`.

use std::path::PathBuf;
use inkwell::OptimizationLevel as InkwellOpt;

/// Optimization level requested for the generated module.
///
/// The smoke-test path always uses `None` — there is nothing in a one basic
/// block, no-arithmetic module worth optimizing. Real lowering phases wire
/// this into the actual LLVM pass pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptLevel {
    None,
    Less,
    #[default]
    Default,
    Aggressive,
}

impl OptLevel {
    /// Converts to the inkwell `OptimizationLevel` used by the LLVM backend
    /// (target machine instruction selection) and by `PassBuilderOptions`.
    pub fn to_inkwell(self) -> InkwellOpt {
        match self {
            // OptLevel::None maps to inkwell's None (no backend optimizations).
            OptLevel::None => InkwellOpt::None,
            OptLevel::Less => InkwellOpt::Less,
            OptLevel::Default => InkwellOpt::Default,
            OptLevel::Aggressive => InkwellOpt::Aggressive,
        }
    }

    /// Returns the `run_passes` pipeline string for the new pass manager.
    ///
    /// `None` returns `None` (skip `run_passes` entirely), rather than an empty 
    /// string (which would still invoke the pass manager machinery with no passes).
    pub fn pipeline_str(self) -> Option<&'static str> {
        match self {
            OptLevel::None => None,
            OptLevel::Less => Some("default<O1>"),
            OptLevel::Default => Some("default<O2>"),
            OptLevel::Aggressive => Some("default<O3>"),
        }
    }
}

/// Options controlling a single `compile()` invocation.
#[derive(Debug, Clone, Default)]
pub struct CodegenOptions {
    /// If set, the generated LLVM IR is also written to this path as
    /// human-readable text (`.ll`). A development and debugging aid only —
    /// never required for a normal build.
    pub emit_llvm_path: Option<PathBuf>,
    /// Where the final linked native executable should be written. The
    /// compiler driver defaults this to `./output` in the current working
    /// directory; it is only overridden here for tests and tooling such as
    /// the Phase 1 smoke example.
    pub output_path: PathBuf,
    pub opt_level: OptLevel,
}

impl CodegenOptions {
    pub fn with_output_path(output_path: impl Into<PathBuf>) -> Self {
        Self {
            output_path: output_path.into(),
            ..Default::default()
        }
    }
}
