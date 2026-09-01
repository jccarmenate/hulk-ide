//! Object layout and vtable construction for HULK types.
//!
//! This module builds the runtime representation of HULK classes:
//! - The LLVM struct type for each concrete type, including the object header.
//! - Field offsets for attribute access.
//! - Vtable slot indices for virtual dispatch.
//! - Vtable globals (built after methods are declared).
//! - GC field maps (for the tracing collector).

use std::collections::HashMap;

use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::GlobalValue;

use hulk_semantic::{topological_order, TypeInfo, TypeRegistry};

use crate::context::CodegenCtx;
use crate::error::CodegenError;
use crate::lower::utils::{llvm_type, is_heap_allocated_type, HEADER_FIELD_COUNT};

const METHOD_HEADER_SLOTS: usize = 2;

/// Layout information for a single HULK type.
#[derive(Clone)]
pub struct TypeLayout<'ctx> {
    /// The LLVM struct type representing an instance of this type.
    pub struct_ty: StructType<'ctx>,
    /// Field offsets in bytes: attribute name -> byte offset from the start of the struct.
    pub field_offsets: HashMap<String, usize>,
    /// Method slot indices: method name -> vtable slot index (0‑based).
    pub method_slots: HashMap<String, usize>,
    /// The vtable global (built later, initially None).
    pub vtable_global: Option<GlobalValue<'ctx>>,
    /// Global constant i64 array: [object_size_bytes, offset0, offset1, …, -1 (sentinel)]
    pub field_map_global: Option<GlobalValue<'ctx>>,
    /// The total size of the struct in bytes.
    pub size: usize,
}

impl<'ctx> TypeLayout<'ctx> {
    pub fn new(struct_ty: StructType<'ctx>, size: usize) -> Self {
        Self {
            struct_ty,
            field_offsets: HashMap::new(),
            method_slots: HashMap::new(),
            vtable_global: None,
            field_map_global: None,
            size,
        }
    }
}

/// Builds type layouts for all user‑defined types in the registry.
///
/// This function walks the inheritance hierarchy parent‑before‑child, creates
/// an LLVM struct type for each type, and computes field offsets.
/// It also records method slot indices based on the flattened method order.
///
/// The layouts are stored in `ctx.type_layouts` for later use.
pub fn build_layouts(
    program: &hulk_ast::Program<hulk_semantic::Type>,
    registry: &TypeRegistry,
    ctx: &mut CodegenCtx,
) -> Result<(), CodegenError> {
    // WHY: collect.rs overwrites seeded "Vector"/"Range" entries when the
    // user declares a type with that name. We must not skip user-declared
    // types even if they share a name with a builtin container type.
    let user_declared_types: std::collections::HashSet<&str> = program
        .declarations
        .iter()
        .filter_map(|d| match &d.kind {
            hulk_ast::DeclarationKind::Type(t) => Some(t.name.as_str()),
            _ => None,
        })
        .collect();

    let mut layouts = HashMap::new();

    // Compute topological order of types (parents before children).
    let order = topological_order(registry);

    for type_name in order {
        let info = registry.lookup_type(&type_name).ok_or_else(|| {
            CodegenError::llvm_verification(format!("type '{}' not in registry", type_name))
        })?;

        // Skip builtin value types and other special types that have no user‑defined layout.
        if info.is_builtin_value
            || type_name == "Object"
            || (type_name == "Vector" && !user_declared_types.contains(type_name.as_str()))
            || (type_name == "Range" && !user_declared_types.contains(type_name.as_str()))
            || type_name == "Number"
            || type_name == "String"
            || type_name == "Boolean"
        {
            continue;
        }

        let (struct_ty, field_offsets, size) = build_struct_type(&type_name, info, registry, ctx)?;

        let mut layout = TypeLayout::new(struct_ty, size);
        layout.field_offsets = field_offsets;

        // Record method slots using the flattened method order.
        let methods = if !info.flattened_methods.is_empty() {
            &info.flattened_methods
        } else {
            &info.methods
        };
        for (idx, method_name) in methods.keys().enumerate() {
            // Method dispatch indices are METHOD_HEADER_SLOTS-based
            layout.method_slots.insert(method_name.clone(), idx + METHOD_HEADER_SLOTS);
        }

        layouts.insert(type_name.clone(), layout);
    }

    ctx.type_layouts = layouts;
    Ok(())
}

