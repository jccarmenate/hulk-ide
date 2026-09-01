//! Lowering of function and method calls.

use hulk_ast::{CallExpr, SourceSpan};
use hulk_semantic::{Type, TypedExpr};
use inkwell::types::BasicType;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

use super::lower_expr;
use crate::error::CodegenError;
use crate::layout::has_subtypes;
use crate::lower::builtins::lower_builtin_call;
use crate::lower::utils::{field_indices, is_protocol_or_iterable as is_protocol_type, llvm_type};
use crate::lower::LowerCtx;

/// Lowers a call expression.
///
/// Handles three cases:
/// 1. Global function call: callee is a `Variable` (free function).
/// 2. Method call: callee is a `Member` expression (`obj.method(args)`).
///    Uses vtable dispatch (load vtable, index, indirect call).
/// 3. `base` call: callee is `BaseRef` – direct call to parent method.
///
/// # Parameters
/// - `ctx`: the lowering context.
/// - `call`: the call AST node.
///
/// # Returns
/// The value returned by the called function.
///
/// # Errors
/// - `Unsupported` if the callee is not a variable name or if the function
///   is not found in the module (should not happen if declared correctly).
/// - `LlvmVerification` if building the call instruction fails.
pub fn lower_call<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    call: &CallExpr<Type>,
) -> Result<inkwell::values::BasicValueEnum<'ctx>, CodegenError> {
    // Try built-in functions first
    if let Some(result) = lower_builtin_call(ctx, call) {
        return result;
    }

    match &call.callee.kind {
        // ─── Global function call ──────────────────────────────────────────
        hulk_ast::ExprKind::Variable(name) => {
            if let Some(fn_val) = ctx.codegen.functions.get(name).copied() {
                // Extract the parameter types from the callee's own annotation.
                // The type checker has already resolved this to Type::Function.
                let param_types = match &call.callee.anno {
                    Type::Function { params, .. } => params,
                    other => {
                        return Err(CodegenError::unsupported(
                            format!(
                                "call to variable `{}` with non-function type `{:?}`",
                                name, other
                            ),
                            Some(call.callee.span),
                        ));
                    }
                };

                // Lower each argument, boxing if the parameter type is Object.
                let mut args = Vec::with_capacity(call.args.len());
                for (arg_expr, param_ty) in call.args.iter().zip(param_types) {
                    let mut arg_val = lower_expr(ctx, arg_expr)?;
                    if matches!(param_ty, Type::Object) {
                        arg_val = crate::lower::utils::ensure_boxed(
                            ctx,
                            arg_val,
                            &arg_expr.anno,
                            param_ty,
                        )?;
                    } else if let Type::Named(_) = &param_ty {
                        // WHY: Go/Kotlin interface boxing pattern — when passing a Named
                        // concrete class where a Named protocol is expected, build the
                        // fat pointer { data_ptr, itable_ptr } at the call site.
                        // is_protocol() returns true only for protocol types, not classes.
                        if ctx.registry.is_protocol(param_ty) {
                            if let Type::Named(_) = &arg_expr.anno {
                                if !ctx.registry.is_protocol(&arg_expr.anno) {
                                    arg_val = crate::lower::utils::convert_to_protocol(
                                        ctx,
                                        arg_val,
                                        &arg_expr.anno,
                                        param_ty,
                                    )?;
                                }
                            }
                        }
                    } else if let Type::Iterable(_) = &param_ty {
                        // WHY: Iterable<T> is a fat pointer { data_ptr, itable_ptr }.
                        // When passing a Named class to an Iterable<T> parameter, we
                        // must box it to the fat pointer — same pattern as Named protocol.
                        if let Type::Named(_) = &arg_expr.anno {
                            if !ctx.registry.is_protocol(&arg_expr.anno) {
                                arg_val = crate::lower::utils::convert_to_protocol(
                                    ctx,
                                    arg_val,
                                    &arg_expr.anno,
                                    param_ty,
                                )?;
                            }
                        }
                    }
                    args.push(arg_val.into());
                }

                let call_site = ctx
                    .codegen
                    .builder
                    .build_call(fn_val, &args, "call")
                    .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
                let result = call_site.try_as_basic_value().unwrap_basic();
                return Ok(result);
            }
        }

        // ─── Method call (`obj.method(args)`) ─────────────────────────────
        hulk_ast::ExprKind::Member(member) => {
            return lower_method_call(
                ctx,
                *member.object.clone(),
                &member.member,
                &call.args,
                call.callee.span,
            );
        }

        // ─── `base` call ──────────────────────────────────────────────────
        hulk_ast::ExprKind::BaseRef => {
            return lower_base_call(ctx, call);
        }

        _ => {}
    }

    // Generic function‑value call (e.g., a variable holding a method reference).
    let callee_val = lower_expr(ctx, &call.callee)?;
    match &call.callee.anno {
        Type::Function { .. } => lower_function_value(
            ctx,
            callee_val,
            &call.callee.anno,
            &call.args,
            Some(call.callee.span),
        ),
        _ => Err(CodegenError::unsupported(
            format!("unable to resolve call to type `{}`", call.callee.anno),
            Some(call.callee.span),
        )),
    }
}

