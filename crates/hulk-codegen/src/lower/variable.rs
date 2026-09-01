use hulk_ast::SourceSpan;
use hulk_semantic::Type;
use inkwell::values::BasicValueEnum;

use super::lower_expr;
use crate::error::CodegenError;
use crate::lower::builtins::lookup_constant;
use crate::lower::utils::{convert_to_protocol, is_heap_allocated_type, ensure_boxed};
use crate::lower::LowerCtx;

/// Lowers a variable reference.
///
/// Looks up the variable in the current lexical scope and loads its value
/// from the corresponding `alloca`. If the variable is not found, this
/// returns an error (which should never happen for a semantically verified
/// program).
///
/// # Parameters
/// - `ctx`: the lowering context.
/// - `name`: the variable name.
///
/// # Returns
/// The loaded value as an LLVM `BasicValueEnum`.
///
/// # Errors
/// - `CodegenError::Unsupported` if the variable is not in scope (should not
///   occur after semantic analysis).
pub fn lower_variable<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    name: &str,
    span: Option<SourceSpan>,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    // Built-in global constants lookup
    if let Some(val) = lookup_constant(name) {
        let float = ctx.codegen.context.f64_type().const_float(val);
        return Ok(float.into());
    }

    // General variable lookup in the current lexical scope.
    ctx.load_var(name, span)
}

/// Lowers an assignment to a variable target `name := value`.
///
/// # Steps
/// 1. Looks up the variable's pointer and semantic type.
/// 2. Lowers the value expression.
/// 3. If the target is a protocol, converts the value to a fat pointer.
/// 4. Loads the old value for release (if heap-allocated).
/// 5. If the target type is heap-allocated, releases the old value and retains the new.
/// 6. Stores the new value.
/// 7. Returns the stored value (the assignment expression's value).
///
/// # Parameters
/// - `ctx`: The lowering context.
/// - `name`: The variable name.
/// - `value`: The expression to store.
/// - `span`: The source span for error reporting.
///
/// # Returns
/// The LLVM value that was stored.
///
/// # Errors
/// - Propagates errors from looking up the variable, lowering the value,
///   or LLVM instruction emission.
pub fn lower_assign_variable<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    name: &str,
    value: &hulk_ast::Expr<Type>,
    span: SourceSpan,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    // 1. Look up the variable's LLVM pointer and semantic type.
    let (ptr, _llvm_ty, target_ty) = ctx.lookup_var(name, Some(span))?;

    // 2. Lower the value to be stored.
    let mut stored_val = lower_expr(ctx, value)?;
    let val_ty = &value.anno;

    // 3. If the target is a protocol and the source is a concrete class, convert.
    if ctx.registry.is_protocol(&target_ty) {
        if let Type::Named(_) = val_ty {
            if !ctx.registry.is_protocol(val_ty) {
                stored_val = convert_to_protocol(ctx, stored_val, val_ty, &target_ty)?;
            }
        }
    }

    // 4. Box primitive if target type is Object.
    stored_val = ensure_boxed(ctx, stored_val, val_ty, &target_ty)?;

    // 5. Load the old value (for release).
    let old_val = ctx
        .codegen
        .builder
        .build_load(
            crate::lower::utils::llvm_type(ctx.codegen, ctx.registry, &target_ty)?,
            ptr,
            "old_var",
        )
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // 6. If the target type is heap-allocated, release the old value and retain the new.
    if is_heap_allocated_type(&target_ty, ctx.registry) {
        let release_fn = ctx
            .codegen
            .functions
            .get("hulk_rt_release")
            .cloned()
            .ok_or_else(|| CodegenError::unsupported("hulk_rt_release not declared", Some(span)))?;
        ctx.codegen
            .builder
            .build_call(release_fn, &[old_val.into()], "release_old_var")
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

        let retain_fn = ctx
            .codegen
            .functions
            .get("hulk_rt_retain")
            .cloned()
            .ok_or_else(|| CodegenError::unsupported("hulk_rt_retain not declared", Some(span)))?;
        ctx.codegen
            .builder
            .build_call(retain_fn, &[stored_val.into()], "retain_new_var")
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    }

    // 7. Store the (possibly converted) value.
    ctx.store_var(name, stored_val, Some(span))?;

    // 8. The assignment expression returns the stored value.
    Ok(stored_val)
}
