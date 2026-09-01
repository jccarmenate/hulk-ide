//! IR-level optimization pipeline.
//!
//! Invoked after the full module has been lowered and verified, and before
//! object-file emission. Uses LLVM 17's pass manager via `Module::run_passes`, 
//! which accepts the same pipeline strings as `opt -passes=...` on the command line.

use inkwell::passes::PassBuilderOptions;

use crate::context::CodegenCtx;
use crate::error::CodegenError;
use crate::options::OptLevel;

/// Runs the optimization pipeline on `ctx.module` at the level specified
/// by `opt`.
///
/// If `opt` is `OptLevel::None`, this function is a no-op, as the module is
/// left in its post-lowering, pre-optimization state. This is the correct
/// behaviour for debug builds and tests that assert on unoptimized IR shape.
///
/// # Correctness guarantee
/// - Pointer-typed allocas registered with `hulk_rt_shadow_push` are
///   address-taken and are excluded from `mem2reg` and SROA automatically.
/// - Retain/release and shadow push/pop calls are external functions;
///   `instcombine` and `simplifycfg` cannot remove them.
/// - Inlining preserves the push/pop balance because pushes and pops are
///   emitted at HULK scope boundaries, not LLVM function boundaries.
///
/// # Errors
/// Returns `CodegenError::Optimization` if `run_passes` reports an LLVM
/// diagnostic (malformed pass name, pass precondition failure, etc.).
pub fn optimize(ctx: &CodegenCtx, opt: OptLevel) -> Result<(), CodegenError> {
    // Skip the pass manager machinery entirely for OptLevel::None.
    let pipeline = match opt.pipeline_str() {
        Some(p) => p,
        None => return Ok(()),
    };

    let pass_opts = PassBuilderOptions::create();

    // run_passes requires the TargetMachine so passes that need target-specific 
    // information can query it. Using the same machine that was used to set the 
    // module's data layout guarantees consistency.
    ctx.module
        .run_passes(pipeline, &ctx.target_machine, pass_opts)
        .map_err(|e| CodegenError::optimization(e.to_string()))
}