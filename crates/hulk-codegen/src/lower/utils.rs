//! Shared helper functions and constants used by multiple lowering submodules.

use inkwell::FloatPredicate;
use inkwell::values::BasicValueEnum;
use hulk_ast::{TypeRef, SourceSpan};
use hulk_semantic::{Type, TypeRegistry};
pub use hulk_rt::{TAG_BOOLEAN, TAG_BOX, TAG_NUMBER, HEADER_FIELD_COUNT, BOX_HEADER_SIZE};

use crate::error::CodegenError;
use crate::lower::LowerCtx;
use crate::runtime_decls;
use crate::CodegenCtx;

// ===================================================================================
// Header layout for all heap-allocated HULK objects.
// Matches `hulk_rt::ObjHeader` exactly.
// ===================================================================================

/// Field indices into the LLVM struct type of the object header.
pub mod field_indices {
    pub const REF_COUNT: u32 = 0;
    pub const GC_MARK: u32 = 1;
    pub const TYPE_TAG: u32 = 2;
    pub const PREV: u32 = 3;
    pub const NEXT: u32 = 4;
    pub const VTABLE: u32 = 5;
}

// Header (40) + Original_tag (1) + Padding (7) + Payload (8)
const BOX_SIZE: u64 = BOX_HEADER_SIZE + 1 + 7 + 8;
// The offset of the payload portion of a boxed object, in bytes.
const PAYLOAD_OFFSET: u64 = BOX_HEADER_SIZE + 8; // 48

// ====================================================================================
// Shared helper functions
// ====================================================================================

/// Compares two floating-point values.
pub fn cmp_float<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    pred: FloatPredicate,
    left: inkwell::values::BasicValueEnum<'ctx>,
    right: inkwell::values::BasicValueEnum<'ctx>,
    name: &str,
) -> Result<inkwell::values::BasicValueEnum<'ctx>, CodegenError> {
    let lf = left.into_float_value();
    let rf = right.into_float_value();
    let cmp = ctx
        .codegen
        .builder
        .build_float_compare(pred, lf, rf, name)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    Ok(cmp.into())
}

/// Converts a value to a string pointer.
pub fn to_string<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    val: inkwell::values::BasicValueEnum<'ctx>,
    ty: &Type,
) -> Result<inkwell::values::PointerValue<'ctx>, CodegenError> {
    match ty {
        Type::String => Ok(val.into_pointer_value()),
        Type::Number => {
            let f = val.into_float_value();
            let fn_val = runtime_decls::declare_number_to_string(ctx.codegen);
            let call = ctx
                .codegen
                .builder
                .build_call(fn_val, &[f.into()], "num_to_str")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
                .try_as_basic_value()
                .unwrap_basic();
            Ok(call.into_pointer_value())
        }
        Type::Boolean => {
            let b = val.into_int_value();
            let fn_val = runtime_decls::declare_bool_to_string(ctx.codegen);
            let call = ctx
                .codegen
                .builder
                .build_call(fn_val, &[b.into()], "bool_to_str")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
                .try_as_basic_value()
                .unwrap_basic();
            Ok(call.into_pointer_value())
        }
        _ => Err(CodegenError::unsupported(
            format!("cannot convert type {} to string", ty),
            None,
        )),
    }
}

/// Maps a HULK type to an LLVM type.
pub fn llvm_type<'ctx>(
    codegen: &CodegenCtx<'ctx>,
    registry: &TypeRegistry,
    ty: &Type,
) -> Result<inkwell::types::BasicTypeEnum<'ctx>, CodegenError> {
    match ty {
        Type::Number => Ok(codegen.context.f64_type().into()),
        Type::Boolean => Ok(codegen.context.bool_type().into()),
        Type::String | Type::Object | Type::Vector(_) => {
            Ok(codegen.context.ptr_type(Default::default()).into())
        }
        // Protocols and Iterable(T) share the same fat-pointer representation (§6.6).
        Type::Function { .. } | Type::Iterable(_) => {
            let ptr_type = codegen.context.ptr_type(Default::default());
            Ok(codegen
                .context
                .struct_type(&[ptr_type.into(), ptr_type.into()], false)
                .into())
        }
        Type::Named(_) if registry.is_protocol(ty) => {
            let ptr_type = codegen.context.ptr_type(Default::default());
            Ok(codegen
                .context
                .struct_type(&[ptr_type.into(), ptr_type.into()], false)
                .into())
        }
        Type::Named(_) => Ok(codegen.context.ptr_type(Default::default()).into()),
        _ => Err(CodegenError::unsupported(
            format!("type {} not supported", ty),
            None,
        )),
    }
}