/// Collects the full inheritance chain from the root down to (but excluding) the given type.
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

/// Builds the LLVM struct type for a single type, including the object header,
/// all inherited fields, and own attributes. Returns the struct type, a mapping
/// from attribute name to byte offset, and the total size.
fn build_struct_type<'ctx>(
    type_name: &str,
    info: &TypeInfo,
    registry: &TypeRegistry,
    ctx: &CodegenCtx<'ctx>,
) -> Result<(StructType<'ctx>, HashMap<String, usize>, usize), CodegenError> {
    let context = ctx.context;
    let data_layout = ctx.target_machine.get_target_data();

    // ─── 1. Build a flat list of (attribute_name, llvm_type) in inheritance order ──

    let mut attr_names = Vec::new();
    let mut attr_tys = Vec::new();

    // WHY: hierarchy.rs flattens all parent attributes into each descendant's
    // TypeInfo. Without deduplication, build_struct_type adds "val" once per
    // ancestor level, so field_offsets["val"] ends up pointing to the last
    // (deepest) duplicate instead of the root ancestor's offset at 32.
    // This tracks which attribute names have already been added so each
    // attribute appears exactly once, at the offset determined by the first
    // ancestor that declares it (root-first order).
    let mut seen = std::collections::HashSet::new();

    // Helper to add attributes from a given type.
    fn add_attributes_from_type<'a>(
        type_info: &TypeInfo,
        attr_names: &mut Vec<String>,
        attr_tys: &mut Vec<BasicTypeEnum<'a>>,
        seen: &mut std::collections::HashSet<String>,
        ctx: &CodegenCtx<'a>,
        registry: &TypeRegistry,
    ) -> Result<(), CodegenError> {
        for (name, attr) in &type_info.attributes {
            if !seen.insert(name.clone()) {
                continue;
            }
            let ty = attr.declared_type.as_ref().ok_or_else(|| {
                CodegenError::llvm_verification(format!(
                    "attribute '{}' has no declared type",
                    name
                ))
            })?;
            let llvm_ty = llvm_type(ctx, registry, ty)?;
            attr_names.push(name.clone());
            attr_tys.push(llvm_ty);
        }
        Ok(())
    }

    // Collect all ancestors (root → immediate parent).
    let ancestors = collect_ancestors(type_name, registry);
    for ancestor_name in ancestors {
        let ancestor_info = registry.lookup_type(&ancestor_name).ok_or_else(|| {
            CodegenError::llvm_verification(format!("ancestor '{}' not found", ancestor_name))
        })?;
        add_attributes_from_type(
            ancestor_info,
            &mut attr_names,
            &mut attr_tys,
            &mut seen,
            ctx,
            registry,
        )?;
    }

    // Add own attributes.
    add_attributes_from_type(
        info,
        &mut attr_names,
        &mut attr_tys,
        &mut seen,
        ctx,
        registry,
    )?;

    // ─── 2. Build the struct type ──────────────────────────────────────────

    // Header fields: ref_count (i64), gc_mark (i1), next (ptr), vtable (ptr)
    let i64_type = context.i64_type();
    let i1_type = context.bool_type(); // gc_mark in memory
    let ptr_type = context.ptr_type(Default::default());

    let i8_type = context.i8_type();
    let mut field_tys = vec![
        i64_type.into(), // ref_count
        i1_type.into(),  // gc_mark
        i8_type.into(),  // type_tag
        ptr_type.into(), // prev
        ptr_type.into(), // next
        ptr_type.into(), // vtable
    ];

    // Append all attribute types.
    field_tys.extend(attr_tys);

    let struct_ty = context.struct_type(&field_tys, false);

    // ─── 3. Compute offsets for each attribute ─────────────────────────────

    let mut field_offsets = HashMap::new();
    // Header fields have indices 0..HEADER_FIELD_COUNT-1; attributes start at index HEADER_FIELD_COUNT.
    for (idx, name) in attr_names.iter().enumerate() {
        let field_idx = HEADER_FIELD_COUNT + idx;
        let offset = data_layout
            .offset_of_element(&struct_ty, field_idx as u32)
            .ok_or_else(|| {
                CodegenError::llvm_verification(format!("offset computation failed for '{}'", name))
            })?;
        field_offsets.insert(name.clone(), offset as usize);
    }

    // ─── 4. Compute total size ─────────────────────────────────────────────

    // Total allocation size including end-padding for array placement
    let size = data_layout.get_abi_size(&struct_ty) as usize;

    Ok((struct_ty, field_offsets, size))
}