/// Lowers a method call on an object expression.
///
/// Dispatches based on the object's static type:
/// - Builtin types (`Vector`, `Range`) → direct call to a runtime function.
/// - Protocol types (`Named` protocol or `Iterable`) → itable dispatch via fat pointer.
/// - Class types (`Named` type) → vtable dispatch.
pub fn lower_method_call_val<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    obj_val: BasicValueEnum<'ctx>,
    obj_type: &Type,
    method_name: &str,
    span: SourceSpan,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    if let Type::Vector(_) = obj_type {
        return lower_builtin_vector_call(ctx, obj_val, method_name, &[], span);
    }
    if let Type::Named(name) = obj_type {
        if name == "Range" {
            return lower_builtin_range_call(ctx, obj_val, method_name, &[], span);
        }
        // WHY: Python, Java, C# for loops use vtable dispatch on any type that
        // implements the iterator protocol — no hardcoded allow-list.  User-defined
        // generator types expose next()/current() as ordinary class methods and
        // must be dispatched via vtable exactly like any other method call.
        return lower_named_method_from_val(ctx, obj_val, name, method_name, span);
    }
    if is_protocol_type(obj_type, ctx.registry) {
        return lower_protocol_call(ctx, obj_val, obj_type, method_name, &[], span);
    }
    Err(CodegenError::unsupported(
        format!(
            "iterable method `{}` not supported on type `{}`",
            method_name, obj_type
        ),
        Some(span),
    ))
}

/// Dispatch a zero-argument method call on an already-evaluated user-defined
/// class value via vtable (or direct call when the type is monomorphic).
///
/// This is the `lower_method_call_val` counterpart of `lower_class_method_call`:
/// the receiver has already been lowered into `obj_val`, so we skip `lower_expr`.
fn lower_named_method_from_val<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    obj_val: BasicValueEnum<'ctx>,
    type_name: &str,
    method_name: &str,
    span: SourceSpan,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    let obj_ptr = obj_val.into_pointer_value();
    let call_args: Vec<BasicMetadataValueEnum> = vec![obj_ptr.into()];

    // Devirtualize when the type has no subtypes.
    if !has_subtypes(type_name, ctx.registry) {
        let owner =
            crate::layout::owning_type_for_method(type_name, method_name, ctx.registry)
                .unwrap_or_else(|| type_name.to_string());
        let qualified_name = format!("{}::{}", owner, method_name);
        if let Some(fn_val) = ctx.codegen.functions.get(&qualified_name) {
            let call_site = ctx
                .codegen
                .builder
                .build_call(*fn_val, &call_args, "direct_method_call")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            return Ok(call_site.try_as_basic_value().unwrap_basic());
        }
    }

    // Vtable dispatch.
    let layout = ctx.codegen.type_layouts.get(type_name).ok_or_else(|| {
        CodegenError::unsupported(format!("no layout for type '{}'", type_name), Some(span))
    })?;
    let struct_ty = layout.struct_ty; // Copy — released before builder borrow
    let slot_idx = *layout.method_slots.get(method_name).ok_or_else(|| {
        CodegenError::unsupported(
            format!("method '{}' not in vtable for type '{}'", method_name, type_name),
            Some(span),
        )
    })?;

    let i32_type = ctx.codegen.context.i32_type();
    let ptr_type = ctx.codegen.context.ptr_type(Default::default());
    let vtable_ptr_ptr = unsafe {
        ctx.codegen
            .builder
            .build_gep(
                struct_ty,
                obj_ptr,
                &[
                    i32_type.const_int(0, false),
                    i32_type.const_int(field_indices::VTABLE as u64, false),
                ],
                "vtable_ptr_ptr",
            )
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
    };
    let vtable_ptr = ctx
        .codegen
        .builder
        .build_load(ptr_type, vtable_ptr_ptr, "vtable_ptr")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
        .into_pointer_value();

    let slot_val = i32_type.const_int(slot_idx as u64, false);
    let fn_ptr_ptr = unsafe {
        ctx.codegen
            .builder
            .build_gep(ptr_type, vtable_ptr, &[slot_val], "fn_ptr_ptr")
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
    };
    let fn_ptr = ctx
        .codegen
        .builder
        .build_load(ptr_type, fn_ptr_ptr, "fn_ptr")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
        .into_pointer_value();

    let fn_owner =
        crate::layout::owning_type_for_method(type_name, method_name, ctx.registry)
            .unwrap_or_else(|| type_name.to_string());
    let qualified_name = format!("{}::{}", fn_owner, method_name);
    let fn_decl = ctx
        .codegen
        .functions
        .get(&qualified_name)
        .cloned()
        .ok_or_else(|| {
            CodegenError::unsupported(
                format!("method '{}' not declared", qualified_name),
                Some(span),
            )
        })?;
    let fn_type = fn_decl.get_type();

    let call_site = ctx
        .codegen
        .builder
        .build_indirect_call(fn_type, fn_ptr, &call_args, "method_call")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    Ok(call_site.try_as_basic_value().unwrap_basic())
}