/// Returns the type and byte offset of an attribute, or an error.
pub fn resolve_attribute_with_offset(
    ctx: &LowerCtx<'_, '_>,
    obj_type: &Type,
    member_name: &str,
) -> Result<(Type, usize), CodegenError> {
    let type_name = match obj_type {
        Type::Named(name) => name,
        _ => {
            return Err(CodegenError::unsupported(
                format!("attribute access on non‑named type: {:?}", obj_type),
                None,
            ));
        }
    };
    let info = ctx.registry.lookup_type(type_name).ok_or_else(|| {
        CodegenError::unsupported(format!("type '{}' not found", type_name), None)
    })?;
    let attr_info = info.attributes.get(member_name).ok_or_else(|| {
        CodegenError::unsupported(format!("attribute '{}' not found", member_name), None)
    })?;
    let attr_type = attr_info
        .declared_type
        .as_ref()
        .ok_or_else(|| {
            CodegenError::llvm_verification(format!(
                "attribute '{}' has no declared type",
                member_name
            ))
        })?
        .clone();
    let layout = ctx.codegen.type_layouts.get(type_name).ok_or_else(|| {
        CodegenError::unsupported(format!("no layout for type '{}'", type_name), None)
    })?;
    let offset = *layout.field_offsets.get(member_name).ok_or_else(|| {
        CodegenError::unsupported(format!("no offset for attribute '{}'", member_name), None)
    })?;
    Ok((attr_type, offset))
}

/// Returns `true` if values of `ty` are stored as heap pointers and must be
/// reference‑counted.
pub fn is_heap_allocated_type(ty: &Type, _registry: &TypeRegistry) -> bool {
    matches!(
        ty,
        Type::String | Type::Object | Type::Vector(_) | Type::Iterable(_) | Type::Named(_) | Type::Function { .. },
    )
}

/// Converts a concrete object pointer to a protocol fat pointer.
///
/// The fat pointer is a struct `{ data: ptr, itable: ptr }`.
///
/// # Parameters
/// - `ctx`: The lowering context.
/// - `value`: The concrete object pointer.
/// - `concrete_ty`: The static type of the concrete object.
/// - `protocol_ty`: The target protocol type.
///
/// # Returns
/// A `BasicValueEnum` containing the fat pointer struct.
pub fn convert_to_protocol<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    value: BasicValueEnum<'ctx>,
    concrete_ty: &Type,
    protocol_ty: &Type,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    let obj_ptr = value.into_pointer_value();

    let protocol_name = match protocol_ty {
        Type::Named(name) => name,
        Type::Iterable(_) => "Iterable",
        _ => {
            return Err(CodegenError::unsupported(
                format!("cannot convert to protocol type: {}", protocol_ty),
                None,
            ));
        }
    };

    let concrete_name = match concrete_ty {
        Type::Named(name) => name,
        _ => {
            return Err(CodegenError::unsupported(
                format!("cannot convert from non-named type: {}", concrete_ty),
                None,
            ));
        }
    };

    // Look up the itable.
    let itable_global = ctx
        .codegen
        .itables
        .get(&(concrete_name.clone(), (*protocol_name).to_string()))
        .ok_or_else(|| {
            CodegenError::unsupported(
                format!(
                    "itable not found for ({}, {})",
                    concrete_name, protocol_name
                ),
                None,
            )
        })?;
    let itable_ptr = itable_global.as_pointer_value();

    // Build the fat pointer struct.
    let ptr_type = ctx.codegen.context.ptr_type(Default::default());
    let fat_ptr_ty = ctx
        .codegen
        .context
        .struct_type(&[ptr_type.into(), ptr_type.into()], false);
    let fat_ptr_alloca = ctx
        .codegen
        .builder
        .build_alloca(fat_ptr_ty, "fat_ptr")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    let zero = ctx.codegen.context.i32_type().const_int(0, false);
    let one = ctx.codegen.context.i32_type().const_int(1, false);

    // Store data pointer at index 0.
    let data_ptr_ptr = unsafe {
        ctx.codegen
            .builder
            .build_gep(fat_ptr_ty, fat_ptr_alloca, &[zero, zero], "data_ptr_ptr")
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
    };
    ctx.codegen
        .builder
        .build_store(data_ptr_ptr, obj_ptr)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // Store itable pointer at index 1.
    let itable_ptr_ptr = unsafe {
        ctx.codegen
            .builder
            .build_gep(fat_ptr_ty, fat_ptr_alloca, &[zero, one], "itable_ptr_ptr")
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
    };
    ctx.codegen
        .builder
        .build_store(itable_ptr_ptr, itable_ptr)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // Load the fat pointer.
    let fat_ptr = ctx
        .codegen
        .builder
        .build_load(fat_ptr_ty, fat_ptr_alloca, "fat_ptr_val")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    Ok(fat_ptr)
}

