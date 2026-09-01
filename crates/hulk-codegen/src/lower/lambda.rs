//! Lambda expression lowering with closure conversion.
//!
//! WHY: Type::Function uses a uniform { env_ptr, fn_ptr } fat-pointer
//! representation (same ABI as protocols/Iterable — see utils.rs llvm_type).
//! This is the standard "uniform calling convention" (cf. Rust RFC 1558,
//! OCaml): a non-capturing lambda is simply a closure whose env_ptr is null.
//! lower_function_value in call.rs always destructures the same { env_ptr,
//! fn_ptr } shape regardless of whether env_ptr is used.
//!
//! Closure conversion: when the lambda body references outer-scope variables
//! (free variables), we allocate an environment struct on the heap and store
//! captured values there. The lambda receives env_ptr as its first argument
//! and loads captured values from it — this avoids the LLVM SSA dominance
//! violation that arises from loading outer-function allocas inside a different
//! LLVM function.
//!
//! ENV_SLOT_BYTES = 16: large enough for any HULK value, including
//! { ptr, ptr } fat pointers (Type::Function / Type::Iterable), which are
//! 16 bytes on x86-64. Primitives (f64, ptr, i1) use at most 8 bytes and
//! waste the upper half — acceptable given typical capture counts.

use std::collections::HashSet;

use hulk_ast::{AssignTarget, Expr, ExprKind, LambdaExpr, VectorExpr};
use hulk_semantic::Type;
use hulk_rt::ENV_SLOT_BYTES;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{BasicValueEnum, PointerValue};

use super::lower_expr;
use crate::error::CodegenError;
use crate::lower::scope::ScopeStack;
use crate::lower::utils::{llvm_type, is_heap_allocated_type};
use crate::lower::LowerCtx;

/// (name, outer alloca ptr, LLVM type, semantic type) for one captured variable.
type Capture<'ctx> = (String, PointerValue<'ctx>, BasicTypeEnum<'ctx>, Type);

