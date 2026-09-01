//! Lowering of `for` loops and vector comprehensions.

use inkwell::values::BasicValueEnum;

use hulk_ast::ForExpr;
use hulk_semantic::Type;

use super::lower_expr;
use crate::error::CodegenError;
use crate::lower::call::lower_method_call_val;
use crate::lower::utils::ensure_unboxed;
use crate::lower::LowerCtx;

/// Lowers a `for` loop.
///
/// The loop iterates over the iterable expression, repeatedly calling `next()`
/// to test continuation and `current()` to obtain the element. The element is
/// bound to the loop variable in a fresh scope for each iteration.
///
/// The value of the `for` expression is the value of the last executed body,
/// or a default value (zero/null) if the body never executes.
pub fn lower_for<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    for_expr: &ForExpr<Type>,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    let iterable_expr = &for_expr.iterable;
    let body_expr = &for_expr.body;

    // Determine the element type from the iterable's `current()` method.
    let elem_ty = ctx
        .registry
        .lookup_method(&iterable_expr.anno, "current")
        .map(|sig| sig.return_type)
        .ok_or_else(|| {
            CodegenError::unsupported(
                format!(
                    "iterable type `{}` does not support `current()`",
                    iterable_expr.anno
                ),
                Some(iterable_expr.span),
            )
        })?;
    let elem_llvm_ty = crate::lower::utils::llvm_type(ctx.codegen, ctx.registry, &elem_ty)?;

    // Compute the LLVM type of the iterable itself (needed for the persistent alloca).
    let iter_ty = iterable_expr.anno.clone();
    let iter_llvm_ty = crate::lower::utils::llvm_type(ctx.codegen, ctx.registry, &iter_ty)?;

    // Result storage: the loop's value is the last body value, or a default.
    let result_ty = crate::lower::utils::llvm_type(ctx.codegen, ctx.registry, &body_expr.anno)?;
    let result_alloca = ctx
        .codegen
        .builder
        .build_alloca(result_ty, "for_result")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // Default value (zero/null) when body never executes.
    let default_val: BasicValueEnum<'ctx> = match &body_expr.anno {
        Type::Number => {
            let val = ctx.codegen.context.f64_type().const_float(0.0);
            BasicValueEnum::FloatValue(val)
        }
        Type::Boolean => {
            let val = ctx.codegen.context.bool_type().const_int(0, false);
            BasicValueEnum::IntValue(val)
        }
        Type::String | Type::Object | Type::Vector(_) => {
            let ptr_type = ctx.codegen.context.ptr_type(Default::default());
            let val = ptr_type.const_null();
            BasicValueEnum::PointerValue(val)
        }
        Type::Named(_) => {
            let ptr_type = ctx.codegen.context.ptr_type(Default::default());
            let val = ptr_type.const_null();
            BasicValueEnum::PointerValue(val)
        }
        Type::Iterable(_) => {
            // Fat pointer: { ptr, ptr } – null both.
            let ptr_type = ctx.codegen.context.ptr_type(Default::default());
            let null_ptr = ptr_type.const_null();
            let null_struct = ctx
                .codegen
                .context
                .const_struct(&[null_ptr.into(), null_ptr.into()], false);
            BasicValueEnum::StructValue(null_struct)
        }
        _ => {
            return Err(CodegenError::unsupported(
                format!(
                    "default value for type `{}` not implemented",
                    body_expr.anno
                ),
                Some(body_expr.span),
            ));
        }
    };
    ctx.codegen
        .builder
        .build_store(result_alloca, default_val)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // WHY: Evaluate the iterable expression ONCE here, before entering the loop.
    // Previously, lower_method_call re-evaluated the receiver expression on every
    // next()/current() call.  For `range(0,5)` that creates a fresh HulkRange
    // each iteration, so next() always returns true → infinite loop.  Store the
    // iterable in a stack slot and load it in both the condition and body blocks.
    let iter_val = lower_expr(ctx, iterable_expr)?;
    let iter_alloca = ctx
        .codegen
        .builder
        .build_alloca(iter_llvm_ty, "iter_obj")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    ctx.codegen
        .builder
        .build_store(iter_alloca, iter_val)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // Basic blocks.
    let parent_fn = ctx
        .codegen
        .builder
        .get_insert_block()
        .unwrap()
        .get_parent()
        .unwrap();
    let cond_bb = ctx
        .codegen
        .context
        .append_basic_block(parent_fn, "for_cond");
    let body_bb = ctx
        .codegen
        .context
        .append_basic_block(parent_fn, "for_body");
    let exit_bb = ctx
        .codegen
        .context
        .append_basic_block(parent_fn, "for_exit");

    // Jump to condition.
    ctx.codegen
        .builder
        .build_unconditional_branch(cond_bb)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // ── Condition block ─────────────────────────────────────────────────
    ctx.codegen.builder.position_at_end(cond_bb);

    // Load the persistent iterable and call next().
    let iter_for_next = ctx
        .codegen
        .builder
        .build_load(iter_llvm_ty, iter_alloca, "iter_next_load")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    let next_val = lower_method_call_val(ctx, iter_for_next, &iter_ty, "next", iterable_expr.span)?;
    let next_bool = next_val.into_int_value();

    ctx.codegen
        .builder
        .build_conditional_branch(next_bool, body_bb, exit_bb)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // ── Body block ──────────────────────────────────────────────────────
    ctx.codegen.builder.position_at_end(body_bb);

    // Load the persistent iterable and call current().
    let iter_for_current = ctx
        .codegen
        .builder
        .build_load(iter_llvm_ty, iter_alloca, "iter_current_load")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    let current_val = lower_method_call_val(
        ctx,
        iter_for_current,
        &iter_ty,
        "current",
        iterable_expr.span,
    )?;

    // Only Vector boxes its elements as HulkBox* (via hulk_rt_vector_current).
    let unboxed_current = if matches!(&iter_ty, Type::Vector(_)) {
        ensure_unboxed(ctx, current_val, &elem_ty, Some(iterable_expr.span))?
    }
    // Range and all user-defined generator types return the element value directly
    else {
        current_val
    };

    // Bind the loop variable.
    ctx.push_scope();
    let var_ptr = ctx
        .codegen
        .builder
        .build_alloca(elem_llvm_ty, &for_expr.var)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    ctx.codegen
        .builder
        .build_store(var_ptr, unboxed_current)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    ctx.scope_stack
        .declare(&for_expr.var, var_ptr, elem_llvm_ty, elem_ty.clone(), false, None);

    // Lower the body.
    let body_val = lower_expr(ctx, body_expr)?;

    // Store the body's value as the loop result.
    ctx.codegen
        .builder
        .build_store(result_alloca, body_val)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    ctx.pop_scope()?;

    // Jump back to condition.
    ctx.codegen
        .builder
        .build_unconditional_branch(cond_bb)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // ── Exit block ──────────────────────────────────────────────────────
    ctx.codegen.builder.position_at_end(exit_bb);
    let result = ctx
        .codegen
        .builder
        .build_load(result_ty, result_alloca, "for_result_load")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    Ok(result)
}
