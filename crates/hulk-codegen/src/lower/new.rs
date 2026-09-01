//! Lowering of object construction (`new T(args)`).

use hulk_ast::{NewExpr, SourceSpan, TypeDecl, TypeMemberKind};
use hulk_rt::TAG_OBJECT;
use hulk_semantic::{Type, TypeRegistry};

use super::lower_expr;
use crate::error::CodegenError;
use crate::lower::utils::{field_indices, ensure_boxed};
use crate::lower::LowerCtx;

/// Lowers a `new T(args)` expression.
///
/// Steps:
/// 1. Allocate memory for the object.
/// 2. Initialise the object header: ref_count, gc_mark, next, vtable.
/// 3. Evaluate attribute initializers in parent‑first order.
/// 4. Return a pointer to the new object.
pub fn lower_new<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    new_expr: &NewExpr<Type>,
    span: Option<SourceSpan>,
) -> Result<inkwell::values::BasicValueEnum<'ctx>, CodegenError> {
    // If this is a vector allocation, delegate to the vector lowering function.
    if new_expr.size.is_some() {
        return crate::lower::vector::lower_new_vector(
            ctx,
            new_expr,
            span.unwrap_or(SourceSpan::new(0, 0)),
        );
    }

    let type_name = &new_expr.type_name.name;

    // --- 0. Collect all needed data before borrowing ctx mutably ------------

    // Compute ancestors (uses ctx.registry immutably, but returns owned data).
    let ancestors = collect_ancestors(type_name, ctx.registry);

    // Look up type info and extract params (owned).
    let params = {
        let info = ctx.registry.lookup_type(type_name).ok_or_else(|| {
            CodegenError::unsupported(format!("type '{}' not found", type_name), span)
        })?;
        info.params.clone()
    };

    // Look up layout and extract all needed fields into owned values.
    let (struct_ty, size, field_offsets, vtable_global) = {
        let layout = ctx.codegen.type_layouts.get(type_name).ok_or_else(|| {
            CodegenError::unsupported(format!("no layout for type '{}'", type_name), span)
        })?;
        (
            layout.struct_ty,
            layout.size,
            layout.field_offsets.clone(),
            layout.vtable_global,
        )
    };

    // Now we have all needed data as owned values; we can safely mutably borrow ctx.

    // --- 1. Allocate memory -------------------------------------------------

    let size_val = ctx.codegen.context.i64_type().const_int(size as u64, false);
    let alloc_fn = ctx
        .codegen
        .functions
        .get("hulk_rt_alloc")
        .cloned()
        .ok_or_else(|| CodegenError::unsupported("hulk_rt_alloc not declared", span))?;

    let call = ctx
        .codegen
        .builder
        .build_call(alloc_fn, &[size_val.into()], "alloc")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    let obj_ptr = call
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| CodegenError::llvm_verification("alloc returned void"))?
        .into_pointer_value();

    // --- 2. Initialise header ------------------------------------------------

    // Use local `struct_ty` and other values.
    let i32_type = ctx.codegen.context.i32_type();
    let i64_type = ctx.codegen.context.i64_type();
    let i1_type = ctx.codegen.context.bool_type();
    let i8_type = ctx.codegen.context.i8_type(); // for type_tag
    // let ptr_type = ctx.codegen.context.ptr_type(Default::default());

    // Helper: GEP into the struct at field index `field_idx` (0‑based).
    let gep_field = |field_idx: u32| -> Result<inkwell::values::PointerValue, _> {
        unsafe {
            ctx.codegen
                .builder
                .build_gep(
                    struct_ty,
                    obj_ptr,
                    &[
                        i32_type.const_int(0, false),
                        i32_type.const_int(field_idx.into(), false),
                    ],
                    "field_ptr",
                )
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))
        }
    };

    // ref_count = 1
    let ref_count_ptr = gep_field(field_indices::REF_COUNT)?;
    ctx.codegen
        .builder
        .build_store(ref_count_ptr, i64_type.const_int(1, false))
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // gc_mark = false
    let gc_mark_ptr = gep_field(field_indices::GC_MARK)?;
    ctx.codegen
        .builder
        .build_store(gc_mark_ptr, i1_type.const_int(0, false))
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // type_tag = TAG_OBJECT – identifies this as a plain user object.
    let tag_ptr = gep_field(field_indices::TYPE_TAG)?;
    ctx.codegen
        .builder
        .build_store(tag_ptr, i8_type.const_int(TAG_OBJECT as u64, false))
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // vtable = global
    let vtable_global = vtable_global.ok_or_else(|| {
        CodegenError::unsupported(format!("vtable for '{}' not built", type_name), span)
    })?;
    let vtable_ptr = vtable_global.as_pointer_value();
    let vtable_ptr_ptr = gep_field(field_indices::VTABLE)?;
    ctx.codegen
        .builder
        .build_store(vtable_ptr_ptr, vtable_ptr)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // --- 3. Bind constructor parameters --------------------------------------

    ctx.push_scope();

    if new_expr.args.len() != params.len() {
        return Err(CodegenError::llvm_verification("argument count mismatch"));
    }
    for (i, (param_name, _)) in params.iter().enumerate() {
        let arg_val = lower_expr(ctx, &new_expr.args[i])?;
        let param_ty = params[i].1.clone(); // get the semantic type
        ctx.declare_var(param_name, arg_val, param_ty, false)?;
    }

    // ─── 4. Evaluate attribute initializers in parent‑first order ────────────

    // `ancestors` is already computed; use it.
    let mut attr_inits = Vec::new();
    for ancestor_name in &ancestors {
        if let Some(ty_decl) = ctx.type_decls.get(ancestor_name) {
            collect_attribute_initializers(ty_decl, &mut attr_inits);
        }
    }
    if let Some(ty_decl) = ctx.type_decls.get(type_name) {
        collect_attribute_initializers(ty_decl, &mut attr_inits);
    }

    // Get the retain function once, outside the loop.
    let retain_fn = ctx
        .codegen
        .functions
        .get("hulk_rt_retain")
        .cloned()
        .ok_or_else(|| CodegenError::unsupported("hulk_rt_retain not declared", span))?;

    // Now evaluate each initializer and store it at the correct offset.
    for (attr_name, init_expr) in attr_inits {
        // 1. Resolve the attribute's declared type.
        let attr_info = ctx
            .registry
            .lookup_type(type_name)
            .and_then(|info| info.attributes.get(&attr_name))
            .ok_or_else(|| {
                CodegenError::unsupported(
                    format!("attribute info for '{}' not found", attr_name),
                    span,
                )
            })?;
        let attr_ty = attr_info.declared_type.as_ref().ok_or_else(|| {
            CodegenError::unsupported(
                format!("attribute '{}' has no declared type", attr_name),
                span,
            )
        })?;

        // 2. Compute the byte offset of the attribute.
        let offset = *field_offsets.get(&attr_name).ok_or_else(|| {
            CodegenError::unsupported(format!("no offset for attribute '{}'", attr_name), span)
        })?;

        // 3. Lower the initializer expression.
        let mut val = lower_expr(ctx, init_expr)?;
        // Box primitive if attribute type is Object.
        val = ensure_boxed(ctx, val, &init_expr.anno, attr_ty)?;

        // 4. Compute the field pointer as an i8*.
        let offset_val = ctx
            .codegen
            .context
            .i64_type()
            .const_int(offset as u64, false);
        let field_ptr = unsafe {
            ctx.codegen
                .builder
                .build_in_bounds_gep(
                    ctx.codegen.context.i8_type(),
                    obj_ptr,
                    &[offset_val],
                    "field_ptr",
                )
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
        };

        // 5. Cast to the attribute's type pointer.
        let target_ptr_type = ctx.codegen.context.ptr_type(Default::default());
        let typed_field_ptr = ctx
            .codegen
            .builder
            .build_pointer_cast(field_ptr, target_ptr_type, "field_typed_ptr")
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

        // 6. Store the value.
        ctx.codegen
            .builder
            .build_store(typed_field_ptr, val)
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

        // 7. If the attribute is heap‑allocated, retain the stored value.
        if crate::lower::utils::is_heap_allocated_type(attr_ty, ctx.registry) {
            ctx.codegen
                .builder
                .build_call(retain_fn, &[val.into()], "retain_attr")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
        }
    }

    // --- 5. Clean up and return ----------------------------------------------

    ctx.pop_scope()?;
    Ok(obj_ptr.into())
}

/// Collects all ancestors of a type (root → immediate parent).
fn collect_ancestors(type_name: &str, registry: &TypeRegistry) -> Vec<String> {
    let mut ancestors = Vec::new();
    let mut current = type_name;
    while let Some(info) = registry.lookup_type(current) {
        if let Some(parent) = &info.parent {
            ancestors.insert(0, parent.name.clone());
            current = &parent.name;
        } else {
            break;
        }
    }
    ancestors
}

/// Collects attribute initializers from a `TypeDecl` in declaration order.
fn collect_attribute_initializers<'a>(
    ty_decl: &'a TypeDecl<Type>,
    out: &mut Vec<(String, &'a hulk_ast::Expr<Type>)>,
) {
    for member in &ty_decl.members {
        if let TypeMemberKind::Attribute(attr) = &member.kind {
            out.push((attr.name.clone(), &attr.initializer));
        }
    }
}
