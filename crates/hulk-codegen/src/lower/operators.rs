//! Lowering of unary and binary operators to LLVM IR.
//!
//! This module handles all HULK operators:
//! - Unary: `-` (negate) and `!` (logical not)
//! - Binary: arithmetic (`+`, `-`, `*`, `/`, `%`, `^`),
//!   comparisons (`==`, `!=`, `<`, `<=`, `>`, `>=`),
//!   logical (`&`, `|`), and concatenation (`@`, `@@`).
//!
//! All numeric operations are performed on 64‑bit floating‑point values
//! (`f64`). Booleans are represented as `i1` (1‑bit integers). Strings are
//! pointers to a `HulkString` struct.
//!
//! Concatenation automatically stringifies `Number` and `Boolean` operands
//! via calls to the runtime library (`hulk_rt_number_to_string` and
//! `hulk_rt_bool_to_string`). The `@@` operator inserts a literal space
//! between the two operands.

use hulk_ast::{BinaryExpr, BinaryOp, UnaryExpr, UnaryOp};
use hulk_semantic::Type;
use inkwell::FloatPredicate;
use inkwell::IntPredicate;

use super::lower_expr;
use crate::error::CodegenError;
use crate::lower::utils::{cmp_float, to_string};
use crate::lower::LowerCtx;
use crate::runtime_decls::{self, declare_string_concat, declare_string_concat_space};

/// Lowers a unary expression.
///
/// # Supported operators
/// - `Negate` (`-`): negates a floating‑point number.
/// - `Not` (`!`): logical negation of a boolean.
///
/// # Errors
/// - If the operand is not of the expected type (float for negate, bool for not).
/// - If LLVM instruction emission fails.
pub fn lower_unary<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    unary: &UnaryExpr<Type>,
) -> Result<inkwell::values::BasicValueEnum<'ctx>, CodegenError> {
    let operand = lower_expr(ctx, &unary.expr)?;

    match unary.op {
        UnaryOp::Negate => {
            let float_val = operand.into_float_value();
            let neg = ctx
                .codegen
                .builder
                .build_float_neg(float_val, "neg")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            Ok(neg.into())
        }
        UnaryOp::Not => {
            let int_val = operand.into_int_value();
            let not = ctx
                .codegen
                .builder
                .build_not(int_val, "not")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            Ok(not.into())
        }
    }
}

/// Lowers a binary expression.
///
/// # Supported operators
/// - **Arithmetic**: `Add`, `Subtract`, `Multiply`, `Divide`, `Modulo`, `Power`
///   – all operate on `f64` values.
/// - **Comparisons**: `Equal`, `NotEqual`, `Less`, `LessEqual`, `Greater`,
///   `GreaterEqual` – compare two `f64` values and return `i1` (boolean).
/// - **Logical**: `And`, `Or` – operate on `i1` booleans.
/// - **Concatenation**: `Concat` (`@`) and `ConcatSpace` (`@@`) – stringify
///   operands and concatenate them (with or without a space).
///
/// # Parameters
/// - `binary`: the binary expression node.
/// - `result_type`: the static type of the whole expression (used for
///   future boxing, but currently ignored in Phase 3).
///
/// # Errors
/// - If an operand is not of the expected type for the operator.
/// - If an LLVM intrinsic or runtime function call fails.
pub fn lower_binary<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    binary: &BinaryExpr<Type>,
    _result_type: &Type, // Reserved for future boxing logic.
) -> Result<inkwell::values::BasicValueEnum<'ctx>, CodegenError> {
    let left = lower_expr(ctx, &binary.left)?;
    let right = lower_expr(ctx, &binary.right)?;

    let result = match binary.op {
        // ─── Arithmetic operators ────────────────────────────────────────
        BinaryOp::Add => {
            let lf = left.into_float_value();
            let rf = right.into_float_value();
            ctx.codegen
                .builder
                .build_float_add(lf, rf, "add")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
                .into()
        }
        BinaryOp::Subtract => {
            let lf = left.into_float_value();
            let rf = right.into_float_value();
            ctx.codegen
                .builder
                .build_float_sub(lf, rf, "sub")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
                .into()
        }
        BinaryOp::Multiply => {
            let lf = left.into_float_value();
            let rf = right.into_float_value();
            ctx.codegen
                .builder
                .build_float_mul(lf, rf, "mul")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
                .into()
        }
        BinaryOp::Divide => {
            let lf = left.into_float_value();
            let rf = right.into_float_value();
            ctx.codegen
                .builder
                .build_float_div(lf, rf, "div")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
                .into()
        }
        BinaryOp::Modulo => {
            let lf = left.into_float_value();
            let rf = right.into_float_value();
            ctx.codegen
                .builder
                .build_float_rem(lf, rf, "rem")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
                .into()
        }
        BinaryOp::Power => {
            // Call `llvm.pow.f64` intrinsic.
            let lf = left.into_float_value();
            let rf = right.into_float_value();
            let pow_fn = ctx
                .codegen
                .module
                .get_function("llvm.pow.f64")
                .unwrap_or_else(|| {
                    let f64_type = ctx.codegen.context.f64_type();
                    let fn_type = f64_type.fn_type(&[f64_type.into(), f64_type.into()], false);
                    ctx.codegen
                        .module
                        .add_function("llvm.pow.f64", fn_type, None)
                });
            let call = ctx
                .codegen
                .builder
                .build_call(pow_fn, &[lf.into(), rf.into()], "pow")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
                .try_as_basic_value()
                .unwrap_basic();
            call
        }

        // ─── Comparison operators ──────────────────────────────────────
        BinaryOp::Equal => lower_equality(ctx, true, left, right, &binary.left.anno)?,
        BinaryOp::NotEqual => lower_equality(ctx, false, left, right, &binary.left.anno)?,
        BinaryOp::Less => cmp_float(ctx, FloatPredicate::OLT, left, right, "lt")?,
        BinaryOp::LessEqual => cmp_float(ctx, FloatPredicate::OLE, left, right, "le")?,
        BinaryOp::Greater => cmp_float(ctx, FloatPredicate::OGT, left, right, "gt")?,
        BinaryOp::GreaterEqual => cmp_float(ctx, FloatPredicate::OGE, left, right, "ge")?,

        // ─── Logical operators ─────────────────────────────────────────
        BinaryOp::And => {
            let li = left.into_int_value();
            let ri = right.into_int_value();
            ctx.codegen
                .builder
                .build_and(li, ri, "and")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
                .into()
        }
        BinaryOp::Or => {
            let li = left.into_int_value();
            let ri = right.into_int_value();
            ctx.codegen
                .builder
                .build_or(li, ri, "or")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
                .into()
        }

        // ─── Concatenation operators ──────────────────────────────────
        BinaryOp::Concat | BinaryOp::ConcatSpace => {
            // Auto‑stringify operands.
            let left_str = to_string(ctx, left, &binary.left.anno)
                .map_err(|e| e.with_span(binary.left.span))?;
            let right_str = to_string(ctx, right, &binary.right.anno)
                .map_err(|e| e.with_span(binary.right.span))?;
            let concat_fn = if matches!(binary.op, BinaryOp::Concat) {
                declare_string_concat(ctx.codegen)
            } else {
                declare_string_concat_space(ctx.codegen)
            };
            let call = ctx
                .codegen
                .builder
                .build_call(concat_fn, &[left_str.into(), right_str.into()], "concat")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
                .try_as_basic_value()
                .unwrap_basic();
            call
        }
    };

    Ok(result)
}