pub fn lower_lambda<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    lambda: &LambdaExpr<Type>,
    lambda_type: &Type,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    let (param_types, return_type) = match lambda_type {
        Type::Function {
            params,
            return_type,
        } => (params.as_slice(), return_type.as_ref()),
        _ => {
            return Err(CodegenError::unsupported(
                format!("lambda expression has non-Function type `{}`", lambda_type),
                None,
            ))
        }
    };

    // 1. Collect free variables before emitting anything.
    let lambda_param_names: HashSet<String> =
        lambda.params.iter().map(|p| p.name.clone()).collect();
    let free_vars = collect_free_vars(&lambda.body, &lambda_param_names, &ctx.scope_stack);

    let ptr_type = ctx.codegen.context.ptr_type(Default::default());

    // Build LLVM param types: (ptr env_ptr, user_params...) — env_ptr is always
    // index 0 so the calling convention is uniform for capturing and non-capturing
    // lambdas alike.
    let mut llvm_params: Vec<BasicMetadataTypeEnum> = vec![ptr_type.into()];
    for ty in param_types {
        llvm_params.push(llvm_type(ctx.codegen, ctx.registry, ty)?.into());
    }
    let llvm_return = llvm_type(ctx.codegen, ctx.registry, return_type)?;
    let fn_type = llvm_return.fn_type(&llvm_params, false);

    let name = format!("lambda_{}", ctx.codegen.next_lambda_id());
    let lambda_fn = ctx.codegen.module.add_function(&name, fn_type, None);

    // Build a field map for the GC: list of byte offsets (within the slots buffer)
    // that contain a heap‑allocated pointer or another closure environment pointer.
    let mut field_offsets: Vec<i64> = Vec::new();
    for (i, (_, _, _, sem_ty)) in free_vars.iter().enumerate() {
        let slot_offset = (i as i64) * ENV_SLOT_BYTES as i64;
        if is_heap_allocated_type(sem_ty, ctx.registry) {
            field_offsets.push(slot_offset);
        } else if matches!(sem_ty, Type::Function { .. }) {
            // Fat pointer: only the env_ptr (first field) is traced.
            field_offsets.push(slot_offset);
        }
        // Number, Boolean, etc. – no pointers.
    }
    field_offsets.push(-1); // sentinel

    let field_map_global = ctx.codegen.module.add_global(
        ctx.codegen.context.i64_type().array_type(field_offsets.len() as u32),
        None,
        &format!("{}_env_map", name),
    );
    let const_offsets: Vec<_> = field_offsets
        .iter()
        .map(|&o| ctx.codegen.context.i64_type().const_int(o as u64, true))
        .collect();
    field_map_global.set_initializer(&ctx.codegen.context.i64_type().const_array(&const_offsets));
    field_map_global.set_constant(true);

    // 2. Save the outer function's insertion point.
    let saved_bb = ctx.codegen.builder.get_insert_block();

    // 3. Alloc env struct and store captured values (still in the outer block).
    let env_ptr: PointerValue<'ctx> = if free_vars.is_empty() {
        ptr_type.const_null()
    } else {
        // Declare and retrieve hulk_rt_env_new
        let env_new_fn = ctx
            .codegen
            .functions
            .get("hulk_rt_env_new")
            .cloned()
            .ok_or_else(|| {
                CodegenError::unsupported("hulk_rt_env_new not declared".to_string(), None)
            })?;

        let slot_count = ctx
            .codegen
            .context
            .i64_type()
            .const_int(free_vars.len() as u64, false);
        let field_map_ptr = field_map_global.as_pointer_value();

        let env_alloc = ctx
            .codegen
            .builder
            .build_call(
                env_new_fn,
                &[slot_count.into(), field_map_ptr.into()],
                "env_new",
            )
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // Now store captured values, retaining pointer‑typed captures.
        let i64_type = ctx.codegen.context.i64_type();
        let i8_type = ctx.codegen.context.i8_type();
        for (i, (cap_name, cap_alloca, cap_llvm_ty, cap_sem_ty)) in free_vars.iter().enumerate() {
            let cap_val = ctx
                .codegen
                .builder
                .build_load(*cap_llvm_ty, *cap_alloca, &format!("cap_{}", cap_name))
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

            if matches!(cap_sem_ty, Type::Function { .. }) {
                // Fat pointer: retain only the environment pointer (field 0)
                let retain_fn = ctx
                    .codegen
                    .functions
                    .get("hulk_rt_retain")
                    .cloned()
                    .ok_or_else(|| CodegenError::unsupported("hulk_rt_retain not declared".to_string(), None))?;
                let inner_env_ptr = ctx
                    .codegen
                    .builder
                    .build_extract_value(cap_val.into_struct_value(), 0, "cap_env_ptr")
                    .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
                ctx.codegen
                    .builder
                    .build_call(retain_fn, &[inner_env_ptr.into()], "cap_env_retain")
                    .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            } else if is_heap_allocated_type(cap_sem_ty, ctx.registry) {
                // Single pointer: retain the whole value
                let retain_fn = ctx
                    .codegen
                    .functions
                    .get("hulk_rt_retain")
                    .cloned()
                    .ok_or_else(|| CodegenError::unsupported("hulk_rt_retain not declared".to_string(), None))?;
                ctx.codegen
                    .builder
                    .build_call(retain_fn, &[cap_val.into()], "cap_retain")
                    .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            }

            let byte_offset = i64_type.const_int(i as u64 * ENV_SLOT_BYTES, false);
            let slot_ptr = unsafe {
                ctx.codegen
                    .builder
                    .build_gep(
                        i8_type,
                        env_alloc,
                        &[byte_offset],
                        &format!("env_slot_{}", i),
                    )
                    .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
            };
            ctx.codegen
                .builder
                .build_store(slot_ptr, cap_val)
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
        }
        env_alloc
    };

    // 4. Switch to the lambda function's entry block.
    let entry_bb = ctx.codegen.context.append_basic_block(lambda_fn, "entry");
    ctx.codegen.builder.position_at_end(entry_bb);

    ctx.push_scope();
    let param_values = lambda_fn.get_params();

    // 5. Load captured variables from env_ptr and shadow them in scope.
    if !free_vars.is_empty() {
        let env_param = param_values[0].into_pointer_value();
        let i64_type = ctx.codegen.context.i64_type();
        let i8_type = ctx.codegen.context.i8_type();
        for (i, (cap_name, _, cap_llvm_ty, cap_sem_ty)) in free_vars.iter().enumerate() {
            let byte_offset = i64_type.const_int(i as u64 * ENV_SLOT_BYTES, false);
            let slot_ptr = unsafe {
                ctx.codegen
                    .builder
                    .build_gep(
                        i8_type,
                        env_param,
                        &[byte_offset],
                        &format!("env_load_{}", i),
                    )
                    .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
            };
            let cap_val = ctx
                .codegen
                .builder
                .build_load(*cap_llvm_ty, slot_ptr, cap_name)
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            let alloca = ctx
                .codegen
                .builder
                .build_alloca(*cap_llvm_ty, cap_name)
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            ctx.codegen
                .builder
                .build_store(alloca, cap_val)
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            ctx.scope_stack
                .declare(cap_name, alloca, *cap_llvm_ty, cap_sem_ty.clone(), false, None);
        }
    }

    // 6. Bind user params (index 0 = env_ptr; user params start at index 1).
    for (i, param) in lambda.params.iter().enumerate() {
        let param_ty = &param_types[i];
        let param_llvm_ty = llvm_type(ctx.codegen, ctx.registry, param_ty)?;
        let param_val = param_values[i + 1];
        let alloca = ctx
            .codegen
            .builder
            .build_alloca(param_llvm_ty, &param.name)
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
        ctx.codegen
            .builder
            .build_store(alloca, param_val)
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
        ctx.scope_stack
            .declare(&param.name, alloca, param_llvm_ty, param_ty.clone(), false, None);
    }

    let body_val = lower_expr(ctx, &lambda.body)?;
    ctx.pop_scope()?;

    ctx.codegen
        .builder
        .build_return(Some(&body_val))
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    // 7. Restore the outer function's insertion point.
    if let Some(bb) = saved_bb {
        ctx.codegen.builder.position_at_end(bb);
    }

    // 8. Build the fat pointer { env_ptr, fn_addr } at the creation site.
    let fn_addr = lambda_fn.as_global_value().as_pointer_value();
    if free_vars.is_empty() {
        // Non-capturing: constant fat pointer (env_ptr is the null constant).
        let fat_ptr = ctx
            .codegen
            .context
            .const_struct(&[env_ptr.into(), fn_addr.into()], false);
        Ok(fat_ptr.into())
    } else {
        // Capturing: build { env_ptr, fn_addr } via alloca+GEP+store+load,
        // matching the pattern used in member.rs for other runtime fat pointers.
        let fat_ptr_ty = ctx
            .codegen
            .context
            .struct_type(&[ptr_type.into(), ptr_type.into()], false);
        let fat_alloca = ctx
            .codegen
            .builder
            .build_alloca(fat_ptr_ty, "lambda_fat")
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
        let i32_type = ctx.codegen.context.i32_type();
        let env_slot = unsafe {
            ctx.codegen
                .builder
                .build_gep(
                    fat_ptr_ty,
                    fat_alloca,
                    &[i32_type.const_int(0, false), i32_type.const_int(0, false)],
                    "lambda_env_slot",
                )
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
        };
        ctx.codegen
            .builder
            .build_store(env_slot, env_ptr)
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
        let fn_slot = unsafe {
            ctx.codegen
                .builder
                .build_gep(
                    fat_ptr_ty,
                    fat_alloca,
                    &[i32_type.const_int(0, false), i32_type.const_int(1, false)],
                    "lambda_fn_slot",
                )
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?
        };
        ctx.codegen
            .builder
            .build_store(fn_slot, fn_addr)
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
        let fat_val = ctx
            .codegen
            .builder
            .build_load(fat_ptr_ty, fat_alloca, "lambda_fat_val")
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
        Ok(fat_val)
    }
}

