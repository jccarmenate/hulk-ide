//! Lowering of vector indexing expressions (`expr[index]`).
//!
//! This module handles the code generation for reading an element from a vector
//! using the bracket syntax. It lowers the object and index expressions,
//! calls the runtime `hulk_rt_vector_get` function, and unboxes the result
//! if the element type is a primitive (`Number` or `Boolean`). The runtime
//! stores elements as boxed `Object*` pointers, so we must unbox primitive
//! values to their raw representation before returning them.
//!
//! # Related
//! - For assignment `a[i] := v`, see `lower::binding::lower_assign`.
//! - For unboxing logic, see `lower::utils::ensure_unboxed`.

use inkwell::values::BasicValueEnum;

use hulk_ast::{IndexExpr, SourceSpan};
use hulk_semantic::Type;

use super::lower_expr;
use crate::error::CodegenError;
use crate::lower::utils::ensure_unboxed;
use crate::lower::LowerCtx;

/// Lowers a vector indexing expression `object[index]`.
///
/// # Steps
/// 1. Lower the `object` expression to obtain a pointer to the vector.
/// 2. Lower the `index` expression (must be a `Number`) and convert it to `i64`.
/// 3. Call the runtime function `hulk_rt_vector_get(vec_ptr, index)` to obtain
///    the boxed element pointer.
/// 4. Unbox the pointer if the static element type is `Number` or `Boolean`.
/// 5. Return the raw value.
///
/// # Parameters
/// - `ctx`: The lowering context.
/// - `index_expr`: The AST node for the indexing expression.
/// - `expr_anno`: The static element type (from semantic analysis), used to
///   determine whether unboxing is required.
/// - `span`: The source span for error reporting.
///
/// # Returns
/// The LLVM value representing the element, either raw (for primitives) or
/// a pointer (for heap‑allocated types like `String`, `Object`, or `Named`).
///
/// # Errors
/// - `CodegenError::Unsupported` if the runtime function `hulk_rt_vector_get`
///   is not declared (should never happen after `runtime_decls::declare_all`).
/// - Propagates errors from lowering the object or index expressions.
pub fn lower_index_get<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    index_expr: &IndexExpr<Type>,
    expr_anno: &Type,
    span: SourceSpan,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    // 1. Lower the object expression (must be a vector pointer).
    let obj_val = lower_expr(ctx, &index_expr.object)?;
    let vec_ptr = obj_val.into_pointer_value();

    // 2. Lower the index expression (must be a Number).
    let idx_val = lower_expr(ctx, &index_expr.index)?;
    let idx_f64 = idx_val.into_float_value();
    let idx_i64 = ctx
        .codegen
        .builder
        .build_float_to_signed_int(idx_f64, ctx.codegen.context.i64_type(), "idx_cast")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // 3. Retrieve the runtime `vector_get` function.
    let get_fn = ctx
        .codegen
        .functions
        .get("hulk_rt_vector_get")
        .copied()
        .ok_or_else(|| {
            CodegenError::unsupported("hulk_rt_vector_get not declared in module", Some(span))
        })?;

    // 4. Call `hulk_rt_vector_get(vec_ptr, idx)`.
    let call_result = ctx
        .codegen
        .builder
        .build_call(get_fn, &[vec_ptr.into(), idx_i64.into()], "vec_get")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    let elem_ptr = call_result.try_as_basic_value().unwrap_basic(); // safe: function returns a pointer

    // 5. Unbox if the element type is primitive.
    let unboxed = ensure_unboxed(ctx, elem_ptr, expr_anno, Some(span))?;

    Ok(unboxed)
}

/// Lowers a vector index assignment `object[index] := value`.
///
/// # Steps
/// 1. Lower the `object` expression to obtain a pointer to the vector.
/// 2. Lower the `index` expression (must be a `Number`) and convert it to `i64`.
/// 3. Lower the `value` expression and box it if it is a primitive
///    (`Number` or `Boolean`) because vectors store `Object*`.
/// 4. Call the runtime function `hulk_rt_vector_set(vec_ptr, index, value)`.
/// 5. Return the assigned value (the value after boxing, for consistency).
///
/// # Parameters
/// - `ctx`: The lowering context.
/// - `object`: The vector expression.
/// - `index`: The index expression.
/// - `value`: The value to store.
/// - `span`: The source span for error reporting.
///
/// # Returns
/// The LLVM value that was stored (boxed if primitive, otherwise unchanged).
///
/// # Errors
/// - `CodegenError::Unsupported` if the runtime function `hulk_rt_vector_set`
///   is not declared.
/// - Propagates errors from lowering the object, index, or value expressions.
pub fn lower_index_assign<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    object: &hulk_ast::Expr<Type>,
    index: &hulk_ast::Expr<Type>,
    value: &hulk_ast::Expr<Type>,
    span: SourceSpan,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    // 1. Lower the object expression (must be a vector pointer).
    let obj_val = lower_expr(ctx, object)?;
    let vec_ptr = obj_val.into_pointer_value();

    // 2. Lower the index expression (must be a Number) and cast to i64.
    let idx_val = lower_expr(ctx, index)?;
    let idx_f64 = idx_val.into_float_value();
    let idx_i64 = ctx
        .codegen
        .builder
        .build_float_to_signed_int(idx_f64, ctx.codegen.context.i64_type(), "idx_cast")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // 3. Lower the value to be stored.
    let val = lower_expr(ctx, value)?;
    let val_ty = &value.anno;

    // 4. Box primitive values if needed.
    let boxed_val = crate::lower::utils::ensure_boxed(ctx, val, val_ty, &Type::Object)?;

    // 5. Retrieve the runtime `vector_set` function.
    let set_fn = ctx
        .codegen
        .functions
        .get("hulk_rt_vector_set")
        .copied()
        .ok_or_else(|| {
            CodegenError::unsupported("hulk_rt_vector_set not declared in module", Some(span))
        })?;

    // 6. Call hulk_rt_vector_set(vec_ptr, idx, boxed_val).
    ctx.codegen
        .builder
        .build_call(
            set_fn,
            &[vec_ptr.into(), idx_i64.into(), boxed_val.into()],
            "vec_set",
        )
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // 7. Return the boxed value (the assignment expression returns the stored value).
    Ok(boxed_val)
}
