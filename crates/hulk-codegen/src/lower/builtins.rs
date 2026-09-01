//! Built-in constants and functions for HULK.
//!
//! This module handles:
//! - Constants: `PI`, `E` – returned as `f64` literals.
//! - Built-in functions: `print`, `sqrt`, `sin`, `cos`, `exp`, `log`, `rand`, `range`.
//!
//! For math functions, LLVM intrinsics are used when available; otherwise, a
//! fallback to `hulk-rt` is provided.

use hulk_ast::CallExpr;
use hulk_semantic::Type;
use inkwell::values::BasicValueEnum;

use super::lower_expr;
use crate::error::CodegenError;
use crate::lower::utils::ensure_boxed;
use crate::lower::LowerCtx;

// ─── Constants ──────────────────────────────────────────────────────────

/// Lookup table for built-in constants.
pub fn lookup_constant(name: &str) -> Option<f64> {
    match name {
        "PI" => Some(std::f64::consts::PI),
        "E" => Some(std::f64::consts::E),
        _ => None,
    }
}

// ─── Built-in function dispatcher ──────────────────────────────────────

/// Attempts to lower a call to a built-in function.
/// Returns `Some(Ok(value))` if the callee is a builtin, `None` otherwise.
pub fn lower_builtin_call<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    call: &CallExpr<Type>,
) -> Option<Result<BasicValueEnum<'ctx>, CodegenError>> {
    let callee_name = match &call.callee.kind {
        hulk_ast::ExprKind::Variable(name) => name,
        _ => return None,
    };

    match callee_name.as_str() {
        "print" => Some(lower_print(ctx, call)),
        "sqrt" => Some(lower_math_unary(ctx, "sqrt", call, "llvm.sqrt.f64")),
        "sin" => Some(lower_math_unary(ctx, "sin", call, "llvm.sin.f64")),
        "cos" => Some(lower_math_unary(ctx, "cos", call, "llvm.cos.f64")),
        "exp" => Some(lower_math_unary(ctx, "exp", call, "llvm.exp.f64")),
        "log" => Some(lower_log(ctx, call)),
        "rand" => Some(lower_rand(ctx, call)),
        "range" => Some(lower_range(ctx, call)),
        _ => None,
    }
}

// ─── Individual builtin handlers ──────────────────────────────────────

/// Lowers a `print(arg)` call.
///
/// Boxes the argument to `Object`, then calls `hulk_rt_print`. Returns the
/// boxed value (the argument is returned as per the spec).
///
/// # Errors
/// Returns an error if the argument count is not exactly 1, or if the runtime
/// function is not declared.
fn lower_print<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    call: &CallExpr<Type>,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    if call.args.len() != 1 {
        return Err(CodegenError::unsupported(
            "print requires exactly one argument",
            Some(call.callee.span),
        ));
    }
    let arg = &call.args[0];
    let val = lower_expr(ctx, arg)?;
    let boxed = ensure_boxed(ctx, val, &arg.anno, &Type::Object)?;
    let print_fn = ctx
        .codegen
        .functions
        .get("hulk_rt_print")
        .copied()
        .ok_or_else(|| {
            CodegenError::unsupported("hulk_rt_print not declared", Some(call.callee.span))
        })?;
    let result = ctx
        .codegen
        .builder
        .build_call(print_fn, &[boxed.into()], "print_call")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
        .try_as_basic_value()
        .unwrap_basic();
    Ok(result)
}

/// Lowers a unary math function (`sqrt`, `sin`, `cos`, `exp`).
///
/// Attempts to use an LLVM intrinsic (`llvm.sqrt.f64`, etc.) first; if not
/// available, falls back to the corresponding `hulk_rt_*` function.
///
/// # Parameters
/// - `name`: The function name (used for error messages and runtime fallback).
/// - `call`: The call node.
/// - `intrinsic_name`: The LLVM intrinsic name (e.g., `"llvm.sqrt.f64"`).
///
/// # Errors
/// Returns an error if the argument count is not exactly 1, or if neither
/// the intrinsic nor the runtime function is available.
fn lower_math_unary<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    name: &str,
    call: &CallExpr<Type>,
    intrinsic_name: &str,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    if call.args.len() != 1 {
        return Err(CodegenError::unsupported(
            format!("{} requires exactly one argument", name),
            Some(call.callee.span),
        ));
    }
    let arg = lower_expr(ctx, &call.args[0])?.into_float_value();

    // Prefer LLVM intrinsic
    if let Some(intrin) = ctx.codegen.module.get_function(intrinsic_name) {
        let result = ctx
            .codegen
            .builder
            .build_call(intrin, &[arg.into()], &format!("{}_intrinsic", name))
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
            .try_as_basic_value()
            .unwrap_basic();
        return Ok(result);
    }

    // Fallback to hulk-rt
    let fn_name = format!("hulk_rt_{}", name);
    let rt_fn = ctx
        .codegen
        .functions
        .get(&fn_name)
        .copied()
        .ok_or_else(|| {
            CodegenError::unsupported(format!("{} not declared", fn_name), Some(call.callee.span))
        })?;
    let result = ctx
        .codegen
        .builder
        .build_call(rt_fn, &[arg.into()], &format!("{}_rt", name))
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
        .try_as_basic_value()
        .unwrap_basic();
    Ok(result)
}