/// Collect free variables in `body`: Variable references that are NOT in
/// `lambda_params` but ARE bound in the outer `scope_stack`.
///
/// Returns one entry per unique free variable, in first-encountered order.
fn collect_free_vars<'ctx>(
    body: &Expr<Type>,
    lambda_params: &HashSet<String>,
    scope_stack: &ScopeStack<'ctx>,
) -> Vec<Capture<'ctx>> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut captures: Vec<Capture<'ctx>> = Vec::new();
    walk_free_vars(body, lambda_params, scope_stack, &mut seen, &mut captures);
    captures
}

/// Recursive AST walker for free variable collection.
///
/// `shadowed` grows as we descend into nested lambdas and let/for bindings
/// so that inner binders don't produce spurious captures.
fn walk_free_vars<'ctx>(
    expr: &Expr<Type>,
    shadowed: &HashSet<String>,
    scope_stack: &ScopeStack<'ctx>,
    seen: &mut HashSet<String>,
    captures: &mut Vec<Capture<'ctx>>,
) {
    match &expr.kind {
        ExprKind::Variable(name) => {
            if !shadowed.contains(name) && !seen.contains(name) {
                if let Some((ptr, llvm_ty, sem_ty)) = scope_stack.lookup(name) {
                    seen.insert(name.clone());
                    captures.push((name.clone(), ptr, llvm_ty, sem_ty));
                }
            }
        }

        ExprKind::Lambda(inner) => {
            // Inner lambda params shadow names for the inner body only.
            let mut inner_shadowed = shadowed.clone();
            for p in &inner.params {
                inner_shadowed.insert(p.name.clone());
            }
            walk_free_vars(&inner.body, &inner_shadowed, scope_stack, seen, captures);
        }

        ExprKind::Let(let_expr) => {
            // Each binding shadows for subsequent bindings and the body.
            let mut cur = shadowed.clone();
            for binding in &let_expr.bindings {
                walk_free_vars(&binding.initializer, &cur, scope_stack, seen, captures);
                cur.insert(binding.name.clone());
            }
            walk_free_vars(&let_expr.body, &cur, scope_stack, seen, captures);
        }

        ExprKind::For(for_expr) => {
            walk_free_vars(&for_expr.iterable, shadowed, scope_stack, seen, captures);
            let mut body_shadowed = shadowed.clone();
            body_shadowed.insert(for_expr.var.clone());
            walk_free_vars(&for_expr.body, &body_shadowed, scope_stack, seen, captures);
        }

        ExprKind::Assign(a) => {
            match &a.target {
                AssignTarget::Variable(_) => {}
                AssignTarget::Member { object, .. } => {
                    walk_free_vars(object, shadowed, scope_stack, seen, captures);
                }
                AssignTarget::Index { object, index } => {
                    walk_free_vars(object, shadowed, scope_stack, seen, captures);
                    walk_free_vars(index, shadowed, scope_stack, seen, captures);
                }
            }
            walk_free_vars(&a.value, shadowed, scope_stack, seen, captures);
        }

        ExprKind::Literal(_) | ExprKind::SelfRef | ExprKind::BaseRef => {}

        ExprKind::Unary(u) => {
            walk_free_vars(&u.expr, shadowed, scope_stack, seen, captures);
        }
        ExprKind::Binary(b) => {
            walk_free_vars(&b.left, shadowed, scope_stack, seen, captures);
            walk_free_vars(&b.right, shadowed, scope_stack, seen, captures);
        }
        ExprKind::Block(b) => {
            for e in &b.expressions {
                walk_free_vars(e, shadowed, scope_stack, seen, captures);
            }
        }
        ExprKind::If(i) => {
            walk_free_vars(&i.condition, shadowed, scope_stack, seen, captures);
            walk_free_vars(&i.then_branch, shadowed, scope_stack, seen, captures);
            for elif in &i.elif_branches {
                walk_free_vars(&elif.condition, shadowed, scope_stack, seen, captures);
                walk_free_vars(&elif.body, shadowed, scope_stack, seen, captures);
            }
            walk_free_vars(&i.else_branch, shadowed, scope_stack, seen, captures);
        }
        ExprKind::While(w) => {
            walk_free_vars(&w.condition, shadowed, scope_stack, seen, captures);
            walk_free_vars(&w.body, shadowed, scope_stack, seen, captures);
        }
        ExprKind::Call(c) => {
            walk_free_vars(&c.callee, shadowed, scope_stack, seen, captures);
            for arg in &c.args {
                walk_free_vars(arg, shadowed, scope_stack, seen, captures);
            }
        }
        ExprKind::Member(m) => {
            walk_free_vars(&m.object, shadowed, scope_stack, seen, captures);
        }
        ExprKind::New(n) => {
            for arg in &n.args {
                walk_free_vars(arg, shadowed, scope_stack, seen, captures);
            }
        }
        ExprKind::TypeTest(t) => {
            walk_free_vars(&t.expr, shadowed, scope_stack, seen, captures);
        }
        ExprKind::Downcast(d) => {
            walk_free_vars(&d.expr, shadowed, scope_stack, seen, captures);
        }
        ExprKind::Vector(v) => match v {
            VectorExpr::Literal(items) => {
                for item in items {
                    walk_free_vars(item, shadowed, scope_stack, seen, captures);
                }
            }
            VectorExpr::Comprehension(c) => {
                walk_free_vars(&c.expr, shadowed, scope_stack, seen, captures);
                walk_free_vars(&c.iterable, shadowed, scope_stack, seen, captures);
            }
        },
        ExprKind::Index(i) => {
            walk_free_vars(&i.object, shadowed, scope_stack, seen, captures);
            walk_free_vars(&i.index, shadowed, scope_stack, seen, captures);
        }
        ExprKind::Match(m) => {
            walk_free_vars(&m.value, shadowed, scope_stack, seen, captures);
            for case in &m.cases {
                walk_free_vars(&case.body, shadowed, scope_stack, seen, captures);
            }
        }
        ExprKind::MacroCall(mc) => {
            panic!(
                "internal error: macro call `{}` reached code generation (lambda free var collection) unexpanded",
                mc.name
            );
        }
        ExprKind::MacroMatch(_mm) => {
                panic!("internal error: macro match reached code generation unexpanded");
            }
    }
}