pub fn lower_method_call<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    object: TypedExpr,
    method_name: &str,
    args: &[TypedExpr],
    span: SourceSpan,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    let obj_val = lower_expr(ctx, &object)?;
    let obj_type = &object.anno;

    // ── Builtin types: direct calls ────────────────────────────────────
    if let Type::Vector(_) = obj_type {
        return lower_builtin_vector_call(ctx, obj_val, method_name, args, span);
    }
    if let Type::Named(name) = obj_type {
        if name == "Range" {
            return lower_builtin_range_call(ctx, obj_val, method_name, args, span);
        }
    }

    // ── Protocol types: itable dispatch ───────────────────────────────
    if is_protocol_type(obj_type, ctx.registry) {
        return lower_protocol_call(ctx, obj_val, obj_type, method_name, args, span);
    }

    // ── Class types: vtable dispatch ──────────────────────────────────
    lower_class_method_call(ctx, object, method_name, args, span)
}

/// Lowers a method call using vtable dispatch.
///
/// # Parameters
/// - `ctx`: The lowering context.
/// - `object`: The receiver expression (already lowered to a value).
/// - `method_name`: The name of the method to call.
/// - `args`: The arguments passed to the method.
/// - `span`: The source span for error reporting.
///
/// # Returns
/// The value returned by the method call.
fn lower_class_method_call<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    object: TypedExpr,
    method_name: &str,
    args: &[TypedExpr],
    span: SourceSpan,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    // 1. Lower the object expression to obtain a pointer to the receiver.
    let obj_val = lower_expr(ctx, &object)?;
    let obj_ptr = obj_val.into_pointer_value();

    // 2. Determine the static type of the object.
    let obj_type = &object.anno;
    let type_name = match obj_type {
        Type::Named(name) => name,
        _ => {
            return Err(CodegenError::unsupported(
                format!("method call on non-named type: {:?}", obj_type),
                Some(span),
            ));
        }
    };

    // 3. Look up the method signature to get parameter types and the declaring type.
    let method_sig = ctx
        .registry
        .lookup_method(obj_type, method_name)
        .ok_or_else(|| {
            CodegenError::unsupported(format!("method '{}' not found", method_name), Some(span))
        })?;

    // 4. Prepare the call arguments: first `self` (the object pointer), then the rest.
    let mut call_args = vec![obj_ptr.into()];
    call_args.extend(lower_and_box_args(ctx, args, &method_sig.params)?);

    // 5. Devirtualization: if the type has no subtypes, we can call the method directly.
    if !has_subtypes(type_name, ctx.registry) {
        // WHY: use owning_type_for_method — TypeInfo.methods is merged/flattened,
        // so type_name::method may not exist; the body lives in the declaring type.
        let owner = crate::layout::owning_type_for_method(type_name, method_name, ctx.registry)
            .unwrap_or_else(|| type_name.to_string());
        let qualified_name = format!("{}::{}", owner, method_name);
        if let Some(fn_val) = ctx.codegen.functions.get(&qualified_name) {
            let call_site = ctx
                .codegen
                .builder
                .build_call(*fn_val, &call_args, "direct_method_call")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            let result = call_site.try_as_basic_value().unwrap_basic();
            return Ok(result);
        }
        // Fallback to vtable if function not found.
    }

    // 6. Look up the type layout to obtain the method slot index.
    let layout = ctx.codegen.type_layouts.get(type_name).ok_or_else(|| {
        CodegenError::unsupported(format!("no layout for type '{}'", type_name), Some(span))
    })?;

    let slot_idx = *layout.method_slots.get(method_name).ok_or_else(|| {
        CodegenError::unsupported(
            format!("method '{}' not found in type '{}'", method_name, type_name),
            Some(span),
        )
    })?;

    // 7. Load the vtable pointer from the object header (field index 3).
    let i32_type = ctx.codegen.context.i32_type();
    let ptr_type = ctx.codegen.context.ptr_type(Default::default());
    let vtable_ptr_ptr = unsafe {
        ctx.codegen
            .builder
            .build_gep(
                layout.struct_ty,
                obj_ptr,
                &[
                    i32_type.const_int(0, false),
                    i32_type.const_int(field_indices::VTABLE as u64, false),
                ],
                "vtable_ptr_ptr",
            )
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
    };
    let vtable_ptr = ctx
        .codegen
        .builder
        .build_load(ptr_type, vtable_ptr_ptr, "vtable_ptr")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
        .into_pointer_value();

    // 8. Load the function pointer from the vtable at the computed slot index.
    let slot_val = ctx
        .codegen
        .context
        .i32_type()
        .const_int(slot_idx as u64, false);
    let fn_ptr_ptr = unsafe {
        ctx.codegen
            .builder
            .build_gep(ptr_type, vtable_ptr, &[slot_val], "fn_ptr_ptr")
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
    };
    let fn_ptr = ctx
        .codegen
        .builder
        .build_load(ptr_type, fn_ptr_ptr, "fn_ptr")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
        .into_pointer_value();

    // 9. Retrieve the method's declared LLVM function type from the module.
    // WHY: use owning_type_for_method — the function was declared under the
    // defining type, not the receiver type (which may have inherited it).
    let fn_owner = crate::layout::owning_type_for_method(type_name, method_name, ctx.registry)
        .unwrap_or_else(|| type_name.to_string());
    let qualified_name = format!("{}::{}", fn_owner, method_name);
    let fn_decl = ctx
        .codegen
        .functions
        .get(&qualified_name)
        .cloned()
        .ok_or_else(|| {
            CodegenError::unsupported(
                format!("method '{}' not declared", qualified_name),
                Some(span),
            )
        })?;
    let fn_type = fn_decl.get_type();

    // 10. Build an indirect call using the loaded function pointer.
    let call_site = ctx
        .codegen
        .builder
        .build_indirect_call(fn_type, fn_ptr, &call_args, "method_call")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    let result = call_site.try_as_basic_value().unwrap_basic();
    Ok(result)
}