/// Builds vtable globals for all types.
///
/// Must be called after all methods have been declared, because vtables
/// reference the method function declarations stored in `ctx.functions`.
pub fn build_vtables<'ctx>(
    ctx: &mut CodegenCtx<'ctx>,
    registry: &TypeRegistry,
) -> Result<(), CodegenError> {
    let type_names: Vec<String> = ctx.type_layouts.keys().cloned().collect();
    let ptr_type = ctx.context.ptr_type(Default::default());

    for type_name in type_names {
        let info = registry.lookup_type(&type_name).ok_or_else(|| {
            CodegenError::llvm_verification(format!("type '{}' not found", type_name))
        })?;

        let methods = if !info.flattened_methods.is_empty() {
            &info.flattened_methods
        } else {
            &info.methods
        };

        // ── Slot 0: pointer to the GC field map ──────────────────────────
        // Mark phase dereferences vtable[0] for pointer-field offsets and object size. 
        // Must come before build_gc_field_maps is called.
        let field_map_ptr = ctx
            .type_layouts
            .get(&type_name)
            .and_then(|l| l.field_map_global)
            .map(|g| g.as_pointer_value())
            .ok_or_else(|| {
                CodegenError::llvm_verification(format!(
                    "no field map for '{}' — call build_gc_field_maps first",
                    type_name
                ))
            })?;

        // ── Slot 1: parent vtable pointer ────────────────────────────────
        // hulk_rt_downcast_check walks vtable[1] to ascend the ancestor chain.
        // Null terminates the walk (root types have no parent vtable).
        let parent_vtable_ptr = info
            .parent
            .as_ref()
            .and_then(|p| ctx.type_layouts.get(&p.name))
            .and_then(|l| l.vtable_global)
            .map(|g| g.as_pointer_value())
            .unwrap_or_else(|| ptr_type.const_null());

        let mut fn_ptrs: Vec<inkwell::values::PointerValue<'ctx>> =
            vec![field_map_ptr, parent_vtable_ptr]; // slots 0 and 1

        // ── Slots 2…N+1: method function pointers ────────────────────────
        for method_name in methods.keys() {
            let owner =
                owning_type_for_method(&type_name, method_name, registry).ok_or_else(|| {
                    CodegenError::llvm_verification(format!(
                        "method '{}' has no declaring type in the ancestor chain of '{}'",
                        method_name, type_name
                    ))
                })?;
            let qualified_name = format!("{}::{}", owner, method_name);
            let fn_value = ctx.functions.get(&qualified_name).cloned().ok_or_else(|| {
                CodegenError::llvm_verification(format!("method '{}' not declared", qualified_name))
            })?;
            let fn_ptr = fn_value.as_global_value().as_pointer_value();
            fn_ptrs.push(fn_ptr);
        }

        let vtable_type = ptr_type.array_type(fn_ptrs.len() as u32);
        let vtable_global =
            ctx.module
                .add_global(vtable_type, None, &format!("{}__vtable", type_name));
        let const_array = ptr_type.const_array(&fn_ptrs);
        vtable_global.set_initializer(&const_array);
        vtable_global.set_constant(true);

        if let Some(layout) = ctx.type_layouts.get_mut(&type_name) {
            layout.vtable_global = Some(vtable_global);
        }
    }
    Ok(())
}

/// Returns the name of the type that actually declares or overrides
/// `method_name`, searching `type_name` and then its ancestors in order.
pub fn owning_type_for_method(
    type_name: &str,
    method_name: &str,
    registry: &TypeRegistry,
) -> Option<String> {
    // WHY: TypeInfo.methods is the merged/flattened set (all ancestor methods
    // are copied into each descendant by the hierarchy pass). contains_key always
    // returns true for type_name itself. We need defined_in to find the actual
    // declaring type.
    let mut current = type_name.to_string();
    loop {
        if registry
            .types
            .get(&current)
            .and_then(|info| info.methods.get(method_name))
            .is_some_and(|sig| sig.defined_in == current)
        {
            return Some(current.to_string());
        }
        let parent = registry
            .lookup_type(&current)?
            .parent
            .as_ref()?
            .name
            .clone();
        current = parent;
    }
}