/// Attempts to resolve a syntactic `TypeRef` to a semantic `Type` using the registry.
///
/// This is a simplified version of the resolver used in the inference pass.
/// It only needs to determine if the type is a protocol, so it handles
/// builtins and user‑defined types by name.
pub fn resolve_type_ref_to_type(tr: &TypeRef, registry: &TypeRegistry) -> Type {
    match tr.name.as_str() {
        "Number" => Type::Number,
        "String" => Type::String,
        "Boolean" => Type::Boolean,
        "Object" => Type::Object,
        "Vector" if !tr.args.is_empty() => {
            // We don't need the inner type for protocol detection.
            Type::Vector(Box::new(Type::Unknown))
        }
        "Iterable" if !tr.args.is_empty() => Type::Iterable(Box::new(Type::Unknown)),
        "Function" if !tr.args.is_empty() => {
            let mut params = Vec::new();
            for arg in &tr.args[..tr.args.len() - 1] {
                params.push(resolve_type_ref_to_type(arg, registry));
            }
            let return_type = resolve_type_ref_to_type(
                tr.args.last().expect("function type has return type"),
                registry,
            );
            Type::Function {
                params,
                return_type: Box::new(return_type),
            }
        }
        name => {
            // It could be a user‑defined type or protocol.
            if registry.types.contains_key(name) || registry.protocols.contains_key(name) {
                Type::Named(name.to_string())
            } else {
                Type::Unknown
            }
        }
    }
}

