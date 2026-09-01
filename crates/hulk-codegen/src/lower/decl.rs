//! Lowering of function declarations and definitions.

use hulk_ast::{DeclarationKind, FunctionDecl, Program};
use hulk_semantic::{Type, TypeRegistry};
use inkwell::types::{BasicMetadataTypeEnum, BasicType};

use super::lower_expr;
use crate::context::CodegenCtx;
use crate::error::CodegenError;
use crate::lower::{utils, LowerCtx};

/// Declares all user‑defined functions in the module.
///
/// This pass collects every function signature and creates an LLVM function
/// declaration, using the resolved types from the registry. Declarations
/// are stored in `CodegenCtx::functions` for later use in definitions and calls.
pub fn declare_functions(
    ctx: &mut CodegenCtx,
    program: &Program<Type>,
    registry: &TypeRegistry,
) -> Result<(), CodegenError> {
    for decl in &program.declarations {
        if let DeclarationKind::Function(func) = &decl.kind {
            declare_function(ctx, func, registry)?;
        }
    }
    Ok(())
}

/// Declares a single function by looking up its signature in the registry.
fn declare_function(
    ctx: &mut CodegenCtx,
    func: &FunctionDecl<Type>,
    registry: &TypeRegistry,
) -> Result<(), CodegenError> {
    let sig = registry.lookup_function(&func.name).ok_or_else(|| {
        CodegenError::llvm_verification(format!("function '{}' not in registry", func.name))
    })?;

    // Map parameter types from the registry signature.
    let param_types: Vec<_> = sig
        .params
        .iter()
        .map(|(_, ty)| utils::llvm_type(ctx, registry, ty))
        .collect::<Result<_, _>>()?;

    let return_ty = utils::llvm_type(ctx, registry, &sig.return_type)?;

    // Convert to `BasicMetadataTypeEnum` for `fn_type`.
    let param_metadata: Vec<BasicMetadataTypeEnum> =
        param_types.into_iter().map(|t| t.into()).collect();

    let fn_type = return_ty.fn_type(&param_metadata, false);
    let fn_value = ctx.module.add_function(&func.name, fn_type, None);
    ctx.functions.insert(func.name.clone(), fn_value);
    Ok(())
}

/// Defines all user‑defined functions by lowering their bodies.
///
/// Must be called after `declare_functions`. Each function body is lowered
/// with a fresh `LowerCtx` that contains the function's parameters as local variables.
pub fn define_functions(
    ctx: &mut CodegenCtx,
    program: &Program<Type>,
    registry: &TypeRegistry,
) -> Result<(), CodegenError> {
    for decl in &program.declarations {
        if let DeclarationKind::Function(func) = &decl.kind {
            define_function(ctx, func, registry, program)?;
        }
    }
    Ok(())
}

/// Defines a single function body.
fn define_function(
    ctx: &mut CodegenCtx,
    func: &FunctionDecl<Type>,
    registry: &TypeRegistry,
    program: &Program<Type>,
) -> Result<(), CodegenError> {
    let fn_value = ctx.functions.get(&func.name).cloned().ok_or_else(|| {
        CodegenError::llvm_verification(format!("function '{}' not declared", func.name))
    })?;

    let entry_bb = ctx.context.append_basic_block(fn_value, "entry");
    ctx.builder.position_at_end(entry_bb);

    // Create a fresh LowerCtx for this function body.
    let mut lower_ctx = LowerCtx::new(ctx, registry, program);
    // Push a scope for the function's parameters and locals.
    lower_ctx.push_scope();

    // Retrieve the function signature from the registry to get parameter types.
    let sig = registry.lookup_function(&func.name).ok_or_else(|| {
        CodegenError::llvm_verification(format!("function '{}' not in registry", func.name))
    })?;

    // Collect all parameter values.
    let param_values = fn_value.get_params();

    // Bind each parameter: create an alloca, store the incoming value, and declare in the scope.
    for (i, (param_name, param_ty)) in sig.params.iter().enumerate() {
        let param_value = param_values
            .get(i)
            .ok_or_else(|| CodegenError::llvm_verification(format!("missing parameter {}", i)))?;
        
        // Handles alloca, store, shadow-push (for pointer types), and marks the binding as borrowed so
        // pop_scope does not release the caller's reference.
        lower_ctx.declare_var(param_name, *param_value, param_ty.clone(), true)?;
    }

    // Lower the function body.
    let body_value = lower_expr(&mut lower_ctx, &func.body)?;

    lower_ctx.pop_scope()?; // Pop the function's parameter scope.

    // Return the body value.
    lower_ctx
        .codegen
        .builder
        .build_return(Some(&body_value))
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    Ok(())
}
