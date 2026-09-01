//! Lowering of method declarations and definitions.

use hulk_ast::{DeclarationKind, TypeDecl, TypeMemberKind};
use hulk_semantic::{Type, TypeRegistry};
use inkwell::types::{BasicMetadataTypeEnum, BasicType};

use super::lower_expr;
use crate::context::CodegenCtx;
use crate::error::CodegenError;
use crate::lower::{utils, LowerCtx};

/// Declares all methods for all user‑defined types.
///
/// For each type, for each method in its flattened method set, creates an
/// LLVM function with `self` as the first parameter (pointer to the type's struct).
/// The function is stored in `ctx.functions` with a qualified name `"Type::method"`.
pub fn declare_methods(
    ctx: &mut CodegenCtx,
    program: &hulk_ast::Program<Type>,
    registry: &TypeRegistry,
) -> Result<(), CodegenError> {
    for decl in &program.declarations {
        if let DeclarationKind::Type(ty_decl) = &decl.kind {
            declare_methods_for_type(ctx, ty_decl, registry)?;
        }
    }
    Ok(())
}

fn declare_methods_for_type(
    ctx: &mut CodegenCtx,
    ty_decl: &TypeDecl<Type>,
    registry: &TypeRegistry,
) -> Result<(), CodegenError> {
    let type_name = &ty_decl.name;
    let type_info = registry.lookup_type(type_name).ok_or_else(|| {
        CodegenError::llvm_verification(format!("type '{}' not found", type_name))
    })?;

    let methods = &type_info.methods;

    // Get the type layout to know the struct type for `self`.
    let _layout = ctx.type_layouts.get(type_name).ok_or_else(|| {
        CodegenError::llvm_verification(format!("no layout for type '{}'", type_name))
    })?;
    let self_ty = ctx.context.ptr_type(Default::default());

    for (method_name, method_sig) in methods {
        // WHY: canonical OOP codegen pattern — each type only declares LLVM
        // functions for methods it owns. Inherited methods (defined_in != type_name)
        // are resolved via vtable dispatch; their bodies live in the declaring type.
        if method_sig.defined_in != *type_name {
            continue;
        }

        let qualified_name = format!("{}::{}", type_name, method_name);

        // Map parameter types (excluding `self` which is implicit).
        let param_types: Vec<BasicMetadataTypeEnum> = method_sig
            .params
            .iter()
            .map(|(_, ty)| utils::llvm_type(ctx, registry, ty).map(BasicMetadataTypeEnum::from))
            .collect::<Result<_, _>>()?;

        // Return type.
        let return_ty = utils::llvm_type(ctx, registry, &method_sig.return_type)?;

        // Signature: (self: *mut T, ...params) -> return_ty
        let mut all_param_types: Vec<BasicMetadataTypeEnum> = vec![self_ty.into()];
        all_param_types.extend(param_types);
        let fn_type = return_ty.fn_type(&all_param_types, false);
        let fn_value = ctx.module.add_function(&qualified_name, fn_type, None);
        ctx.functions.insert(qualified_name.clone(), fn_value);
    }

    Ok(())
}

/// Defines method bodies for all user‑defined types.
///
/// Must be called after vtables are built, because vtables reference method
/// functions (declarations are sufficient, definitions are not needed for vtables).
pub fn define_methods(
    ctx: &mut CodegenCtx,
    program: &hulk_ast::Program<Type>,
    registry: &TypeRegistry,
) -> Result<(), CodegenError> {
    for decl in &program.declarations {
        if let DeclarationKind::Type(ty_decl) = &decl.kind {
            define_methods_for_type(ctx, ty_decl, registry, program)?;
        }
    }
    Ok(())
}

fn define_methods_for_type(
    ctx: &mut CodegenCtx,
    ty_decl: &TypeDecl<Type>,
    registry: &TypeRegistry,
    program: &hulk_ast::Program<Type>,
) -> Result<(), CodegenError> {
    let type_name = &ty_decl.name;
    let type_info = registry.lookup_type(type_name).ok_or_else(|| {
        CodegenError::llvm_verification(format!("type '{}' not found", type_name))
    })?;

    let methods = &type_info.methods;

    // Get layout for self type.
    let _layout = ctx.type_layouts.get(type_name).ok_or_else(|| {
        CodegenError::llvm_verification(format!("no layout for type '{}'", type_name))
    })?;

    for (method_name, method_sig) in methods {
        // Canonical OOP codegen pattern - each type only defines LLVM functions
        // for methods it owns. Inherited methods (defined_in != type_name) are
        // resolved via vtable dispatch; their bodies live in the declaring type.
        if method_sig.defined_in != *type_name {
            continue;
        }

        let qualified_name = format!("{}::{}", type_name, method_name);
        let fn_value = ctx.functions.get(&qualified_name).cloned().ok_or_else(|| {
            CodegenError::llvm_verification(format!("method '{}' not declared", qualified_name))
        })?;

        // Create entry block.
        let entry_bb = ctx.context.append_basic_block(fn_value, "entry");
        ctx.builder.position_at_end(entry_bb);

        // Create LowerCtx with current_type and current_method for base resolution.
        let mut lower_ctx = LowerCtx::new(ctx, registry, program);
        lower_ctx.current_type = Some(type_name.clone());
        lower_ctx.current_method = Some(method_name.clone());
        lower_ctx.push_scope(); // function scope

        let params = fn_value.get_params();

        // Bind `self` parameter.
        let self_sem_ty = Type::Named(type_name.clone());
        lower_ctx.declare_var("self", params[0], self_sem_ty, true)?;

        // Bind other parameters.
        for (i, (param_name, param_ty)) in method_sig.params.iter().enumerate() {
            let param_value = params
                .get(i + 1) // +1 because params[0] is self
                .ok_or_else(|| {
                    CodegenError::llvm_verification(format!("missing method parameter {}", i))
                })?;
            lower_ctx.declare_var(param_name, *param_value, param_ty.clone(), true)?;
        }

        // Find the corresponding method body in the AST.
        // We need to locate the method declaration in the type.
        let method_body = ty_decl
            .members
            .iter()
            .find_map(|member| {
                if let TypeMemberKind::Method(method) = &member.kind {
                    if method.name == *method_name {
                        Some(&method.body)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                CodegenError::llvm_verification(format!(
                    "method body for '{}' not found",
                    method_name
                ))
            })?;

        // Lower the method body.
        let body_value = lower_expr(&mut lower_ctx, method_body)?;

        lower_ctx.pop_scope()?; // Pop the function's parameter scope.

        // Return.
        lower_ctx
            .codegen
            .builder
            .build_return(Some(&body_value))
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    }

    Ok(())
}