// Determines if a given type has any subtypes in the current compilation unit
pub fn has_subtypes(type_name: &str, registry: &TypeRegistry) -> bool {
    registry
        .types
        .values()
        .any(|info| info.parent.as_ref().is_some_and(|p| p.name == type_name))
}

/// Emits a per-type GC field map for every user-defined type in `ctx.type_layouts`.
///
/// Array layout:  [size_bytes, ptr_offset_0, …, ptr_offset_N-1, -1]
///
/// Must be called before `build_vtables` so that vtable construction
/// can embed a pointer to each type's map at vtable slot 0.
pub fn build_gc_field_maps<'ctx>(
    ctx: &mut CodegenCtx<'ctx>,
    registry: &TypeRegistry,
) -> Result<(), CodegenError> {
    let i64_type = ctx.context.i64_type();

    // Collect type names first to avoid borrowing ctx mutably and immutably.
    let type_names: Vec<String> = ctx.type_layouts.keys().cloned().collect();

    for type_name in type_names {
        // ── Collect byte offsets of pointer-typed fields ──────────────────
        // A field is a GC pointer if its semantic type is heap-allocated
        // (String, Object, Named, Vector, Iterable — anything that becomes a
        // `ptr` in LLVM IR and is reference-counted).
        let (obj_size, pointer_offsets): (u64, Vec<u64>) = {
            let layout = &ctx.type_layouts[&type_name];
            registry.lookup_type(&type_name).ok_or_else(|| {
                CodegenError::llvm_verification(format!(
                    "type '{}' not found in registry during field-map build",
                    type_name
                ))
            })?;

            let mut offsets = Vec::new();
            for (attr_name, &byte_offset) in &layout.field_offsets {
                // Resolve the attribute's semantic type through the registry,
                // walking the ancestor chain (flattened attributes live in the
                // TypeInfo but the declaring ancestor holds the canonical type).
                let attr_ty = find_attribute_type(&type_name, attr_name, registry);
                if let Some(ty) = attr_ty {
                    if is_heap_allocated_type(&ty, registry) {
                        offsets.push(byte_offset as u64);
                    }
                }
            }
            offsets.sort_unstable(); // deterministic order for reproducible IR
            (layout.size as u64, offsets)
        };

        // ── Build the constant array: [size, offsets…, -1] ───────────────
        // field_map[0] is the object size so hulk_rt_release can find the 
        // deallocation Layout for TAG_OBJECT without a separate vtable slot.
        // The mark phase skips field_map[0] and reads from index 1.
        let mut values: Vec<inkwell::values::IntValue<'ctx>> =
            vec![i64_type.const_int(obj_size, false)]; // [0] = size
        values.extend(
            pointer_offsets
                .iter()
                .map(|&off| i64_type.const_int(off, false)),
        );
        // -1 sentinel: u64::MAX interpreted as two's-complement i64(-1).
        values.push(i64_type.const_int(u64::MAX, false));

        let array_ty = i64_type.array_type(values.len() as u32);
        let global_name = format!("{}__gc_field_map", type_name);
        let global = ctx.module.add_global(array_ty, None, &global_name);
        let const_array = i64_type.const_array(&values);
        global.set_initializer(&const_array);
        global.set_constant(true);

        ctx.type_layouts
            .get_mut(&type_name)
            .unwrap()
            .field_map_global = Some(global);
    }

    Ok(())
}

/// Searches `type_name` and its ancestor chain for the semantic type of
/// `attr_name`. Returns `None` only if the attribute is not found
/// (which should never happen for a semantically valid program).
fn find_attribute_type(
    type_name: &str,
    attr_name: &str,
    registry: &TypeRegistry,
) -> Option<hulk_semantic::Type> {
    let mut current = type_name.to_string();
    loop {
        let info = registry.lookup_type(&current)?;
        if let Some(attr) = info.attributes.get(attr_name) {
            return attr.declared_type.clone();
        }
        current = info.parent.as_ref()?.name.clone();
    }
}

#[cfg(test)]
mod tests {
    // TODO: Add unit tests for layout building.
}