/// Boxes a primitive value into a `HulkBox` heap object.
///
/// # Parameters
/// - `ctx`: The lowering context.
/// - `value`: The primitive value to box.
/// - `ty`: The semantic type of the value (must be `Number` or `Boolean`).
///
/// # Returns
/// A pointer to the newly allocated box.
pub fn box_primitive<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    value: BasicValueEnum<'ctx>,
    ty: &Type,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    let alloc_fn = ctx
        .codegen
        .functions
        .get("hulk_rt_alloc")
        .cloned()
        .ok_or_else(|| CodegenError::unsupported("hulk_rt_alloc not declared", None))?;

    let i64_type = ctx.codegen.context.i64_type();
    let i8_type = ctx.codegen.context.i8_type();
    let ptr_type = ctx.codegen.context.ptr_type(Default::default());

    let size = i64_type.const_int(BOX_SIZE, false);
    let call = ctx
        .codegen
        .builder
        .build_call(alloc_fn, &[size.into()], "box_alloc")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    let box_ptr = call
        .try_as_basic_value()
        .basic()
        .unwrap()
        .into_pointer_value();

    // Byte‑offset GEP helper.
    let byte_ptr = |offset: u64, name: &str| unsafe {
        ctx.codegen
            .builder
            .build_gep(i8_type, box_ptr, &[i64_type.const_int(offset, false)], name)
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))
    };

    // ── Header ──────────────────────────────────────────────────────
    // ref_count = 1
    ctx.codegen
        .builder
        .build_store(byte_ptr(0, "ref_count_ptr")?, i64_type.const_int(1, false))
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    // gc_mark = false
    ctx.codegen
        .builder
        .build_store(byte_ptr(8, "gc_mark_ptr")?, i8_type.const_int(0, false))
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    // type_tag = TAG_BOX
    ctx.codegen
        .builder
        .build_store(
            byte_ptr(9, "box_type_tag_ptr")?,
            i8_type.const_int(TAG_BOX as u64, false),
        )
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    // prev and next are already assigned by the runtime allocator, so we don't need to set them here.
    // vtable = null
    ctx.codegen
        .builder
        .build_store(byte_ptr(32, "vtable_ptr")?, ptr_type.const_null())
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // Store tag
    let original_tag = match ty {
        Type::Number => TAG_NUMBER,
        Type::Boolean => TAG_BOOLEAN,
        _ => {
            return Err(CodegenError::unsupported(
                format!("boxing of type {} not supported", ty),
                None,
            ))
        }
    };
    ctx.codegen
        .builder
        .build_store(
            byte_ptr(BOX_HEADER_SIZE, "original_tag_ptr")?,
            i8_type.const_int(original_tag as u64, false),
        )
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // ─── payload (offset) ────────────────────────────────────────
    let payload_ptr = byte_ptr(PAYLOAD_OFFSET, "payload_ptr")?;
    let payload_typed_ptr = ctx
        .codegen
        .builder
        .build_pointer_cast(payload_ptr, ptr_type, "payload_typed_ptr")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    match ty {
        Type::Number => {
            ctx.codegen
                .builder
                .build_store(payload_typed_ptr, value.into_float_value())
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
        }
        Type::Boolean => {
            let ext = ctx
                .codegen
                .builder
                .build_int_z_extend(value.into_int_value(), i64_type, "bool_ext")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            ctx.codegen
                .builder
                .build_store(payload_typed_ptr, ext)
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
        }
        _ => unreachable!(),
    }

    Ok(box_ptr.into())
}

/// Ensures a value is boxed if the target type is a pointer type (`Object` or `Vector` element).
///
/// This is the single entry point for boxing at dynamic boundaries.
/// It delegates to `box_primitive` when boxing is required.
pub fn ensure_boxed<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    value: BasicValueEnum<'ctx>,
    src_ty: &Type,
    target_ty: &Type,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    if matches!(target_ty, Type::Object) && matches!(src_ty, Type::Number | Type::Boolean) {
        return box_primitive(ctx, value, src_ty);
    }
    Ok(value)
}

