//! Lowering of bindings and assignments.
//!
//! This module handles:
//! - Variable references (`Variable`): looking up a variable in the current
//!   lexical scope and loading its value.
//! - `let` expressions: introducing new variables in a fresh scope, with
//!   sequential binding evaluation (later bindings can refer to earlier ones).
//! - Assignment (`Assign`): storing a new value into an existing AssignTarget.
//!
//! The lexical scoping discipline mirrors `hulk_semantic::Environment`:
//! - Each `let` introduces a new scope that encloses its body.
//! - Bindings within a single `let` are processed left‑to‑right, and
//!   subsequent bindings can refer to earlier ones.
//! - A plain `Block` does not introduce a new scope (handled in `control.rs`).

use hulk_ast::{AssignExpr, AssignTarget, LetExpr, ExprKind};
use hulk_semantic::Type;
use inkwell::values::BasicValueEnum;

use super::lower_expr;
use crate::error::CodegenError;
use crate::lower::utils::{convert_to_protocol, is_protocol_or_iterable, resolve_type_ref_to_type};
use crate::lower::LowerCtx;
use crate::lower::{index, member, variable};
use crate::lower::utils::{ensure_boxed, is_heap_allocated_type};

/// Lowers a `let` expression with one or more bindings.
///
/// This function:
/// 1. Pushes a new lexical scope.
/// 2. Processes each binding in order:
///    - Lowers the initialiser expression (which may refer to previously
///      declared bindings in the same `let`).
///    - Allocates a stack slot (`alloca`) for the variable and stores the
///      initial value.
///    - Records the binding in the current scope.
/// 3. Lowers the body expression in the same scope.
/// 4. Pops the scope.
/// 5. Returns the body's value as the result of the `let` expression.
///
/// # Parameters
/// - `ctx`: the lowering context.
/// - `let_expr`: the `let` AST node.
///
/// # Returns
/// The value of the body expression.
///
/// # Errors
/// - Propagates errors from lowering the initialisers or body.
/// - Propagates errors from variable declaration (e.g., LLVM allocation
///   failures).
pub fn lower_let<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    let_expr: &LetExpr<Type>,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    ctx.push_scope();

    for binding in &let_expr.bindings {
        // 1. Lower the initializer.
        let mut init_val = lower_expr(ctx, &binding.initializer)?;
        let init_ty = &binding.initializer.anno;

        // 2. Determine the declared type from the annotation, or fallback to initializer type.
        let declared_ty = if let Some(ann) = &binding.type_annotation {
            resolve_type_ref_to_type(ann, ctx.registry)
        } else {
            init_ty.clone()
        };
        // 3. If the declared type is a protocol and the initializer is concrete, convert.
        if is_protocol_or_iterable(&declared_ty, ctx.registry) {
            if let Type::Named(_) = init_ty {
                if !is_protocol_or_iterable(init_ty, ctx.registry) {
                    init_val = convert_to_protocol(ctx, init_val, init_ty, &declared_ty)?;
                }
            }
        }

        // 4. Box primitive if target type is Object.
        // ensure_boxed is idempotent – it only boxes when necessary.
        init_val = ensure_boxed(ctx, init_val, init_ty, &declared_ty)?;

        // 5. If the initializer is a variable or member access, retain the value
        // because it's a borrowed reference, not a newly created one.
        if is_heap_allocated_type(&declared_ty, ctx.registry) {
            if matches!(&binding.initializer.kind, ExprKind::Variable(_) | ExprKind::Member(_)) {
                let retain_fn = ctx.codegen.functions.get("hulk_rt_retain")
                    .cloned()
                    .ok_or_else(|| CodegenError::unsupported("hulk_rt_retain not declared", Some(binding.initializer.span)))?;
                ctx.codegen.builder
                    .build_call(retain_fn, &[init_val.into()], "retain_let")
                    .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            }
        }

        // 6. Declare the variable with the resolved semantic type.
        ctx.declare_var(&binding.name, init_val, declared_ty, false)?;
    }

    let body_val = lower_expr(ctx, &let_expr.body)?;
    ctx.pop_scope()?;
    Ok(body_val)
}

/// Lowers an assignment expression.
///
/// In Phase 3, only assignments to a simple variable target are supported.
/// For such a target:
/// 1. The right‑hand side is lowered to a value.
/// 2. The target variable's stack slot is looked up.
/// 3. The value is stored into that slot.
/// 4. The assignment expression itself returns the stored value (the same
///    convention used in C and many other languages).
///
/// # Parameters
/// - `ctx`: the lowering context.
/// - `assign`: the assignment AST node.
///
/// # Returns
/// The assigned value (the right‑hand side's value).
///
/// # Errors
/// - `CodegenError::Unsupported` if the target is not a variable (deferred
///   to later phases).
/// - Propagates errors from lowering the right‑hand side or looking up the
///   target variable.
pub fn lower_assign<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    assign: &AssignExpr<Type>,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    match &assign.target {
        AssignTarget::Variable(name) => {
            variable::lower_assign_variable(ctx, name, &assign.value, assign.value.span)
        }
        AssignTarget::Member { object, field } => {
            member::lower_member_assign(ctx, object, field, &assign.value, assign.value.span)
        }
        AssignTarget::Index { object, index } => {
            index::lower_index_assign(ctx, object, index, &assign.value, assign.value.span)
        }
    }
}
