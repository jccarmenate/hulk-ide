//! Lowering of control flow: `Block`, `If`/`elif`/`else`, and `While`.
//!
//! This module handles the three main control‑flow constructs of HULK:
//! - `Block`: a sequence of expressions evaluated in order; the result is
//!   the value of the last expression (or `false` for an empty block).
//! - `If`/`elif`/`else`: conditional branching with multiple `elif` arms
//!   and a mandatory `else` branch. The result is stored in an `alloca`
//!   and loaded at the merge block, which the `mem2reg` pass will later
//!   promote to a `phi` node.
//! - `While`: a loop that executes its body while the condition is true.
//!   The result is the value of the last executed body iteration (or a
//!   default value if the loop never runs), again stored in an `alloca`.
//!
//! The use of `alloca` for result storage is a deliberate front‑end
//! simplification. It avoids the need to manually create `phi` nodes for
//! each branch, and LLVM's `mem2reg` pass will clean it up later.

use inkwell::values::BasicValueEnum;

use hulk_ast::{BlockExpr, IfExpr, WhileExpr};
use hulk_semantic::Type;

use super::lower_expr;
use crate::error::CodegenError;
use crate::lower::{utils, LowerCtx};

/// Lowers a block expression (sequence of expressions).
///
/// Each expression is lowered in order. The value of the block is the value
/// of the last expression; if the block is empty, it returns a default `false`.
pub fn lower_block<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    block: &BlockExpr<Type>,
) -> Result<inkwell::values::BasicValueEnum<'ctx>, CodegenError> {
    let mut last_val = ctx.codegen.context.bool_type().const_int(0, false).into();
    for (i, e) in block.expressions.iter().enumerate() {
        let val = lower_expr(ctx, e)?;
        if i == block.expressions.len() - 1 {
            last_val = val;
        }
        // else: discard value (only side effects matter)
    }
    Ok(last_val)
}

/// Lowers an `if`/`elif`/`else` expression.
///
/// # Structure
/// - Condition block branches to the first `then` block or to the first `elif`
///   (or final `else` if no `elif`s).
/// - Each `elif` is lowered as a nested conditional inside the previous `else`
///   block.
/// - The final `else` block stores its value and jumps to the merge block.
/// - The merge block loads the result and returns it.
pub fn lower_if<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    if_expr: &IfExpr<Type>,
    result_type: &Type,
) -> Result<inkwell::values::BasicValueEnum<'ctx>, CodegenError> {
    let result_ty = utils::llvm_type(ctx.codegen, ctx.registry, result_type)?;
    let result_alloca = ctx
        .codegen
        .builder
        .build_alloca(result_ty, "if_result")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // Current function to append basic blocks to.
    let parent_fn = ctx
        .codegen
        .builder
        .get_insert_block()
        .unwrap()
        .get_parent()
        .unwrap();

    // Condition block for the main if.
    let cond_val = lower_expr(ctx, &if_expr.condition)?;
    let cond_int = cond_val.into_int_value();

    let then_bb = ctx.codegen.context.append_basic_block(parent_fn, "if_then");
    let else_bb = ctx.codegen.context.append_basic_block(parent_fn, "if_else");
    let merge_bb = ctx
        .codegen
        .context
        .append_basic_block(parent_fn, "if_merge");

    // Branch on condition.
    ctx.codegen
        .builder
        .build_conditional_branch(cond_int, then_bb, else_bb)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // ─── Then branch ──────────────────────────────────────────────────────

    ctx.codegen.builder.position_at_end(then_bb);
    let then_val = lower_expr(ctx, &if_expr.then_branch)?;
    let then_boxed = utils::ensure_boxed(ctx, then_val, &if_expr.then_branch.anno, result_type)?;
    ctx.codegen
        .builder
        .build_store(result_alloca, then_boxed)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    ctx.codegen
        .builder
        .build_unconditional_branch(merge_bb)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // ─── Elif chain ──────────────────────────────────────────────────────

    let mut current_else_bb = else_bb;
    for (idx, elif) in if_expr.elif_branches.iter().enumerate() {
        ctx.codegen.builder.position_at_end(current_else_bb);

        let elif_cond = lower_expr(ctx, &elif.condition)?;
        let elif_cond_int = elif_cond.into_int_value();

        let elif_then_bb = ctx
            .codegen
            .context
            .append_basic_block(parent_fn, &format!("elif_then_{}", idx));
        let elif_next_bb = ctx
            .codegen
            .context
            .append_basic_block(parent_fn, &format!("elif_else_{}", idx));

        ctx.codegen
            .builder
            .build_conditional_branch(elif_cond_int, elif_then_bb, elif_next_bb)
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

        // Elif body.
        ctx.codegen.builder.position_at_end(elif_then_bb);
        let elif_val = lower_expr(ctx, &elif.body)?;
        let elif_boxed = utils::ensure_boxed(ctx, elif_val, &elif.body.anno, result_type)?;
        ctx.codegen
            .builder
            .build_store(result_alloca, elif_boxed)
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
        ctx.codegen
            .builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

        current_else_bb = elif_next_bb;
    }

    // ─── Final else ─────────────────────────────────────────────────────

    ctx.codegen.builder.position_at_end(current_else_bb);
    let else_val = lower_expr(ctx, &if_expr.else_branch)?;
    let else_boxed = utils::ensure_boxed(ctx, else_val, &if_expr.else_branch.anno, result_type)?;
    ctx.codegen
        .builder
        .build_store(result_alloca, else_boxed)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    ctx.codegen
        .builder
        .build_unconditional_branch(merge_bb)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // ─── Merge ──────────────────────────────────────────────────────────

    ctx.codegen.builder.position_at_end(merge_bb);
    let result = ctx
        .codegen
        .builder
        .build_load(result_ty, result_alloca, "if_result_load")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    Ok(result)
}