/// Converts a boxed primitive pointer back to a raw value.
/// If `ty` is not Number or Boolean, returns the pointer unchanged.
/// 
/// # Errors
/// Returns `CodegenError::Internal` for LLVM builder failures.
pub fn ensure_unboxed<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    boxed_ptr: inkwell::values::BasicValueEnum<'ctx>,
    ty: &Type,
    span: Option<SourceSpan>
) -> Result<inkwell::values::BasicValueEnum<'ctx>, CodegenError> {
    // If the static type is already a pointer type, no unboxing needed.
    if !matches!(ty, Type::Number | Type::Boolean) ||
        // If boxed_ptr is not a ptr, return without unboxing.
        !boxed_ptr.is_pointer_value()
    {
        return Ok(boxed_ptr);
    }

    let ptr = boxed_ptr.into_pointer_value();

    let current_block = ctx
        .codegen
        .builder
        .get_insert_block()
        .ok_or_else(|| CodegenError::internal("no active insertion block while unboxing", span))?;
    let parent_fn = current_block
        .get_parent()
        .ok_or_else(|| CodegenError::internal("unboxing outside of a function", span))?;

    let null_bb = ctx.codegen.context.append_basic_block(parent_fn, "unbox_null");
    let cont_bb = ctx.codegen.context.append_basic_block(parent_fn, "unbox_cont");
    let merge_bb = ctx.codegen.context.append_basic_block(parent_fn, "unbox_merge");

    let result_ty: inkwell::types::BasicTypeEnum<'ctx> = match ty {
        Type::Number => ctx.codegen.context.f64_type().into(),
        Type::Boolean => ctx.codegen.context.bool_type().into(),
        _ => unreachable!(),
    };
    let result_alloca = ctx
        .codegen
        .builder
        .build_alloca(result_ty, "unbox_result")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    let is_null = ctx
        .codegen
        .builder
        .build_is_null(ptr, "is_null")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    ctx.codegen
        .builder
        .build_conditional_branch(is_null, null_bb, cont_bb)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // Null means the runtime handed an uninitialized vector slot or another invalid object reference.
    ctx.codegen.builder.position_at_end(null_bb);
    let fail_fn = runtime_decls::ensure_decl(ctx.codegen, "hulk_rt_internal_error")?;
    ctx.codegen
        .builder
        .build_call(fail_fn, &[], "internal_error")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    ctx.codegen
        .builder
        .build_unreachable()
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    let i8_type = ctx.codegen.context.i8_type();
    let i64_type = ctx.codegen.context.i64_type();
    let ptr_type = ctx.codegen.context.ptr_type(Default::default());

    // Compute payload address only on the non-null path.
    ctx.codegen.builder.position_at_end(cont_bb);
    let payload_ptr_i8 = unsafe {
        ctx.codegen
            .builder
            .build_gep(
                i8_type,
                ptr,
                &[i64_type.const_int(PAYLOAD_OFFSET, false)],
                "payload_ptr",
            )
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
    };

    let payload_typed_ptr = ctx
        .codegen
        .builder
        .build_pointer_cast(payload_ptr_i8, ptr_type, "payload_typed")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    match ty {
        Type::Number => {
            let val = ctx
                .codegen
                .builder
                .build_load(
                    ctx.codegen.context.f64_type(),
                    payload_typed_ptr,
                    "unbox_num",
                )
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            ctx.codegen
                .builder
                .build_store(result_alloca, val)
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
        }
        Type::Boolean => {
            let val = ctx
                .codegen
                .builder
                .build_load(i64_type, payload_typed_ptr, "unbox_bool")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            let truncated = ctx
                .codegen
                .builder
                .build_int_truncate(
                    val.into_int_value(),
                    ctx.codegen.context.bool_type(),
                    "bool_trunc",
                )
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            ctx.codegen
                .builder
                .build_store(result_alloca, truncated)
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
        }
        _ => unreachable!(),
    }

    ctx.codegen
        .builder
        .build_unconditional_branch(merge_bb)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    ctx.codegen.builder.position_at_end(merge_bb);
    let result = ctx.codegen
        .builder
        .build_load(result_ty, result_alloca, "unbox_result_load")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    Ok(result)
}

// Returns `true` if the type is a protocol or an Iterable, which are both represented as fat pointers.
pub fn is_protocol_or_iterable(ty: &Type, registry: &TypeRegistry) -> bool {
    match ty {
        Type::Named(_) => registry.is_protocol(ty),
        Type::Iterable(_) => true,
        _ => false,
    }
}

/// If `val` is a fat‑pointer struct (protocol or Iterable), extract its
/// first field (the object data pointer). Otherwise, return `val` unchanged.
pub fn object_pointer_from_fat_ptr<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    val: BasicValueEnum<'ctx>,
    ty: &Type,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    if is_protocol_or_iterable(ty, ctx.registry) || matches!(ty, Type::Function { .. }) {
        let struct_val = val.into_struct_value();
        let data_ptr = ctx
            .codegen
            .builder
            .build_extract_value(struct_val, 0, "fat_data")
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
        Ok(data_ptr)
    } else {
        Ok(val)
    }
}