/// Lowers a `base` call (direct call to the parent's method).
fn lower_base_call<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    call: &CallExpr<Type>,
) -> Result<inkwell::values::BasicValueEnum<'ctx>, CodegenError> {
    let (current_type, current_method) = match (&ctx.current_type, &ctx.current_method) {
        (Some(ty), Some(meth)) => (ty, meth),
        _ => {
            return Err(CodegenError::unsupported(
                "base call outside of an overriding method",
                Some(call.callee.span),
            ));
        }
    };

    // Look up the parent type.
    let parent_name = ctx.registry.parent_of(current_type).ok_or_else(|| {
        CodegenError::unsupported(
            format!("type '{}' has no parent", current_type),
            Some(call.callee.span),
        )
    })?;

    // Look up the parent method signature.
    let parent_info = ctx.registry.lookup_type(&parent_name).ok_or_else(|| {
        CodegenError::unsupported(
            format!("parent type '{}' not found", parent_name),
            Some(call.callee.span),
        )
    })?;
    if !parent_info.methods.contains_key(current_method) {
        return Err(CodegenError::unsupported(
            format!(
                "method '{}' not found in parent type '{}'",
                current_method, parent_name
            ),
            Some(call.callee.span),
        ));
    }

    // Get the function from the module using the qualified name.
    let qualified_name = format!("{}::{}", parent_name, current_method);
    let fn_val = ctx
        .codegen
        .functions
        .get(&qualified_name)
        .cloned()
        .ok_or_else(|| {
            CodegenError::unsupported(
                format!("parent method '{}' not declared", qualified_name),
                Some(call.callee.span),
            )
        })?;

    // Prepare arguments: `self` is the first argument.
    // WHY: scope_stack stores the alloca address (.0), not the value it holds.
    // We must load the actual object pointer out of the alloca before passing it
    // as `self` to the parent method — otherwise the callee receives the stack
    // slot address and treats it as the object, reading garbage at offset 32+.
    let (self_alloca, self_llvm_ty, _) = ctx
        .scope_stack
        .lookup("self")
        .ok_or_else(|| CodegenError::unsupported("self not in scope", Some(call.callee.span)))?;
    let self_val = ctx
        .codegen
        .builder
        .build_load(self_llvm_ty, self_alloca, "self_for_base")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    let mut args = Vec::new();
    args.push(self_val.into());
    for arg in &call.args {
        args.push(lower_expr(ctx, arg)?.into());
    }

    // Build a direct call.
    let call_site = ctx
        .codegen
        .builder
        .build_call(fn_val, &args, "base_call")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    let result = call_site.try_as_basic_value().unwrap_basic();
    Ok(result)
}

