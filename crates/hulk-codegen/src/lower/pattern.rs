//! Lowering of `match` expressions and patterns.
//!
//! Each `match` expression is lowered to a linear sequence of pattern checks.
//! - Literal patterns compare by value (`==` for numbers and booleans,
//!   `hulk_rt_string_equals` for strings).
//! - Type patterns call `hulk_rt_downcast_check` and bind the aliased variable
//!   to the downcasted pointer.
//! - Wildcard and variable patterns always match.
//! - If no pattern matches, `hulk_rt_match_fail()` is called (non‑returning).

use hulk_ast::{Expr, ExprKind, Literal, MatchExpr, Pattern, SourceSpan};
use hulk_semantic::Type;
use inkwell::values::{BasicValueEnum, IntValue};
use inkwell::FloatPredicate;

use super::lower_expr;
use crate::error::CodegenError;
use crate::lower::utils::llvm_type;
use crate::lower::LowerCtx;
use crate::runtime_decls::ensure_decl;

type PatternMatchResult<'ctx> = (
    IntValue<'ctx>,
    Vec<(String, BasicValueEnum<'ctx>, Type)>,
    bool,
);

/// Lowers a `match` expression.
///
/// The scrutinee is evaluated once. Each case is tested sequentially; the first
/// matching case executes its body and the result becomes the value of the
/// whole `match`. If no case matches, `hulk_rt_match_fail` is called.
pub fn lower_match<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    match_expr: &MatchExpr<Type>,
    result_type: &Type,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    let result_ty = llvm_type(ctx.codegen, ctx.registry, result_type)?;
    let result_alloca = ctx
        .codegen
        .builder
        .build_alloca(result_ty, "match_result")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // Store a dummy default value (zero/null) – it is only used if the
    // match is non‑exhaustive, which will call `match_fail` instead.
    let dummy: BasicValueEnum<'ctx> = match result_type {
        Type::Number => {
            let val = ctx.codegen.context.f64_type().const_float(0.0);
            BasicValueEnum::FloatValue(val)
        }
        Type::Boolean => {
            let val = ctx.codegen.context.bool_type().const_int(0, false);
            BasicValueEnum::IntValue(val)
        }
        Type::String | Type::Object | Type::Vector(_) => {
            let ptr_type = ctx.codegen.context.ptr_type(Default::default());
            let val = ptr_type.const_null();
            BasicValueEnum::PointerValue(val)
        }
        Type::Named(_) => {
            let ptr_type = ctx.codegen.context.ptr_type(Default::default());
            let val = ptr_type.const_null();
            BasicValueEnum::PointerValue(val)
        }
        Type::Iterable(_) => {
            let ptr_type = ctx.codegen.context.ptr_type(Default::default());
            let null_ptr = ptr_type.const_null();
            let val = ctx
                .codegen
                .context
                .const_struct(&[null_ptr.into(), null_ptr.into()], false);
            BasicValueEnum::StructValue(val)
        }
        _ => {
            return Err(CodegenError::unsupported(
                format!(
                    "match result type `{}` not supported",
                    match_expr.value.anno
                ),
                Some(match_expr.value.span),
            ));
        }
    };
    ctx.codegen
        .builder
        .build_store(result_alloca, dummy)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    let parent_fn = ctx
        .codegen
        .builder
        .get_insert_block()
        .unwrap()
        .get_parent()
        .unwrap();
    let merge_bb = ctx
        .codegen
        .context
        .append_basic_block(parent_fn, "match_merge");

    // Lower the scrutinee once.
    let scrutinee_val = lower_expr(ctx, &match_expr.value)?;
    let scrutinee_ty = &match_expr.value.anno;

    // Determine if the match has a catch‑all pattern (wildcard or variable).
    let has_catch_all = match_expr
        .cases
        .iter()
        .any(|case| matches!(&case.pattern, Pattern::Wildcard | Pattern::Variable(_)));

    // Create the fail block only if the match is non‑exhaustive.
    let fail_bb = if !has_catch_all {
        Some(
            ctx.codegen
                .context
                .append_basic_block(parent_fn, "match_fail"),
        )
    } else {
        None
    };

    // For each case, generate a pattern check block and a body block.
    let mut case_check_blocks = Vec::new();
    let mut case_body_blocks = Vec::new();
    for (i, _case) in match_expr.cases.iter().enumerate() {
        let check_bb = ctx
            .codegen
            .context
            .append_basic_block(parent_fn, &format!("case_{}_check", i));
        let body_bb = ctx
            .codegen
            .context
            .append_basic_block(parent_fn, &format!("case_{}_body", i));
        case_check_blocks.push(check_bb);
        case_body_blocks.push(body_bb);
    }

    // Branch from current block to the first check block.
    ctx.codegen
        .builder
        .build_unconditional_branch(case_check_blocks[0])
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // Generate each case.
    for (i, case) in match_expr.cases.iter().enumerate() {
        let check_bb = case_check_blocks[i];
        let body_bb = case_body_blocks[i];

        // Determine the next block if this case fails.
        let next_bb = if i + 1 < case_check_blocks.len() {
            case_check_blocks[i + 1]
        } else if let Some(fail) = &fail_bb {
            *fail
        } else {
            merge_bb
        };

        ctx.codegen.builder.position_at_end(check_bb);

        // Lower the pattern; returns a boolean condition and the bindings.
        let (cond, bindings, _is_catch_all) = lower_pattern(
            ctx,
            &case.pattern,
            &scrutinee_val,
            scrutinee_ty,
            Some(case.body.span),
        )?;

        // If pattern matches, jump to body; otherwise to next check or fail.
        ctx.codegen
            .builder
            .build_conditional_branch(cond, body_bb, next_bb)
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

        // Body block.
        ctx.codegen.builder.position_at_end(body_bb);

        // Push a new scope for the case body, and declare any bindings.
        ctx.push_scope();
        for (name, val, sem_ty) in bindings {
            let llvm_ty = val.get_type();
            let ptr = ctx
                .codegen
                .builder
                .build_alloca(llvm_ty, &name)
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            ctx.codegen
                .builder
                .build_store(ptr, val)
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            ctx.scope_stack.declare(&name, ptr, llvm_ty, sem_ty, false, None);
        }

        // Lower the case body.
        let body_val = lower_expr(ctx, &case.body)?;

        // Box the body value if the match result type is Object and the body type is primitive.
        let boxed_body_val = crate::lower::utils::ensure_boxed(
            ctx,
            body_val,
            &case.body.anno,
            result_type, // The match's result type.
        )?;

        // Store the (possibly boxed) body value in the result alloca.
        ctx.codegen
            .builder
            .build_store(result_alloca, boxed_body_val)
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

        ctx.pop_scope()?;

        // Jump to the merge block.
        ctx.codegen
            .builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    }

    // Fail block: call `hulk_rt_match_fail()` (which does not return) only
    // if the match is non‑exhaustive.
    if let Some(fail) = fail_bb {
        ctx.codegen.builder.position_at_end(fail);
        let fail_fn = ensure_decl(ctx.codegen, "hulk_rt_match_fail")?;
        ctx.codegen
            .builder
            .build_call(fail_fn, &[], "match_fail_call")
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
        ctx.codegen
            .builder
            .build_unreachable()
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    }

    // Merge block: load the result and return it.
    ctx.codegen.builder.position_at_end(merge_bb);
    let result = ctx
        .codegen
        .builder
        .build_load(result_ty, result_alloca, "match_result_load")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    Ok(result)
}