/// Lowers a `while` loop.
///
/// The loop body is executed as long as the condition evaluates to `true`.
/// The result of the loop is the value of the last executed body expression
/// (or a default value if the body is never executed).
///
/// # Structure
/// - An initial unconditional branch goes to the condition block.
/// - The condition block evaluates the condition and branches to the body
///   or exit.
/// - The body block executes, stores its value in the result alloca, and
///   jumps back to the condition block.
/// - The exit block loads the result and returns it.
pub fn lower_while<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    while_expr: &WhileExpr<Type>,
    result_type: &Type,
) -> Result<inkwell::values::BasicValueEnum<'ctx>, CodegenError> {
    let result_ty = utils::llvm_type(ctx.codegen, ctx.registry, result_type)?;
    let result_alloca = ctx
        .codegen
        .builder
        .build_alloca(result_ty, "while_result")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // Store a default value in case the body is never executed.
    let default_val: BasicValueEnum<'ctx> = match result_type {
        Type::Number => ctx.codegen.context.f64_type().const_float(0.0).into(),
        Type::Boolean => ctx.codegen.context.bool_type().const_int(0, false).into(),
        Type::String | Type::Object | Type::Named(_) | Type::Vector(_) => {
            ctx.codegen.context.ptr_type(Default::default()).const_null().into()
        }
        Type::Iterable(_) | Type::Function { .. } => {
            let ptr = ctx.codegen.context.ptr_type(Default::default()).const_null();
            ctx.codegen.context.const_struct(&[ptr.into(), ptr.into()], false).into()
        }
        _ => {
            return Err(CodegenError::unsupported(
                format!("while loop result type {} not supported", result_type),
                Some(while_expr.body.span),
            ))
        }
    };

    // Push result_alloca as a GC root
    let result_shadow_slot = if utils::is_heap_allocated_type(result_type, ctx.registry) {
        let push_fn = ctx.codegen.functions.get("hulk_rt_shadow_push")
            .cloned()
            .ok_or_else(|| CodegenError::unsupported("hulk_rt_shadow_push not declared", None))?;
        ctx.codegen.builder
            .build_call(push_fn, &[result_alloca.into()], "while_result_shadow_push")
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
        true
    } else {
        false
    };

    ctx.codegen
        .builder
        .build_store(result_alloca, default_val)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

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
        .append_basic_block(parent_fn, "while_cond");
    let body_bb = ctx
        .codegen
        .context
        .append_basic_block(parent_fn, "while_body");
    let exit_bb = ctx
        .codegen
        .context
        .append_basic_block(parent_fn, "while_exit");

    // Jump to condition.
    ctx.codegen
        .builder
        .build_unconditional_branch(cond_bb)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // ─── Condition block ────────────────────────────────────────────────

    ctx.codegen.builder.position_at_end(cond_bb);
    let cond_val = lower_expr(ctx, &while_expr.condition)?;
    let cond_int = cond_val.into_int_value();
    ctx.codegen
        .builder
        .build_conditional_branch(cond_int, body_bb, exit_bb)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // ─── Body block ─────────────────────────────────────────────────────

    ctx.codegen.builder.position_at_end(body_bb);
    let body_val = lower_expr(ctx, &while_expr.body)?;
    let body_boxed = utils::ensure_boxed(ctx, body_val, &while_expr.body.anno, result_type)?;
    ctx.codegen
        .builder
        .build_store(result_alloca, body_boxed)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    ctx.codegen
        .builder
        .build_unconditional_branch(cond_bb)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // ─── Exit block ─────────────────────────────────────────────────────

    ctx.codegen.builder.position_at_end(exit_bb);
    if result_shadow_slot {
        let pop_fn = ctx.codegen.functions.get("hulk_rt_shadow_pop").cloned().unwrap();
        ctx.codegen.builder
            .build_call(pop_fn, &[], "while_result_shadow_pop")
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    }
    let result = ctx.codegen.builder
        .build_load(result_ty, result_alloca, "while_result_load")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    Ok(result)
}