/// Lowers a call to a function‑typed value (fat pointer `{ self_ptr, fn_ptr }`).
///
/// # Parameters
/// - `ctx`: the lowering context.
/// - `callee_val`: the LLVM value of the callee (must be a struct of two pointers).
/// - `callee_ty`: the static type of the callee, expected to be `Type::Function`.
/// - `args`: the AST arguments to the call.
///
/// # Returns
/// The value returned by the called function.
fn lower_function_value<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    callee_val: inkwell::values::BasicValueEnum<'ctx>,
    callee_ty: &Type,
    args: &[hulk_ast::Expr<Type>],
    span: Option<SourceSpan>,
) -> Result<inkwell::values::BasicValueEnum<'ctx>, CodegenError> {
    // Extract the function type details.
    let (param_types, return_type) = match callee_ty {
        Type::Function {
            params,
            return_type,
        } => (params, return_type.as_ref()),
        _ => {
            return Err(CodegenError::unsupported("expected function type", span));
        }
    };

    // Ensure the callee value is a struct of two pointers (fat pointer).
    let struct_val = callee_val.into_struct_value();

    // Extract self_ptr and fn_ptr using `build_extract_value`.
    let self_ptr = ctx
        .codegen
        .builder
        .build_extract_value(struct_val, 0, "self_ptr")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
        .into_pointer_value();
    let fn_ptr = ctx
        .codegen
        .builder
        .build_extract_value(struct_val, 1, "fn_ptr")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
        .into_pointer_value();

    // Build the function type for the indirect call.
    let llvm_param_types: Vec<inkwell::types::BasicMetadataTypeEnum> = param_types
        .iter()
        .map(|ty| llvm_type(ctx.codegen, ctx.registry, ty).map(|t| t.into()))
        .collect::<Result<Vec<_>, _>>()?;
    let llvm_return = llvm_type(ctx.codegen, ctx.registry, return_type)?;

    // Method function pointers expect `self` as the first parameter.
    let self_ptr_type = ctx.codegen.context.ptr_type(Default::default());
    let mut all_param_types = vec![self_ptr_type.into()];
    all_param_types.extend(llvm_param_types);
    let fn_type = llvm_return.fn_type(&all_param_types, false);

    // Cast fn_ptr to the correct function pointer type.
    let fn_ptr_typed = ctx
        .codegen
        .builder
        .build_pointer_cast(
            fn_ptr,
            ctx.codegen.context.ptr_type(Default::default()),
            "fn_ptr_typed",
        )
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // Prepare arguments: first self_ptr, then the rest.
    let mut call_args = Vec::new();
    call_args.push(self_ptr.into());
    for arg_expr in args {
        call_args.push(lower_expr(ctx, arg_expr)?.into());
    }

    // Build indirect call.
    let call_site = ctx
        .codegen
        .builder
        .build_indirect_call(fn_type, fn_ptr_typed, &call_args, "call_fat_ptr")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    let result = call_site.try_as_basic_value().unwrap_basic();
    Ok(result)
}

