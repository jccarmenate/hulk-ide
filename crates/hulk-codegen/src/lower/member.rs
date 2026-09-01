//! Lowering of member access: attributes and method references.

use hulk_ast::{MemberExpr, SourceSpan};
use hulk_semantic::Type;

use super::lower_expr;
use crate::error::CodegenError;
use crate::lower::utils::{
    field_indices, is_heap_allocated_type, llvm_type, resolve_attribute_with_offset,
};
use crate::lower::LowerCtx;

/// Lowers a member access expression.
///
/// If the member is an attribute, loads and returns its value.
/// If the member is a method, produces a fat pointer `{ self_ptr, fn_ptr }`
/// representing a function‑typed value.
pub fn lower_member<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    member: &MemberExpr<Type>,
    span: Option<SourceSpan>,
) -> Result<inkwell::values::BasicValueEnum<'ctx>, CodegenError> {
    // 1. Lower the object expression to get the object pointer.
    let obj_val = lower_expr(ctx, &member.object)?;
    let obj_ptr = obj_val.into_pointer_value();

    // 2. Determine the static type of the object.
    let obj_type = &member.object.anno;

    // 3. Resolve the member: attribute or method.
    if let Ok((attr_type, offset)) = resolve_attribute_with_offset(ctx, obj_type, &member.member) {
        return load_attribute(ctx, obj_ptr, &attr_type, offset);
    }

    if resolve_method(ctx, obj_type, &member.member) {
        return lower_method_reference(ctx, obj_ptr, obj_type, &member.member, span);
    }

    Err(CodegenError::unsupported(
        format!(
            "member '{}' not found in type '{}'",
            member.member, obj_type
        ),
        Some(member.object.span),
    ))
}

/// Lowers a member field assignment `object.field := value`.
///
/// # Steps
/// 1. Lower the `object` expression to obtain a pointer.
/// 2. Resolve the attribute's type and byte offset.
/// 3. Compute the field address using byte offset.
/// 4. Lower the `value` expression.
/// 5. Load the old value for release (if heap‑allocated).
/// 6. If the attribute is heap‑allocated, release the old value and retain the new.
/// 7. Store the new value.
/// 8. Return the stored value (the assignment expression's value).
///
/// # Parameters
/// - `ctx`: The lowering context.
/// - `object`: The object expression.
/// - `field`: The attribute name.
/// - `value`: The expression to store.
/// - `span`: The source span for error reporting.
///
/// # Returns
/// The LLVM value that was stored.
///
/// # Errors
/// - Propagates errors from lowering the object or value, resolving the attribute,
///   or LLVM instruction emission.
pub fn lower_member_assign<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    object: &hulk_ast::Expr<Type>,
    field: &str,
    value: &hulk_ast::Expr<Type>,
    span: SourceSpan,
) -> Result<inkwell::values::BasicValueEnum<'ctx>, CodegenError> {
    // 1. Lower the object expression to get a pointer.
    let obj_val = super::lower_expr(ctx, object)?;
    let obj_ptr = obj_val.into_pointer_value();

    // 2. Determine the static type of the object and resolve the attribute.
    let obj_type = &object.anno;
    let (attr_type, offset) = resolve_attribute_with_offset(ctx, obj_type, field)?;

    // 3. Compute the field address using byte offset.
    let ptr_type = ctx.codegen.context.ptr_type(Default::default());
    let obj_i8 = ctx
        .codegen
        .builder
        .build_pointer_cast(obj_ptr, ptr_type, "obj_i8")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    let offset_val = ctx
        .codegen
        .context
        .i64_type()
        .const_int(offset as u64, false);
    let field_ptr_i8 = unsafe {
        ctx.codegen
            .builder
            .build_gep(
                ctx.codegen.context.i8_type(),
                obj_i8,
                &[offset_val],
                "field_ptr",
            )
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
    };

    // 4. Cast to the attribute's type pointer.
    let attr_ptr_type = ctx.codegen.context.ptr_type(Default::default());
    let field_ptr = ctx
        .codegen
        .builder
        .build_pointer_cast(field_ptr_i8, attr_ptr_type, "field_typed_ptr")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // 5. Lower the new value.
    let val = super::lower_expr(ctx, value)?;

    // 6. Load the old value.
    let attr_llvm_ty = llvm_type(ctx.codegen, ctx.registry, &attr_type)?;
    let old_val = ctx
        .codegen
        .builder
        .build_load(attr_llvm_ty, field_ptr, "old_attr")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // 7. If the attribute is heap-allocated, release the old value and retain the new.
    if is_heap_allocated_type(&attr_type, ctx.registry) {
        let release_fn = ctx
            .codegen
            .functions
            .get("hulk_rt_release")
            .cloned()
            .ok_or_else(|| CodegenError::unsupported("hulk_rt_release not declared", Some(span)))?;
        ctx.codegen
            .builder
            .build_call(release_fn, &[old_val.into()], "release_old_attr")
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

        let retain_fn = ctx
            .codegen
            .functions
            .get("hulk_rt_retain")
            .cloned()
            .ok_or_else(|| CodegenError::unsupported("hulk_rt_retain not declared", Some(span)))?;
        ctx.codegen
            .builder
            .build_call(retain_fn, &[val.into()], "retain_new_attr")
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    }

    // 8. Store the new value.
    ctx.codegen
        .builder
        .build_store(field_ptr, val)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // 9. Assignment expression returns the value.
    Ok(val)
}