/// Lowers a `log(base, x)` call.
///
/// Uses the `llvm.log.f64` intrinsic to compute natural logarithms of `base`
/// and `x`, then divides them. If the intrinsic is unavailable, falls back to
/// `hulk_rt_log`.
///
/// # Errors
/// Returns an error if the argument count is not exactly 2, or if the runtime
/// function is not declared when fallback is needed.
fn lower_log<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    call: &CallExpr<Type>,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    if call.args.len() != 2 {
        return Err(CodegenError::unsupported(
            "log requires exactly two arguments (base, x)",
            Some(call.callee.span),
        ));
    }
    let base = lower_expr(ctx, &call.args[0])?.into_float_value();
    let x = lower_expr(ctx, &call.args[1])?.into_float_value();

    // Use natural log intrinsic if available
    if let Some(ln_fn) = ctx.codegen.module.get_function("llvm.log.f64") {
        let ln_x = ctx
            .codegen
            .builder
            .build_call(ln_fn, &[x.into()], "ln_x")
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
            .try_as_basic_value()
            .unwrap_basic()
            .into_float_value();
        let ln_base = ctx
            .codegen
            .builder
            .build_call(ln_fn, &[base.into()], "ln_base")
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
            .try_as_basic_value()
            .unwrap_basic()
            .into_float_value();
        let result = ctx
            .codegen
            .builder
            .build_float_div(ln_x, ln_base, "log_result")
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
        Ok(result.into())
    } else {
        // Fallback to runtime
        let rt_fn = ctx
            .codegen
            .functions
            .get("hulk_rt_log")
            .copied()
            .ok_or_else(|| {
                CodegenError::unsupported("hulk_rt_log not declared", Some(call.callee.span))
            })?;
        let result = ctx
            .codegen
            .builder
            .build_call(rt_fn, &[base.into(), x.into()], "log_rt")
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
            .try_as_basic_value()
            .unwrap_basic();
        Ok(result)
    }
}

/// Lowers a `rand()` call.
///
/// Calls `hulk_rt_rand`, which returns a random `f64` in `[0, 1)`.
///
/// # Errors
/// Returns an error if any arguments are provided, or if `hulk_rt_rand` is
/// not declared.
fn lower_rand<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    call: &CallExpr<Type>,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    if !call.args.is_empty() {
        return Err(CodegenError::unsupported(
            "rand takes no arguments",
            Some(call.callee.span),
        ));
    }
    let rand_fn = ctx
        .codegen
        .functions
        .get("hulk_rt_rand")
        .copied()
        .ok_or_else(|| {
            CodegenError::unsupported("hulk_rt_rand not declared", Some(call.callee.span))
        })?;
    let result = ctx
        .codegen
        .builder
        .build_call(rand_fn, &[], "rand_call")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
        .try_as_basic_value()
        .unwrap_basic();
    Ok(result)
}

/// Lowers a `range(min, max)` call.
///
/// Calls `hulk_rt_range_new(min, max)`, which returns a `Range` object
/// usable in `for` loops.
///
/// # Errors
/// Returns an error if the argument count is not exactly 2, or if
/// `hulk_rt_range_new` is not declared.
fn lower_range<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    call: &CallExpr<Type>,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    if call.args.len() != 2 {
        return Err(CodegenError::unsupported(
            "range requires exactly two arguments (min, max)",
            Some(call.callee.span),
        ));
    }
    let min = lower_expr(ctx, &call.args[0])?.into_float_value();
    let max = lower_expr(ctx, &call.args[1])?.into_float_value();
    let range_fn = ctx
        .codegen
        .functions
        .get("hulk_rt_range_new")
        .copied()
        .ok_or_else(|| {
            CodegenError::unsupported("hulk_rt_range_new not declared", Some(call.callee.span))
        })?;
    let result = ctx
        .codegen
        .builder
        .build_call(range_fn, &[min.into(), max.into()], "range_new")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
        .try_as_basic_value()
        .unwrap_basic();
    Ok(result)
}
