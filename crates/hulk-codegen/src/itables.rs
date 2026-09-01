//! Interface tables (itables) for protocol dispatch.
//!
//! For every pair (Type, Protocol) where the type structurally implements the
//! protocol and the protocol is actually used in the program, we emit a global
//! constant array containing one function pointer for each protocol method,
//! in the protocol's declared (flattened) order.

use std::collections::HashSet;

use hulk_ast::{DeclarationKind, Expr, ExprKind, Program, SourceSpan, TypeMemberKind, VectorExpr};
use hulk_semantic::{Type, TypeRegistry};

use crate::context::CodegenCtx;
use crate::error::CodegenError;
use crate::lower::utils::resolve_type_ref_to_type;

/// A pair `(type_name, protocol_name)` that requires an itable.
type ItablePair = (String, String);

/// Builds itable globals for every used protocol that is implemented by at least one type.
///
/// # Arguments
/// * `ctx` – The codegen context (mutated to store the itable globals).
/// * `registry` – The type registry (read‑only).
/// * `typed_program` – The fully typed AST, used to determine which protocols are actually used.
///
/// # Guarantees
/// - Only protocols that appear as a static type somewhere in the program are considered.
/// - For each such protocol, we emit an itable for every type that `implements_protocol`.
/// - Builtin types (`Vector`, `Range`) are handled via their runtime functions.
pub fn build_itables(
    ctx: &mut CodegenCtx,
    registry: &TypeRegistry,
    typed_program: &Program<Type>,
) -> Result<(), CodegenError> {
    let used_pairs = collect_used_pairs(typed_program, registry);

    for (type_name, protocol_name) in used_pairs {
        build_itable_for_pair(ctx, registry, &type_name, &protocol_name)?;
    }

    Ok(())
}

/// Returns the slot index of a method in a protocol's flattened method table.
///
/// The slot index is the position of the method in the protocol's flattened
/// method table iteration order. This is used to index into the itable array.
pub fn protocol_method_slot(
    registry: &TypeRegistry,
    protocol_name: &str,
    method_name: &str,
) -> Option<usize> {
    registry
        .lookup_protocol(protocol_name)
        .and_then(|info| info.flattened_methods.get_index_of(method_name))
}