/// Loads an attribute value from the object at the given offset.
fn load_attribute<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    obj_ptr: inkwell::values::PointerValue<'ctx>,
    attr_type: &Type,
    offset: usize,
) -> Result<inkwell::values::BasicValueEnum<'ctx>, CodegenError> {
    let ptr_type = ctx.codegen.context.ptr_type(Default::default());
    let obj = ctx
        .codegen
        .builder
        .build_pointer_cast(obj_ptr, ptr_type, "obj_i8")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    let offset_val = ctx
        .codegen
        .context
        .i64_type()
        .const_int(offset as u64, false);
    let field_ptr_i8 = unsafe {
        ctx.codegen
            .builder
            .build_gep(
                ctx.codegen.context.i8_type(),
                obj,
                &[offset_val],
                "field_ptr",
            )
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
    };

    let attr_llvm_ty = crate::lower::utils::llvm_type(ctx.codegen, ctx.registry, attr_type)?;
    let attr_ptr_type = ctx.codegen.context.ptr_type(Default::default());
    let field_ptr = ctx
        .codegen
        .builder
        .build_pointer_cast(field_ptr_i8, attr_ptr_type, "field_typed_ptr")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // Load and return the attribute value.
    let val = ctx
        .codegen
        .builder
        .build_load(attr_llvm_ty, field_ptr, "attr_load")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    Ok(val)
}

/// Checks whether the member is a method of the given type.
fn resolve_method(ctx: &LowerCtx<'_, '_>, obj_type: &Type, member_name: &str) -> bool {
    let type_name = match obj_type {
        Type::Named(name) => name,
        _ => return false,
    };
    ctx.registry.lookup_type(type_name).is_some_and(|info| {
        let methods = if !info.flattened_methods.is_empty() {
            &info.flattened_methods
        } else {
            &info.methods
        };
        methods.contains_key(member_name)
    })
}

/// Produces a fat pointer `{ self_ptr, fn_ptr }` for a bare method reference.
fn lower_method_reference<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    obj_ptr: inkwell::values::PointerValue<'ctx>,
    obj_type: &Type,
    method_name: &str,
    span: Option<SourceSpan>,
) -> Result<inkwell::values::BasicValueEnum<'ctx>, CodegenError> {
    let type_name = match obj_type {
        Type::Named(name) => name,
        _ => {
            return Err(CodegenError::unsupported(
                format!("method reference on non‑named type: {:?}", obj_type),
                span,
            ));
        }
    };

    // Get the type layout and method slot index.
    let layout = ctx.codegen.type_layouts.get(type_name).ok_or_else(|| {
        CodegenError::unsupported(format!("no layout for type '{}'", type_name), span)
    })?;

    let slot_idx = *layout.method_slots.get(method_name).ok_or_else(|| {
        CodegenError::unsupported(
            format!("method '{}' not found in type '{}'", method_name, type_name),
            span,
        )
    })?;

    // Load the vtable pointer from the object header.
    let i32_type = ctx.codegen.context.i32_type();
    let ptr_type = ctx.codegen.context.ptr_type(Default::default());
    let struct_ty = layout.struct_ty;

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

    // Load the function pointer from the vtable at the slot index.
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

    // Build the fat pointer struct: { self_ptr, fn_ptr }
    let fat_ptr_ty = ctx
        .codegen
        .context
        .struct_type(&[ptr_type.into(), ptr_type.into()], false);
    let fat_ptr_alloca = ctx
        .codegen
        .builder
        .build_alloca(fat_ptr_ty, "fat_ptr")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // Store self pointer at index 0.
    let self_ptr_ptr = unsafe {
        ctx.codegen
            .builder
            .build_gep(
                fat_ptr_ty,
                fat_ptr_alloca,
                &[i32_type.const_int(0, false), i32_type.const_int(0, false)],
                "self_ptr_ptr",
            )
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
    };
    ctx.codegen
        .builder
        .build_store(self_ptr_ptr, obj_ptr)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // Store function pointer at index 1.
    let fn_ptr_ptr2 = unsafe {
        ctx.codegen
            .builder
            .build_gep(
                fat_ptr_ty,
                fat_ptr_alloca,
                &[i32_type.const_int(0, false), i32_type.const_int(1, false)],
                "fn_ptr_ptr2",
            )
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
    };
    ctx.codegen
        .builder
        .build_store(fn_ptr_ptr2, fn_ptr)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // Load the fat pointer struct as the result.
    let fat_ptr_val = ctx
        .codegen
        .builder
        .build_load(fat_ptr_ty, fat_ptr_alloca, "fat_ptr_val")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    Ok(fat_ptr_val)
}