/// Lowers a single pattern and returns a condition (i1) indicating whether it matches,
/// plus a list of (variable_name, value) bindings that must be declared in the case body.
///
/// # Parameters
/// - `ctx`: the lowering context.
/// - `pattern`: the pattern to match.
/// - `scrutinee_val`: the LLVM value of the scrutinee expression.
/// - `scrutinee_ty`: the static type of the scrutinee.
fn lower_pattern<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    pattern: &Pattern,
    scrutinee_val: &BasicValueEnum<'ctx>,
    scrutinee_ty: &Type,
    span: Option<SourceSpan>,
) -> Result<PatternMatchResult<'ctx>, CodegenError> {
    let bool_ty = ctx.codegen.context.bool_type();
    let true_val = bool_ty.const_int(1, false);

    match pattern {
        Pattern::Wildcard => Ok((true_val, Vec::new(), true)),

        Pattern::Variable(name) => {
            // Always matches, binds the scrutinee value with its semantic type.
            Ok((
                true_val,
                vec![(name.clone(), *scrutinee_val, scrutinee_ty.clone())],
                true,
            ))
        }

        Pattern::Literal(lit) => {
            let cond = match lit {
                Literal::Number(n) => {
                    let c = ctx.codegen.context.f64_type().const_float(*n);
                    ctx.codegen
                        .builder
                        .build_float_compare(
                            FloatPredicate::OEQ,
                            scrutinee_val.into_float_value(),
                            c,
                            "lit_cmp",
                        )
                        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
                }
                Literal::Boolean(b) => {
                    let c = ctx
                        .codegen
                        .context
                        .bool_type()
                        .const_int(if *b { 1 } else { 0 }, false);
                    ctx.codegen
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::EQ,
                            scrutinee_val.into_int_value(),
                            c,
                            "lit_cmp",
                        )
                        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
                }
                Literal::String(s) => {
                    let lit_expr = Expr {
                        kind: ExprKind::Literal(Literal::String(s.clone())),
                        anno: Type::String,
                        span: hulk_ast::SourceSpan::new(0, 0),
                    };
                    let lit_val = lower_expr(ctx, &lit_expr)?;
                    let str_eq_fn = ensure_decl(ctx.codegen, "hulk_rt_string_equals")?;
                    let call = ctx
                        .codegen
                        .builder
                        .build_call(
                            str_eq_fn,
                            &[(*scrutinee_val).into(), lit_val.into()],
                            "str_eq",
                        )
                        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
                    call.try_as_basic_value().basic().unwrap().into_int_value()
                }
            };
            Ok((cond, Vec::new(), false))
        }

        Pattern::Type(type_ref, alias) => {
            // Resolve the target type from the type reference.
            let target_ty = crate::lower::utils::resolve_type_ref_to_type(type_ref, ctx.registry);

            // Get the vtable for the target type.
            let target_type_name = &type_ref.name;
            let target_vtable = ctx
                .codegen
                .type_layouts
                .get(target_type_name)
                .and_then(|layout| layout.vtable_global)
                .ok_or_else(|| {
                    CodegenError::unsupported(
                        format!("vtable for type `{}` not found", target_type_name),
                        span,
                    )
                })?;
            let target_vtable_ptr = target_vtable.as_pointer_value();

            // Call downcast_check.
            let downcast_fn = ensure_decl(ctx.codegen, "hulk_rt_downcast_check")?;
            let obj_ptr = scrutinee_val.into_pointer_value();
            let call = ctx
                .codegen
                .builder
                .build_call(
                    downcast_fn,
                    &[obj_ptr.into(), target_vtable_ptr.into()],
                    "downcast_check",
                )
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            let is_ok = call.try_as_basic_value().basic().unwrap().into_int_value();

            // If alias is present, bind the downcasted pointer with the target type.
            let mut bindings = Vec::new();
            if let Some(alias_name) = alias {
                let ptr_type = ctx.codegen.context.ptr_type(Default::default());
                let cast_ptr = ctx
                    .codegen
                    .builder
                    .build_pointer_cast(obj_ptr, ptr_type, "downcast_ptr")
                    .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
                // The semantic type is the target type.
                bindings.push((alias_name.clone(), cast_ptr.into(), target_ty));
            }
            Ok((is_ok, bindings, false))
        }
    }
}