/// Lowers a call to a builtin `Vector` method.
fn lower_builtin_vector_call<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    obj_val: BasicValueEnum<'ctx>,
    method_name: &str,
    args: &[TypedExpr],
    span: SourceSpan,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    let obj_ptr = obj_val.into_pointer_value();
    let runtime_name = match method_name {
        "size" => "hulk_rt_vector_size",
        "get" => "hulk_rt_vector_get",
        "set" => "hulk_rt_vector_set",
        "next" => "hulk_rt_vector_next",
        "current" => "hulk_rt_vector_current",
        _ => {
            return Err(CodegenError::unsupported(
                format!("Vector method `{}` not implemented", method_name),
                Some(span),
            ))
        }
    };
    let fn_val = ctx
        .codegen
        .functions
        .get(runtime_name)
        .cloned()
        .ok_or_else(|| {
            CodegenError::unsupported(
                format!("runtime function `{}` not declared", runtime_name),
                Some(span),
            )
        })?;

    let mut call_args = vec![obj_ptr.into()];
    for arg in args {
        call_args.push(lower_expr(ctx, arg)?.into());
    }

    let call_site = ctx
        .codegen
        .builder
        .build_call(fn_val, &call_args, "vector_call")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    Ok(call_site.try_as_basic_value().unwrap_basic())
}

/// Lowers a call to a builtin `Range` method.
fn lower_builtin_range_call<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    obj_val: BasicValueEnum<'ctx>,
    method_name: &str,
    args: &[TypedExpr],
    span: SourceSpan,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    let obj_ptr = obj_val.into_pointer_value();
    let runtime_name = match method_name {
        "next" => "hulk_rt_range_next",
        "current" => "hulk_rt_range_current",
        _ => {
            return Err(CodegenError::unsupported(
                format!("Range method `{}` not implemented", method_name),
                Some(span),
            ))
        }
    };
    let fn_val = ctx
        .codegen
        .functions
        .get(runtime_name)
        .cloned()
        .ok_or_else(|| {
            CodegenError::unsupported(
                format!("runtime function `{}` not declared", runtime_name),
                Some(span),
            )
        })?;

    let mut call_args = vec![obj_ptr.into()];
    for arg in args {
        call_args.push(lower_expr(ctx, arg)?.into());
    }

    let call_site = ctx
        .codegen
        .builder
        .build_call(fn_val, &call_args, "range_call")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    Ok(call_site.try_as_basic_value().unwrap_basic())
}