/// Lowers `==` and `!=` with type-dispatched LLVM instructions.
///
/// WHY: rustc codegen dispatch pattern — the static type determines
/// which LLVM instruction to emit. Never call into_float_value()
/// without verifying the operand is Number (f64):
/// - Number  → build_float_compare (OEQ/ONE)
/// - Boolean → build_int_compare   (EQ/NE on i1)
/// - String  → hulk_rt_string_equals(ptr, ptr) -> i1, negated for !=
/// - _       → reference equality: ptr-to-int then build_int_compare
fn lower_equality<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    is_eq: bool,
    left: inkwell::values::BasicValueEnum<'ctx>,
    right: inkwell::values::BasicValueEnum<'ctx>,
    left_type: &Type,
) -> Result<inkwell::values::BasicValueEnum<'ctx>, CodegenError> {
    match left_type {
        Type::Number => {
            let pred = if is_eq {
                FloatPredicate::OEQ
            } else {
                FloatPredicate::ONE
            };
            cmp_float(ctx, pred, left, right, if is_eq { "eq" } else { "ne" })
        }
        Type::Boolean => {
            let pred = if is_eq {
                IntPredicate::EQ
            } else {
                IntPredicate::NE
            };
            Ok(ctx
                .codegen
                .builder
                .build_int_compare(
                    pred,
                    left.into_int_value(),
                    right.into_int_value(),
                    "bool_eq",
                )
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
                .into())
        }
        Type::String => {
            let str_eq_fn = runtime_decls::declare_string_equals(ctx.codegen);
            let result = ctx
                .codegen
                .builder
                .build_call(str_eq_fn, &[left.into(), right.into()], "str_eq")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
                .try_as_basic_value()
                .unwrap_basic();
            if is_eq {
                Ok(result)
            } else {
                Ok(ctx
                    .codegen
                    .builder
                    .build_not(result.into_int_value(), "str_ne")
                    .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
                    .into())
            }
        }
        _ => {
            // WHY: Named/Object/protocol fat pointers — reference equality
            let i64_ty = ctx.codegen.context.i64_type();
            let li = ctx
                .codegen
                .builder
                .build_ptr_to_int(left.into_pointer_value(), i64_ty, "lp_int")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            let ri = ctx
                .codegen
                .builder
                .build_ptr_to_int(right.into_pointer_value(), i64_ty, "rp_int")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            let pred = if is_eq {
                IntPredicate::EQ
            } else {
                IntPredicate::NE
            };
            Ok(ctx
                .codegen
                .builder
                .build_int_compare(pred, li, ri, "ptr_eq")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
                .into())
        }
    }
}