/// Collects all `(type, protocol)` pairs that are actually used in the program.
///
/// A pair is recorded whenever a value of a concrete type is implicitly or explicitly
/// converted to a protocol type. This includes:
///   - Assignments to variables, parameters, or fields with a protocol type.
///   - Passing arguments to parameters of protocol type.
///   - Returning a value from a function whose return type is a protocol.
///   - Explicit casts (`as`) to a protocol type.
///
/// The result is a set of pairs used to generate interface tables.
fn collect_used_pairs(program: &Program<Type>, registry: &TypeRegistry) -> HashSet<ItablePair> {
    let mut pairs = HashSet::new();

    /// Records a conversion from `src_ty` to `target_ty` if the source is a concrete
    /// named type and the target is a protocol.
    fn record_conversion(
        src_ty: &Type,
        target_ty: &Type,
        registry: &TypeRegistry,
        pairs: &mut HashSet<ItablePair>,
    ) {
        if let Type::Named(src_name) = src_ty {
            if registry.is_protocol(src_ty) {
                return;
            }
            match target_ty {
                Type::Named(target_name) if registry.is_protocol(target_ty) => {
                    pairs.insert((src_name.clone(), target_name.clone()));
                }
                Type::Iterable(_) => {
                    pairs.insert((src_name.clone(), "Iterable".to_string()));
                }
                _ => {}
            }
        }
    }

    /// Recursively walks an expression tree, propagating the expected type downward.
    /// The `visitor` closure is called for each expression with its expected type.
    fn walk_expr<F>(
        expr: &Expr<Type>,
        expected: Option<&Type>,
        registry: &TypeRegistry,
        pairs: &mut HashSet<ItablePair>,
        visitor: &mut F,
    ) where
        F: FnMut(&Expr<Type>, Option<&Type>, &TypeRegistry, &mut HashSet<ItablePair>),
    {
        visitor(expr, expected, registry, pairs);

        match &expr.kind {
            ExprKind::Literal(_)
            | ExprKind::Variable(_)
            | ExprKind::SelfRef
            | ExprKind::BaseRef => {}

            ExprKind::Unary(unary) => {
                walk_expr(&unary.expr, expected, registry, pairs, visitor);
            }

            ExprKind::Binary(binary) => {
                walk_expr(&binary.left, None, registry, pairs, visitor);
                walk_expr(&binary.right, None, registry, pairs, visitor);
            }

            ExprKind::Let(let_expr) => {
                for binding in &let_expr.bindings {
                    let expected_ty = binding
                        .type_annotation
                        .as_ref()
                        .map(|tr| resolve_type_ref_to_type(tr, registry));
                    walk_expr(
                        &binding.initializer,
                        expected_ty.as_ref(),
                        registry,
                        pairs,
                        visitor,
                    );
                }
                walk_expr(&let_expr.body, None, registry, pairs, visitor);
            }

            ExprKind::Assign(assign) => {
                walk_expr(&assign.value, None, registry, pairs, visitor);
            }

            ExprKind::Block(block) => {
                for e in &block.expressions {
                    walk_expr(e, None, registry, pairs, visitor);
                }
            }

            ExprKind::If(if_expr) => {
                walk_expr(&if_expr.condition, None, registry, pairs, visitor);
                let branch_expected = Some(&expr.anno);
                walk_expr(
                    &if_expr.then_branch,
                    branch_expected,
                    registry,
                    pairs,
                    visitor,
                );
                for elif in &if_expr.elif_branches {
                    walk_expr(&elif.condition, None, registry, pairs, visitor);
                    walk_expr(&elif.body, branch_expected, registry, pairs, visitor);
                }
                walk_expr(
                    &if_expr.else_branch,
                    branch_expected,
                    registry,
                    pairs,
                    visitor,
                );
            }

            ExprKind::While(while_expr) => {
                walk_expr(&while_expr.condition, None, registry, pairs, visitor);
                walk_expr(&while_expr.body, None, registry, pairs, visitor);
            }

            ExprKind::For(for_expr) => {
                walk_expr(&for_expr.iterable, None, registry, pairs, visitor);
                walk_expr(&for_expr.body, None, registry, pairs, visitor);
            }

            ExprKind::Call(call) => {
                // Gather parameter types for the call, if the callee is a known function or method.
                let param_types: Vec<Type> = if let ExprKind::Variable(name) = &call.callee.kind {
                    if let Some(sig) = registry.lookup_function(name) {
                        sig.params.iter().map(|(_, ty)| ty.clone()).collect()
                    } else {
                        Vec::new()
                    }
                } else if let ExprKind::Member(member) = &call.callee.kind {
                    if let Some(method_sig) =
                        registry.lookup_method(&member.object.anno, &member.member)
                    {
                        method_sig.params.iter().map(|(_, ty)| ty.clone()).collect()
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                };

                // Walk each argument with its corresponding parameter type if available.
                for (arg, param_ty) in call.args.iter().zip(param_types.iter()) {
                    walk_expr(arg, Some(param_ty), registry, pairs, visitor);
                }

                // If there are more arguments than we have parameter types (should not happen),
                // walk the remaining with no expected type.
                if call.args.len() > param_types.len() {
                    for arg in &call.args[param_types.len()..] {
                        walk_expr(arg, None, registry, pairs, visitor);
                    }
                }

                // Walk the callee expression itself.
                walk_expr(&call.callee, None, registry, pairs, visitor);
            }

            ExprKind::Lambda(lambda) => {
                walk_expr(&lambda.body, None, registry, pairs, visitor);
            }

            ExprKind::Member(member) => {
                walk_expr(&member.object, None, registry, pairs, visitor);
            }

            ExprKind::New(_) => {}

            ExprKind::TypeTest(type_test) => {
                walk_expr(&type_test.expr, None, registry, pairs, visitor);
            }

            ExprKind::Downcast(downcast) => {
                let src_ty = &downcast.expr.anno;
                let target_ty = resolve_type_ref_to_type(&downcast.type_name, registry);
                // Only record conversion if the target type is a protocol.
                if registry.is_protocol(&target_ty) {
                    record_conversion(src_ty, &target_ty, registry, pairs);
                }
                walk_expr(&downcast.expr, None, registry, pairs, visitor);
            }

            ExprKind::Vector(vector) => match vector {
                VectorExpr::Literal(items) => {
                    for item in items {
                        walk_expr(item, None, registry, pairs, visitor);
                    }
                }
                VectorExpr::Comprehension(comp) => {
                    walk_expr(&comp.expr, None, registry, pairs, visitor);
                    walk_expr(&comp.iterable, None, registry, pairs, visitor);
                }
            },

            ExprKind::Index(index) => {
                walk_expr(&index.object, None, registry, pairs, visitor);
                walk_expr(&index.index, None, registry, pairs, visitor);
            }

            ExprKind::Match(match_expr) => {
                walk_expr(&match_expr.value, None, registry, pairs, visitor);
                for case in &match_expr.cases {
                    walk_expr(&case.body, Some(&expr.anno), registry, pairs, visitor);
                }
            }
            ExprKind::MacroCall(mc) => {
                panic!(
                    "internal error: macro call `{}` reached code generation unexpanded",
                    mc.name
                );
            }
            ExprKind::MacroMatch(_mm) => {
                panic!("internal error: macro match reached code generation unexpanded");
            }
        }
    }

    // Visitor closure that records conversions when an expression is used in a protocol context.
    let mut visitor = |expr: &Expr<Type>,
                       expected: Option<&Type>,
                       registry: &TypeRegistry,
                       pairs: &mut HashSet<ItablePair>| {
        if let Some(expected_ty) = expected {
            record_conversion(&expr.anno, expected_ty, registry, pairs);
        }
    };

    // Walk all declarations.
    for decl in &program.declarations {
        match &decl.kind {
            DeclarationKind::Function(f) => {
                let return_ty = f
                    .return_type
                    .as_ref()
                    .map(|tr| resolve_type_ref_to_type(tr, registry));
                walk_expr(
                    &f.body,
                    return_ty.as_ref(),
                    registry,
                    &mut pairs,
                    &mut visitor,
                );
            }

            DeclarationKind::Type(ty) => {
                for member in &ty.members {
                    match &member.kind {
                        TypeMemberKind::Attribute(attr) => {
                            let expected_ty = attr
                                .type_annotation
                                .as_ref()
                                .map(|tr| resolve_type_ref_to_type(tr, registry));
                            walk_expr(
                                &attr.initializer,
                                expected_ty.as_ref(),
                                registry,
                                &mut pairs,
                                &mut visitor,
                            );
                        }
                        TypeMemberKind::Method(m) => {
                            let return_ty = m
                                .return_type
                                .as_ref()
                                .map(|tr| resolve_type_ref_to_type(tr, registry));
                            walk_expr(
                                &m.body,
                                return_ty.as_ref(),
                                registry,
                                &mut pairs,
                                &mut visitor,
                            );
                        }
                    }
                }
            }

            DeclarationKind::Protocol(_) => {}

            DeclarationKind::Macro(m) => {
                panic!(
                    "internal error: macro declaration `{}` reached code generation unexpanded",
                    m.name
                );
            }
        }
    }

    // Walk the entry expression.
    walk_expr(&program.entry, None, registry, &mut pairs, &mut visitor);

    pairs
}

/// Builds a single itable for a given `(type_name, protocol_name)` pair.
///
/// The itable is a global constant array of function pointers, one per protocol
/// method, in the protocol's flattened method order.
fn build_itable_for_pair(
    ctx: &mut CodegenCtx,
    registry: &TypeRegistry,
    type_name: &str,
    protocol_name: &str,
) -> Result<(), CodegenError> {
    let proto_info = registry.lookup_protocol(protocol_name).ok_or_else(|| {
        CodegenError::llvm_verification(format!("protocol `{}` not found", protocol_name))
    })?;

    // Use the flattened method table (includes inherited methods).
    let proto_methods = &proto_info.flattened_methods;
    if proto_methods.is_empty() {
        return Ok(());
    }

    let ptr_type = ctx.context.ptr_type(Default::default());
    let mut fn_ptrs = Vec::new();

    for method_name in proto_methods.keys() {
        let fn_val =
            get_method_function(ctx, registry, type_name, method_name, Some(proto_info.span))?;
        let fn_ptr = fn_val.as_global_value().as_pointer_value();
        fn_ptrs.push(fn_ptr);
    }

    // Create a global array of pointers.
    let itable_type = ptr_type.array_type(fn_ptrs.len() as u32);
    let global_name = format!("{}__itable__{}", type_name, protocol_name);
    let global = ctx.module.add_global(itable_type, None, &global_name);
    let const_array = ptr_type.const_array(&fn_ptrs);
    global.set_initializer(&const_array);
    global.set_constant(true);

    // Store for later lookup.
    ctx.itables
        .insert((type_name.to_string(), protocol_name.to_string()), global);

    Ok(())
}

/// Retrieves the LLVM `FunctionValue` for a method of a given type.
///
/// For user‑defined types, the method is stored as `"Type::method"`.
/// For builtin types (`Vector`, `Range`), we map to the runtime function names.
fn get_method_function<'ctx>(
    ctx: &CodegenCtx<'ctx>,
    registry: &TypeRegistry,
    type_name: &str,
    method_name: &str,
    span: Option<SourceSpan>,
) -> Result<inkwell::values::FunctionValue<'ctx>, CodegenError> {
    if let Some(owner) = crate::layout::owning_type_for_method(type_name, method_name, registry) {
        let qualified_name = format!("{}::{}", owner, method_name);
        if let Some(fn_val) = ctx.functions.get(&qualified_name) {
            return Ok(*fn_val);
        }
    }

    // Fallback for builtin types (hard‑coded mapping).
    let runtime_name = match (type_name, method_name) {
        ("Vector", "size") => "hulk_rt_vector_size",
        ("Vector", "get") => "hulk_rt_vector_get",
        ("Vector", "set") => "hulk_rt_vector_set",
        ("Vector", "next") => "hulk_rt_vector_next",
        ("Vector", "current") => "hulk_rt_vector_current",
        ("Range", "next") => "hulk_rt_range_next",
        ("Range", "current") => "hulk_rt_range_current",
        _ => {
            return Err(CodegenError::unsupported(
                format!("method `{}` of type `{}` not found", method_name, type_name),
                span,
            ))
        }
    };

    ctx.functions.get(runtime_name).cloned().ok_or_else(|| {
        CodegenError::unsupported(
            format!("runtime function `{}` not declared", runtime_name),
            span,
        )
    })
}