/// Lowers a method call on a protocol‑typed value using itable dispatch.
///
/// Expects `obj_val` to be a fat pointer `{ ptr data, ptr itable }`.
fn lower_protocol_call<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    obj_val: BasicValueEnum<'ctx>,
    obj_type: &Type,
    method_name: &str,
    args: &[TypedExpr],
    span: SourceSpan,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    // 1. Extract data and itable from the fat pointer.
    let struct_val = obj_val.into_struct_value();
    let data_ptr = ctx
        .codegen
        .builder
        .build_extract_value(struct_val, 0, "data_ptr")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
        .into_pointer_value();
    let itable_ptr = ctx
        .codegen
        .builder
        .build_extract_value(struct_val, 1, "itable_ptr")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
        .into_pointer_value();

    // 2. Determine the protocol name (for Iterable, use "Iterable").
    let protocol_name = match obj_type {
        Type::Named(name) => name.as_str(),
        Type::Iterable(_) => "Iterable",
        _ => {
            return Err(CodegenError::unsupported(
                format!("expected protocol type, got {:?}", obj_type),
                Some(span),
            ))
        }
    };

    // 3. Get the method slot index in the protocol's flattened method table.
    let slot = crate::itables::protocol_method_slot(ctx.registry, protocol_name, method_name)
        .ok_or_else(|| {
            CodegenError::unsupported(
                format!(
                    "method `{}` not found in protocol `{}`",
                    method_name, protocol_name
                ),
                Some(span),
            )
        })?;

    // 4. Load the function pointer from the itable.
    let ptr_type = ctx.codegen.context.ptr_type(Default::default());
    let slot_val = ctx.codegen.context.i32_type().const_int(slot as u64, false);
    let fn_ptr_ptr = unsafe {
        ctx.codegen
            .builder
            .build_gep(ptr_type, itable_ptr, &[slot_val], "fn_ptr_ptr")
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
    };
    let fn_ptr = ctx
        .codegen
        .builder
        .build_load(ptr_type, fn_ptr_ptr, "fn_ptr")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
        .into_pointer_value();

    // 5. Build the function type from the method signature.
    let method_sig = ctx
        .registry
        .lookup_method(obj_type, method_name)
        .ok_or_else(|| {
            CodegenError::unsupported(format!("method `{}` not found", method_name), Some(span))
        })?;
    let mut param_types: Vec<inkwell::types::BasicMetadataTypeEnum> = Vec::new();
    for (_, ty) in &method_sig.params {
        let llvm_ty = llvm_type(ctx.codegen, ctx.registry, ty)?;
        param_types.push(llvm_ty.into());
    }
    let return_ty = llvm_type(ctx.codegen, ctx.registry, &method_sig.return_type)?;
    let mut all_param_types = vec![ptr_type.into()];
    all_param_types.extend(param_types);
    let fn_type = return_ty.fn_type(&all_param_types, false);

    // 6. Cast the function pointer to the correct type.
    let fn_ptr_typed = ctx
        .codegen
        .builder
        .build_pointer_cast(
            fn_ptr,
            ctx.codegen.context.ptr_type(Default::default()),
            "fn_ptr_typed",
        )
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // 7. Prepare arguments: data_ptr as `self`, then the rest.
    let mut call_args = vec![data_ptr.into()];

    // Lower and box each argument according to the parameter type.
    call_args.extend(lower_and_box_args(ctx, args, &method_sig.params)?);

    let call_site = ctx
        .codegen
        .builder
        .build_indirect_call(fn_type, fn_ptr_typed, &call_args, "itable_call")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    Ok(call_site.try_as_basic_value().unwrap_basic())
}

// --------------- HELPERS ------------------------

/// Lowers a list of argument expressions and boxes them according to the corresponding
/// parameter types.
///
/// # Parameters
/// - `ctx`: The lowering context.
/// - `args`: The argument expressions to lower.
/// - `params`: The function/method parameter list `(name, type)`.
///
/// # Returns
/// A vector of LLVM metadata values ready to be passed to `build_call` or
/// `build_indirect_call`.
fn lower_and_box_args<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    args: &[TypedExpr],
    params: &[(String, Type)],
) -> Result<Vec<BasicMetadataValueEnum<'ctx>>, CodegenError> {
    let mut call_args = Vec::with_capacity(args.len());
    for (arg_expr, (_, param_ty)) in args.iter().zip(params) {
        let mut arg_val = lower_expr(ctx, arg_expr)?;
        if matches!(param_ty, Type::Object) {
            arg_val = crate::lower::utils::ensure_boxed(ctx, arg_val, &arg_expr.anno, param_ty)?;
        } else if let Type::Named(_) = &param_ty {
            // WHY: Go/Kotlin interface boxing pattern — when passing a Named
            // concrete class where a Named protocol is expected, build the
            // fat pointer { data_ptr, itable_ptr } at the call site.
            // is_protocol() returns true only for protocol types, not classes.
            if ctx.registry.is_protocol(param_ty) {
                if let Type::Named(_) = &arg_expr.anno {
                    if !ctx.registry.is_protocol(&arg_expr.anno) {
                        arg_val = crate::lower::utils::convert_to_protocol(
                            ctx,
                            arg_val,
                            &arg_expr.anno,
                            param_ty,
                        )?;
                    }
                }
            }
        } else if let Type::Iterable(_) = &param_ty {
            // WHY: Iterable<T> is a fat pointer { data_ptr, itable_ptr }.
            // When passing a Named class to an Iterable<T> parameter, we
            // must box it to the fat pointer — same pattern as Named protocol.
            if let Type::Named(_) = &arg_expr.anno {
                if !ctx.registry.is_protocol(&arg_expr.anno) {
                    arg_val = crate::lower::utils::convert_to_protocol(
                        ctx,
                        arg_val,
                        &arg_expr.anno,
                        param_ty,
                    )?;
                }
            }
        }
        call_args.push(arg_val.into());
    }
    Ok(call_args)
}
