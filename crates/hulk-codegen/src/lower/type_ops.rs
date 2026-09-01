//! Lowering of type tests (`is`) and downcasts (`as`).

use hulk_ast::{DowncastExpr, SourceSpan, TypeTestExpr};
use hulk_semantic::Type;

use super::lower_expr;
use crate::error::CodegenError;
use crate::lower::LowerCtx;
use crate::runtime_decls::ensure_decl;

/// Lowers a type test expression (`expr is Type`).
///
/// Calls `hulk_rt_downcast_check(obj, target_vtable)` and returns the boolean result.
pub fn lower_typetest<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    type_test: &TypeTestExpr<Type>,
) -> Result<inkwell::values::BasicValueEnum<'ctx>, CodegenError> {
    let obj_ptr = lower_object_pointer(ctx, &type_test.expr)?;
    let target_vtable = resolve_vtable(ctx, &type_test.type_name.name, Some(type_test.expr.span))?;

    let check_fn = ensure_decl(ctx.codegen, "hulk_rt_downcast_check")?;
    let args: Vec<inkwell::values::BasicMetadataValueEnum> =
        vec![obj_ptr.into(), target_vtable.into()];
    let call_site = ctx
        .codegen
        .builder
        .build_call(check_fn, &args, "downcast_check")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    let result = call_site.try_as_basic_value().unwrap_basic();
    Ok(result)
}

/// Lowers a downcast expression (`expr as Type`).
///
/// If the downcast succeeds, returns the object pointer cast to the target type pointer.
/// If it fails, calls `hulk_rt_downcast_fail()` (which does not return).
pub fn lower_downcast<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    downcast: &DowncastExpr<Type>,
) -> Result<inkwell::values::BasicValueEnum<'ctx>, CodegenError> {
    let obj_ptr = lower_object_pointer(ctx, &downcast.expr)?;
    let target_vtable = resolve_vtable(ctx, &downcast.type_name.name, Some(downcast.expr.span))?;

    let check_fn = ensure_decl(ctx.codegen, "hulk_rt_downcast_check")?;
    let args: Vec<inkwell::values::BasicMetadataValueEnum> =
        vec![obj_ptr.into(), target_vtable.into()];
    let call_site = ctx
        .codegen
        .builder
        .build_call(check_fn, &args, "downcast_check")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    let is_ok = call_site.try_as_basic_value().unwrap_basic();

    // Branch: if ok, continue; else trap.
    let current_block = ctx.codegen.builder.get_insert_block().unwrap();
    let parent_fn = current_block.get_parent().unwrap();
    let ok_bb = ctx
        .codegen
        .context
        .append_basic_block(parent_fn, "downcast_ok");
    let trap_bb = ctx
        .codegen
        .context
        .append_basic_block(parent_fn, "downcast_trap");
    let merge_bb = ctx
        .codegen
        .context
        .append_basic_block(parent_fn, "downcast_merge");

    let cond_int = is_ok.into_int_value();
    ctx.codegen
        .builder
        .build_conditional_branch(cond_int, ok_bb, trap_bb)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // ─── Trap block ──────────────────────────────────────────────────────

    ctx.codegen.builder.position_at_end(trap_bb);
    let fail_fn = ensure_decl(ctx.codegen, "hulk_rt_downcast_fail")?;
    ctx.codegen
        .builder
        .build_call(fail_fn, &[], "downcast_fail")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    // Mark the block as unreachable (the function never returns).
    ctx.codegen
        .builder
        .build_unreachable()
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // ─── OK block ────────────────────────────────────────────────────────

    ctx.codegen.builder.position_at_end(ok_bb);
    // Cast the object pointer to the target type pointer.
    let target_ptr_type = ctx.codegen.context.ptr_type(Default::default());
    let cast_ptr = ctx
        .codegen
        .builder
        .build_pointer_cast(obj_ptr, target_ptr_type, "downcast_result")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    ctx.codegen
        .builder
        .build_unconditional_branch(merge_bb)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // ─── Merge block ─────────────────────────────────────────────────────

    ctx.codegen.builder.position_at_end(merge_bb);
    // Since only one path (ok_bb) contributes a value, we can just use the cast_ptr.
    Ok(cast_ptr.into())
}

/// Lowers the object expression and returns a pointer to it.
fn lower_object_pointer<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    expr: &hulk_ast::Expr<Type>,
) -> Result<inkwell::values::PointerValue<'ctx>, CodegenError> {
    let obj_val = lower_expr(ctx, expr)?;
    Ok(obj_val.into_pointer_value())
}

/// Resolves the vtable global for a given type name.
fn resolve_vtable<'ctx>(
    ctx: &LowerCtx<'_, 'ctx>,
    type_name: &str,
    span: Option<SourceSpan>,
) -> Result<inkwell::values::PointerValue<'ctx>, CodegenError> {
    let layout = ctx.codegen.type_layouts.get(type_name).ok_or_else(|| {
        CodegenError::unsupported(format!("no layout for type '{}'", type_name), span)
    })?;
    let vtable_global = layout.vtable_global.ok_or_else(|| {
        CodegenError::unsupported(format!("vtable for '{}' not built", type_name), span)
    })?;
    Ok(vtable_global.as_pointer_value())
}
