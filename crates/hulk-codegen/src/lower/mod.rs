//! Lowering of typed HULK expressions to LLVM IR.
//!
//! This module implements Phase 3 of the code generation pipeline:
//! converting the typed AST (`hulk_ast::Expr<Type>`) into LLVM IR
//! instructions. It handles literals, variables, unary/binary operators,
//! control flow (`if`/`while`), blocks, `let` bindings, and assignments to
//! variable targets. More complex constructs (calls, member access, object
//! creation, vectors, and pattern matching) are deferred to later phases and
//! are currently stubbed as `CodegenError::Unsupported`.
//!
//! The design follows the standard LLVM front‑end pattern: every local
//! variable is allocated on the stack via `alloca`, and the `mem2reg` pass
//! (Phase 8) later promotes these to SSA values. This avoids manual SSA
//! bookkeeping and keeps the lowering code simple and correct.

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, PointerValue};
use std::collections::HashMap;

use hulk_ast::{DeclarationKind, Expr, Program, SourceSpan, TypeDecl};
use hulk_semantic::{Type, TypeRegistry};

use crate::context::CodegenCtx;
use crate::error::CodegenError;
use crate::lower::scope::ScopeStack;
use crate::lower::utils::{is_heap_allocated_type, object_pointer_from_fat_ptr};

// ─── Submodules ───────────────────────────────────────────────────────────

pub mod binding;
pub mod builtins;
pub mod call;
pub mod control;
pub mod decl;
pub mod for_loop;
pub mod index;
pub mod lambda;
pub mod literal;
pub mod member;
pub mod method;
pub mod new;
pub mod operators;
pub mod pattern;
pub mod scope;
pub mod type_ops;
pub mod utils;
pub mod variable;
pub mod vector;

// ─── Lowering context ────────────────────────────────────────────────────

/// Context for lowering a single expression tree.
///
/// This struct holds all the mutable state needed during expression lowering:
/// the LLVM context, the current module and builder, a stack of lexical
/// scopes for local variables, and a read‑only reference to the type registry
/// from semantic analysis.
pub struct LowerCtx<'a, 'ctx> {
    /// The LLVM context, module, and instruction builder.
    pub codegen: &'a mut CodegenCtx<'ctx>,
    /// Stack of lexical scopes, mirroring `hulk_semantic::Environment`.
    pub scope_stack: ScopeStack<'ctx>,
    /// Read‑only registry from semantic analysis.
    pub registry: &'a TypeRegistry,
    /// The typed program being lowered (for reference to declarations).
    pub typed_program: &'a Program<Type>,
    /// Map of type names to their declarations for quick lookup.
    pub type_decls: HashMap<String, &'a TypeDecl<Type>>,
    /// Which type the current method belongs to (if any).
    pub current_type: Option<String>,
    /// The name of the current method being lowered (if any).
    pub current_method: Option<String>,
}

impl<'a, 'ctx> LowerCtx<'a, 'ctx> {
    /// Creates a new lowering context.
    pub fn new(
        codegen: &'a mut CodegenCtx<'ctx>,
        registry: &'a TypeRegistry,
        typed_program: &'a Program<Type>,
    ) -> Self {
        let mut type_decls = HashMap::new();
        for decl in &typed_program.declarations {
            if let DeclarationKind::Type(ty_decl) = &decl.kind {
                type_decls.insert(ty_decl.name.clone(), ty_decl);
            }
        }
        Self {
            codegen,
            scope_stack: ScopeStack::new(),
            registry,
            typed_program,
            type_decls,
            current_type: None,
            current_method: None,
        }
    }

    /// Pushes a new lexical scope.
    pub fn push_scope(&mut self) {
        self.scope_stack.push_scope();
    }

    /// Pops the innermost lexical scope.
    ///
    /// For owned pointer-typed bindings: loads the current value and releases it.
    /// For all pointer-typed bindings (owned or borrowed): pops their shadow-stack
    /// entries in reverse declaration order (LIFO) so the push/pop sequence is
    /// always balanced.
    pub fn pop_scope(&mut self) -> Result<(), CodegenError> {
        let (scope, shadow_slots) = self.scope_stack.pop_scope();

        // ── 1. Release owned heap-allocated bindings ──────────────────────
        let release_fn = self.codegen.functions.get("hulk_rt_release").cloned();
        if let Some(release) = release_fn {
            for (_name, (ptr, llvm_ty, sem_ty, owned)) in &scope {
                if *owned && is_heap_allocated_type(sem_ty, self.registry) {
                    // Load the full value using its stored LLVM type.
                    let val = self
                        .codegen
                        .builder
                        .build_load(*llvm_ty, *ptr, "scope_exit_load")
                        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
                    // If it's a fat pointer, extract the object data.
                    let obj_ptr = object_pointer_from_fat_ptr(self, val, sem_ty)?;
                    self.codegen
                        .builder
                        .build_call(release, &[obj_ptr.into()], "scope_exit_release")
                        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
                    // Null the slot to prevent accidental reuse
                    let null_val = llvm_ty.const_zero();
                    self.codegen
                        .builder
                        .build_store(*ptr, null_val)
                        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
                }
            }
        }

        // ── 2. Pop shadow-stack entries in reverse (LIFO) order ──────────
        let pop_fn = self.codegen.functions.get("hulk_rt_shadow_pop").cloned();
        if let Some(pop) = pop_fn {
            for _slot in shadow_slots.iter().rev() {
                // The runtime shadow stack is purely positional (LIFO)
                self.codegen
                    .builder
                    .build_call(pop, &[], "shadow_pop")
                    .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            }
        }

        Ok(())
    }
    
    /// Declares an owned variable in the current scope and initialises it.
    ///
    /// If the variable is pointer-typed (heap-allocated), the alloca's address
    /// is immediately registered with the GC shadow stack via
    /// `hulk_rt_shadow_push`. This keeps the object reachable as a root during
    /// any collection triggered while the variable is in scope.
    pub fn declare_var(
        &mut self,
        name: &str,
        value: BasicValueEnum<'ctx>,
        sem_ty: Type,
        is_param: bool, // indicates if the caller already owns the reference
    ) -> Result<(), CodegenError> {
        let llvm_ty = value.get_type();
        let ptr = self
            .codegen
            .builder
            .build_alloca(llvm_ty, name)
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
        self.codegen
            .builder
            .build_store(ptr, value)
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

        // Shadow-push pointer-typed locals so the GC mark phase can find them.
        // Taking the alloca's address causes LLVM to treat it as address-taken,
        // preventing mem2reg from promoting it to an SSA value — the slot must
        // stay in memory because the GC will dereference it during collection.
        let shadow_slot = if is_heap_allocated_type(&sem_ty, self.registry) {
            let push_fn = self
                .codegen
                .functions
                .get("hulk_rt_shadow_push")
                .cloned()
                .ok_or_else(|| {
                    CodegenError::unsupported("hulk_rt_shadow_push not declared", None)
                })?;
            self.codegen
                .builder
                .build_call(push_fn, &[ptr.into()], "shadow_push")
                .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
            Some(ptr)
        } else {
            None // not a GC root, no shadow involvement
        };

        // owned: false -> caller retains ownership; pop_scope will not release.
        let owned = !is_param;
        self.scope_stack
            .declare(name, ptr, llvm_ty, sem_ty, owned, shadow_slot);
        Ok(())
    }

    /// Looks up a variable's pointer in the scope stack.
    pub fn lookup_var(
        &self,
        name: &str,
        span: Option<SourceSpan>,
    ) -> Result<(PointerValue<'ctx>, BasicTypeEnum<'ctx>, Type), CodegenError> {
        self.scope_stack.lookup(name).ok_or_else(|| {
            CodegenError::unsupported(
                format!(
                    "undefined variable `{}` (should have been caught by semantic analysis)",
                    name
                ),
                span,
            )
        })
    }

    /// Loads a variable's value.
    pub fn load_var(
        &self,
        name: &str,
        span: Option<SourceSpan>,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let (ptr, llvm_ty, _sem_ty) = self.scope_stack.lookup(name).ok_or_else(|| {
            CodegenError::unsupported(format!("undefined variable `{}`", name), span)
        })?;
        self.codegen
            .builder
            .build_load(llvm_ty, ptr, name)
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))
    }

    pub fn store_var(
        &mut self,
        name: &str,
        value: BasicValueEnum<'ctx>,
        span: Option<SourceSpan>,
    ) -> Result<(), CodegenError> {
        let (ptr, _llvm_ty, _sem_ty) = self.lookup_var(name, span)?;
        self.codegen
            .builder
            .build_store(ptr, value)
            .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
        Ok(())
    }
}

/// Lowers a typed expression to an LLVM value.
///
/// This is the main entry point for expression lowering. It matches on the
/// `ExprKind` and delegates to the appropriate submodule for the actual
/// lowering logic. The submodules (`literal`, `operators`, `control`,
/// `binding`, etc.) each handle a specific category of language constructs.
///
/// # Returns
/// An LLVM `BasicValueEnum` representing the computed value of the
/// expression. The exact type depends on the expression's static HULK type
/// (e.g., `f64` for `Number`, `i1` for `Boolean`, a pointer for `String`).
///
/// # Errors
/// Returns `CodegenError::Unsupported` for constructs that are not yet
/// implemented, or `CodegenError::LlvmVerification` if LLVM
/// instruction emission fails.
pub fn lower_expr<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    expr: &Expr<Type>,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    use hulk_ast::ExprKind;

    match &expr.kind {
        // ─── Literals ─────────────────────────────────────────────────────
        ExprKind::Literal(lit) => literal::lower_literal(ctx, lit),

        // ─── Variables and special references ────────────────────────────
        ExprKind::Variable(name) => variable::lower_variable(ctx, name, Some(expr.span)),
        ExprKind::SelfRef => variable::lower_variable(ctx, "self", Some(expr.span)),
        ExprKind::Vector(vector) => vector::lower_vector(ctx, vector, &expr.anno, expr.span),
        ExprKind::Index(index_expr) => {
            index::lower_index_get(ctx, index_expr, &expr.anno, expr.span)
        }

        // ─── Unary and binary operators ──────────────────────────────────
        ExprKind::Unary(unary) => operators::lower_unary(ctx, unary),
        ExprKind::Binary(binary) => operators::lower_binary(ctx, binary, &expr.anno),

        // ─── Control flow ────────────────────────────────────────────────
        ExprKind::Block(block) => control::lower_block(ctx, block),
        ExprKind::If(if_expr) => control::lower_if(ctx, if_expr, &expr.anno),
        ExprKind::While(while_expr) => control::lower_while(ctx, while_expr, &expr.anno),
        ExprKind::For(for_expr) => for_loop::lower_for(ctx, for_expr),
        ExprKind::Match(match_expr) => pattern::lower_match(ctx, match_expr, &expr.anno),

        // ─── Bindings and assignments ────────────────────────────────────
        ExprKind::Let(let_expr) => binding::lower_let(ctx, let_expr),
        ExprKind::Assign(assign) => binding::lower_assign(ctx, assign),

        // ─── Functions and properties calls ────────────────────────────────────
        ExprKind::Call(call) => call::lower_call(ctx, call),
        ExprKind::New(new_expr) => new::lower_new(ctx, new_expr, Some(expr.span)),
        ExprKind::Member(member_expr) => member::lower_member(ctx, member_expr, Some(expr.span)),

        // ─── Lambda expressions ───────────────────────────────────────────────
        ExprKind::Lambda(lambda_expr) => lambda::lower_lambda(ctx, lambda_expr, &expr.anno),

        // ─── Type tests and downcasts ──────────────────────────────────────────
        ExprKind::TypeTest(type_test) => type_ops::lower_typetest(ctx, type_test),
        ExprKind::Downcast(downcast) => type_ops::lower_downcast(ctx, downcast),

        // ─── Catch-all for unhandled cases ───────────────────────────────
        _ => Err(CodegenError::unsupported(
            format!(
                "lowering of {:?} not yet implemented or unsupported",
                expr.kind
            ),
            Some(expr.span),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OptLevel, itables, layout, lower, runtime_decls};
    use hulk_lexer::Lexer;
    use hulk_parser::parse;
    use hulk_semantic::analyze;
    use hulk_ast::{
        AssignExpr, AssignTarget, BinaryExpr, BinaryOp, BlockExpr, ElifBranch, Expr, ExprKind,
        IfExpr, IndexExpr, LetBinding, LetExpr, Literal, SourceSpan, UnaryExpr, UnaryOp, WhileExpr, ForExpr,
        MatchExpr, MatchCase, Pattern, VectorExpr, VectorComprehension, TypeRef,
    };
    use hulk_semantic::{seeded_registry, Type};
    use inkwell::context::Context;

    /// Dummy span for tests.
    fn dummy_span() -> SourceSpan {
        SourceSpan::new(0, 0)
    }

    fn dummy_program(expr: Expr<Type>) -> Program<Type> {
        Program {
            declarations: vec![],
            entry: expr.clone(),
        }
    }

    /// Helper: create a typed number literal.
    fn num(n: f64) -> Expr<Type> {
        Expr {
            kind: ExprKind::Literal(Literal::Number(n)),
            anno: Type::Number,
            span: dummy_span(),
        }
    }

    /// Helper: create a typed boolean literal.
    fn bool_lit(b: bool) -> Expr<Type> {
        Expr {
            kind: ExprKind::Literal(Literal::Boolean(b)),
            anno: Type::Boolean,
            span: dummy_span(),
        }
    }

    /// Helper: create a typed string literal.
    fn string_lit(s: &str) -> Expr<Type> {
        Expr {
            kind: ExprKind::Literal(Literal::String(s.to_string())),
            anno: Type::String,
            span: dummy_span(),
        }
    }

    /// Helper: create a typed variable.
    fn var(name: &str, ty: Type) -> Expr<Type> {
        Expr {
            kind: ExprKind::Variable(name.to_string()),
            anno: ty,
            span: dummy_span(),
        }
    }

    /// Helper: create a typed binary expression.
    fn bin_op(left: Expr<Type>, op: BinaryOp, right: Expr<Type>, ty: Type) -> Expr<Type> {
        Expr {
            kind: ExprKind::Binary(BinaryExpr {
                op,
                left: Box::new(left),
                right: Box::new(right),
            }),
            anno: ty,
            span: dummy_span(),
        }
    }

    /// Helper: create a typed unary expression.
    fn unary_op(op: UnaryOp, operand: Expr<Type>, ty: Type) -> Expr<Type> {
        Expr {
            kind: ExprKind::Unary(UnaryExpr {
                op,
                expr: Box::new(operand),
            }),
            anno: ty,
            span: dummy_span(),
        }
    }

    /// Helper: create a typed let expression.
    fn let_expr(bindings: Vec<(String, Expr<Type>)>, body: Expr<Type>, ty: Type) -> Expr<Type> {
        let let_bindings = bindings
            .into_iter()
            .map(|(name, init)| LetBinding {
                name,
                type_annotation: None,
                initializer: *Box::new(init),
            })
            .collect();
        Expr {
            kind: ExprKind::Let(LetExpr {
                bindings: let_bindings,
                body: Box::new(body),
            }),
            anno: ty,
            span: dummy_span(),
        }
    }

    /// Helper: create a typed block.
    fn block(exprs: Vec<Expr<Type>>, ty: Type) -> Expr<Type> {
        Expr {
            kind: ExprKind::Block(BlockExpr { expressions: exprs }),
            anno: ty,
            span: dummy_span(),
        }
    }

    /// Helper: create a typed if expression.
    fn if_expr(
        cond: Expr<Type>,
        then_branch: Expr<Type>,
        elifs: Vec<ElifBranch<Type>>,
        else_branch: Expr<Type>,
        ty: Type,
    ) -> Expr<Type> {
        Expr {
            kind: ExprKind::If(IfExpr {
                condition: Box::new(cond),
                then_branch: Box::new(then_branch),
                elif_branches: elifs,
                else_branch: Box::new(else_branch),
            }),
            anno: ty,
            span: dummy_span(),
        }
    }

    /// Helper: create a typed while expression.
    fn while_expr(cond: Expr<Type>, body: Expr<Type>, ty: Type) -> Expr<Type> {
        Expr {
            kind: ExprKind::While(WhileExpr {
                condition: Box::new(cond),
                body: Box::new(body),
            }),
            anno: ty,
            span: dummy_span(),
        }
    }

    /// Helper: create a typed assignment.
    fn assign(target: AssignTarget<Type>, value: Expr<Type>, ty: Type) -> Expr<Type> {
        Expr {
            kind: ExprKind::Assign(AssignExpr {
                target,
                value: Box::new(value),
            }),
            anno: ty,
            span: dummy_span(),
        }
    }

    /// Helper: create a typed call.
    fn call(callee: Expr<Type>, args: Vec<Expr<Type>>, ty: Type) -> Expr<Type> {
        Expr {
            kind: ExprKind::Call(hulk_ast::CallExpr {
                callee: Box::new(callee),
                args,
            }),
            anno: ty,
            span: dummy_span(),
        }
    }

    /// Helper: lower an expression and return the LLVM IR string.
    fn lower_expr_to_ir(expr: Expr<Type>) -> String {
        let context = Context::create();
        let mut codegen = CodegenCtx::new(&context, "test", OptLevel::None).expect("codegen ctx");
        let i32_type = context.i32_type();
        let main_fn = codegen
            .module
            .add_function("main", i32_type.fn_type(&[], false), None);
        let entry_bb = context.append_basic_block(main_fn, "entry");
        codegen.builder.position_at_end(entry_bb);

        runtime_decls::declare_all(&mut codegen);
        declare_test_builtins(&mut codegen);

        let registry = seeded_registry();
        let dprog = dummy_program(expr.clone());
        // Lower the expression in a separate scope so the borrow ends.
        {
            let mut lower_ctx = LowerCtx::new(&mut codegen, &registry, &dprog);
            let val = lower_expr(&mut lower_ctx, &expr).expect("lowering failed");
            // Store the result so it's a real operand of a `store` instruction
            let slot = lower_ctx
                .codegen
                .builder
                .build_alloca(val.get_type(), "test_result")
                .expect("alloca failed");
            lower_ctx
                .codegen
                .builder
                .build_store(slot, val)
                .expect("store failed");
        }

        // Now we can use codegen freely.
        codegen
            .builder
            .build_return(Some(&i32_type.const_int(0, false)))
            .expect("return failed");
        codegen.module.verify().expect("module verification failed");
        codegen.module.print_to_string().to_string()
    }

    /// Declares builtin functions needed for tests (e.g., `range`, `print`).
    fn declare_test_builtins(ctx: &mut CodegenCtx) {
        let ptr_type = ctx.context.ptr_type(Default::default());
        let f64_type = ctx.context.f64_type();

        // range(min: Number, max: Number) -> Range
        let range_type = ptr_type.fn_type(&[f64_type.into(), f64_type.into()], false);
        let range_fn = ctx.module.add_function("range", range_type, None);
        ctx.functions.insert("range".to_string(), range_fn);

        // print(x: Object) -> Object
        let print_type = ptr_type.fn_type(&[ptr_type.into()], false);
        let print_fn = ctx.module.add_function("print", print_type, None);
        ctx.functions.insert("print".to_string(), print_fn);
    }

    /// Lower a HULK source string to LLVM IR.
    ///
    /// This runs lexing, parsing, semantic analysis, and then code generation
    /// up to IR emission (without object emission or linking).
    fn lower_source_to_ir(src: &str) -> String {
        // 1. Lex and parse.
        let tokens = Lexer::new(src).tokenize().expect("lex failed");
        let program = parse(tokens).expect("parse failed");

        // 2. Semantic analysis.
        let verified = analyze(&program).expect("semantic analysis failed");

        // 3. Set up LLVM context and module.
        let context = Context::create();
        let mut codegen = CodegenCtx::new(&context, "test", OptLevel::None).expect("codegen ctx");

        // 4. Declare runtime functions needed for the lowered code.
        runtime_decls::declare_all(&mut codegen);
        declare_test_builtins(&mut codegen);

        // 5. Create main function (entry point).
        let i32_type = context.i32_type();
        let main_fn = codegen
            .module
            .add_function("main", i32_type.fn_type(&[], false), None);
        let entry_bb = context.append_basic_block(main_fn, "entry");
        codegen.builder.position_at_end(entry_bb);

        // 6. Build layouts for user‑defined types.
        layout::build_layouts(&verified.typed_program, &verified.registry, &mut codegen)
            .expect("build layouts");

        // 7. Declare free functions and methods.
        lower::decl::declare_functions(&mut codegen, &verified.typed_program, &verified.registry)
            .expect("declare functions");
        lower::method::declare_methods(&mut codegen, &verified.typed_program, &verified.registry)
            .expect("declare methods");

        // 8. Build vtables and itables for every (type, protocol) pair the program actually uses.
        // Emit GC field maps (before build_vtables)
        let _ = layout::build_gc_field_maps(&mut codegen, &verified.registry);

        layout::build_vtables(&mut codegen, &verified.registry).expect("build vtables");

        itables::build_itables(&mut codegen, &verified.registry, &verified.typed_program)
            .expect("build itables");

        lower::decl::define_functions(&mut codegen, &verified.typed_program, &verified.registry)
            .expect("define functions");

        // 9. Define free functions and methods.
        lower::decl::define_functions(&mut codegen, &verified.typed_program, &verified.registry)
            .expect("define functions");
        lower::method::define_methods(&mut codegen, &verified.typed_program, &verified.registry)
            .expect("define methods");

        // 10. Reset builder to main entry.
        codegen.builder.position_at_end(entry_bb);

        // 11. Lower the entry expression.
        let mut lower_ctx =
            lower::LowerCtx::new(&mut codegen, &verified.registry, &verified.typed_program);
        let _val = lower::lower_expr(&mut lower_ctx, &verified.typed_program.entry)
            .expect("lowering failed");

        // 12. Return 0 from main.
        codegen
            .builder
            .build_return(Some(&i32_type.const_int(0, false)))
            .expect("return failed");

        // 13. Verify and print the module.
        codegen.module.verify().expect("module verification failed");
        codegen.module.print_to_string().to_string()
    }

    /// Assert that the IR contains a substring.
    fn assert_ir_contains(ir: &str, expected: &str) {
        assert!(ir.contains(expected), "IR did not contain '{}'", expected);
    }

    /// Helper: create a typed `for` expression.
    fn for_expr(var: &str, iterable: Expr<Type>, body: Expr<Type>, ty: Type) -> Expr<Type> {
        Expr {
            kind: ExprKind::For(ForExpr {
                var: var.to_string(),
                iterable: Box::new(iterable),
                body: Box::new(body),
            }),
            anno: ty,
            span: dummy_span(),
        }
    }

    /// Helper: create a typed vector comprehension.
    fn comprehension(expr: Expr<Type>, var: &str, iterable: Expr<Type>, ty: Type) -> Expr<Type> {
        Expr {
            kind: ExprKind::Vector(VectorExpr::Comprehension(VectorComprehension {
                expr: Box::new(expr),
                var: var.to_string(),
                iterable: Box::new(iterable),
            })),
            anno: ty,
            span: dummy_span(),
        }
    }

    /// Helper: create a typed vector literal.
    fn vector_lit(items: Vec<Expr<Type>>, ty: Type) -> Expr<Type> {
        Expr {
            kind: ExprKind::Vector(VectorExpr::Literal(items)),
            anno: ty,
            span: dummy_span(),
        }
    }

    /// Helper: create a typed indexing expression.
    fn index_expr(object: Expr<Type>, index: Expr<Type>, ty: Type) -> Expr<Type> {
        Expr {
            kind: ExprKind::Index(IndexExpr {
                object: Box::new(object),
                index: Box::new(index),
            }),
            anno: ty,
            span: dummy_span(),
        }
    }

    /// Helper: create a typed `match` expression.
    fn match_expr(value: Expr<Type>, cases: Vec<MatchCase<Type>>, ty: Type) -> Expr<Type> {
        Expr {
            kind: ExprKind::Match(MatchExpr {
                value: Box::new(value),
                cases,
            }),
            anno: ty,
            span: dummy_span(),
        }
    }

    /// Helper: create a typed match case.
    fn match_case(pattern: Pattern, body: Expr<Type>) -> MatchCase<Type> {
        MatchCase { pattern, body }
    }

    /// Helper: create a type pattern.
    fn _type_pattern(ty: &str, alias: Option<&str>) -> Pattern {
        Pattern::Type(TypeRef::named(ty), alias.map(String::from))
    }

    // ─── Literals ─────────────────────────────────────────────────────────

    #[test]
    fn test_literal_number() {
        let expr = num(42.0);
        let ir = lower_expr_to_ir(expr);
        assert_ir_contains(&ir, "double 4.200000e+01");
    }

    #[test]
    fn test_literal_bool() {
        let expr = bool_lit(true);
        let ir = lower_expr_to_ir(expr);
        assert_ir_contains(&ir, "i1 true");
    }

    // ─── Variables and Let ──────────────────────────────────────────────

    #[test]
    fn test_variable_and_let() {
        // let x = 5 in x
        let init = num(5.0);
        let body = var("x", Type::Number);
        let expr = let_expr(vec![("x".to_string(), init)], body, Type::Number);
        let ir = lower_expr_to_ir(expr);
        assert_ir_contains(&ir, "alloca double");
        assert_ir_contains(&ir, "store double 5.000000e+00");
        assert_ir_contains(&ir, "load double");
    }

    #[test]
    fn test_let_shadowing() {
        // let a = 1 in let a = 2 in a
        let inner_init = num(2.0);
        let inner_body = var("a", Type::Number);
        let inner_let = let_expr(
            vec![("a".to_string(), inner_init)],
            inner_body,
            Type::Number,
        );
        let outer_let = let_expr(vec![("a".to_string(), num(1.0))], inner_let, Type::Number);
        let ir = lower_expr_to_ir(outer_let);
        assert_ir_contains(&ir, "alloca double");
        assert_ir_contains(&ir, "load double");
    }

    #[test]
    fn test_block() {
        // { 1; 2; 3 } -> 3
        let exprs = vec![num(1.0), num(2.0), num(3.0)];
        let block_expr = block(exprs, Type::Number);
        let ir = lower_expr_to_ir(block_expr);
        assert_ir_contains(&ir, "double 3.000000e+00");
    }

    // ─── Unary ────────────────────────────────────────────────────────────

    #[test]
    fn test_unary_negate() {
        // let x = 5 in -x
        let neg = unary_op(UnaryOp::Negate, var("x", Type::Number), Type::Number);
        let expr = let_expr(vec![("x".to_string(), num(5.0))], neg, Type::Number);
        let ir = lower_expr_to_ir(expr);
        assert_ir_contains(&ir, "load double, ptr %x");
        assert_ir_contains(&ir, "fneg double");
    }

    #[test]
    fn test_unary_not() {
        // let b = true in !b
        let not_expr = unary_op(UnaryOp::Not, var("b", Type::Boolean), Type::Boolean);
        let expr = let_expr(
            vec![("b".to_string(), bool_lit(true))],
            not_expr,
            Type::Boolean,
        );
        let ir = lower_expr_to_ir(expr);
        assert_ir_contains(&ir, "load i1, ptr %b");
        assert_ir_contains(&ir, "xor i1");
    }

    // ─── Binary ──────────────────────────────────────────────────────────

    #[test]
    fn test_binary_add() {
        // let a = 2 in let b = 3 in a + b
        let sum = bin_op(
            var("a", Type::Number),
            BinaryOp::Add,
            var("b", Type::Number),
            Type::Number,
        );
        let with_b = let_expr(vec![("b".to_string(), num(3.0))], sum, Type::Number);
        let expr = let_expr(vec![("a".to_string(), num(2.0))], with_b, Type::Number);
        let ir = lower_expr_to_ir(expr);
        assert_ir_contains(&ir, "load double, ptr %a");
        assert_ir_contains(&ir, "load double, ptr %b");
        assert_ir_contains(&ir, "fadd double");
    }
    #[test]
    fn test_binary_power() {
        let expr = bin_op(num(2.0), BinaryOp::Power, num(3.0), Type::Number);
        let ir = lower_expr_to_ir(expr);
        assert_ir_contains(
            &ir,
            "call double @llvm.pow.f64(double 2.000000e+00, double 3.000000e+00)",
        );
    }

    #[test]
    fn test_binary_concat() {
        let left = string_lit("Hello");
        let right = string_lit("World");
        let expr = bin_op(left, BinaryOp::Concat, right, Type::String);
        let ir = lower_expr_to_ir(expr);
        assert_ir_contains(&ir, "call ptr @hulk_rt_string_concat");
    }

    #[test]
    fn test_binary_concat_space() {
        let left = num(42.0);
        let right = bool_lit(true);
        let expr = bin_op(left, BinaryOp::ConcatSpace, right, Type::String);
        let ir = lower_expr_to_ir(expr);
        assert_ir_contains(&ir, "call ptr @hulk_rt_number_to_string");
        assert_ir_contains(&ir, "call ptr @hulk_rt_bool_to_string");
        assert_ir_contains(&ir, "call ptr @hulk_rt_string_concat_space");
    }

    #[test]
    fn test_binary_compare() {
        // let a = 5 in let b = 3 in a < b
        let cmp = bin_op(
            var("a", Type::Number),
            BinaryOp::Less,
            var("b", Type::Number),
            Type::Boolean,
        );
        let with_b = let_expr(vec![("b".to_string(), num(3.0))], cmp, Type::Boolean);
        let expr = let_expr(vec![("a".to_string(), num(5.0))], with_b, Type::Boolean);
        let ir = lower_expr_to_ir(expr);
        assert_ir_contains(&ir, "load double, ptr %a");
        assert_ir_contains(&ir, "load double, ptr %b");
        assert_ir_contains(&ir, "fcmp olt double");
    }

    // ─── If ──────────────────────────────────────────────────────────────

    #[test]
    fn test_if_simple() {
        let cond = bool_lit(true);
        let then_branch = num(1.0);
        let else_branch = num(2.0);
        let expr = if_expr(cond, then_branch, vec![], else_branch, Type::Number);
        let ir = lower_expr_to_ir(expr);
        assert_ir_contains(&ir, "br i1 true, label %if_then, label %if_else");
        assert_ir_contains(&ir, "if_then:");
        assert_ir_contains(&ir, "store double 1.000000e+00, ptr %if_result");
        assert_ir_contains(&ir, "if_else:");
        assert_ir_contains(&ir, "store double 2.000000e+00, ptr %if_result");
        assert_ir_contains(&ir, "if_merge:");
        assert_ir_contains(&ir, "load double, ptr %if_result");
    }

    #[test]
    fn test_if_with_elif() {
        let cond1 = bool_lit(false);
        let then1 = num(1.0);
        let elif_cond = bool_lit(true);
        let elif_body = num(2.0);
        let else_branch = num(3.0);
        let expr = if_expr(
            cond1,
            then1,
            vec![ElifBranch {
                condition: *Box::new(elif_cond),
                body: *Box::new(elif_body),
            }],
            else_branch,
            Type::Number,
        );
        let ir = lower_expr_to_ir(expr);
        assert_ir_contains(&ir, "br i1 false, label %if_then, label %if_else");
        assert_ir_contains(&ir, "elif_then_0:");
        assert_ir_contains(&ir, "store double 2.000000e+00");
        assert_ir_contains(&ir, "elif_else_0:");
        assert_ir_contains(&ir, "store double 3.000000e+00");
        assert_ir_contains(&ir, "load double, ptr %if_result");
    }

    // ─── While ───────────────────────────────────────────────────────────

    #[test]
    fn test_while_loop() {
        let cond = bool_lit(true);
        let body = num(5.0);
        let expr = while_expr(cond, body, Type::Number);
        let ir = lower_expr_to_ir(expr);
        assert_ir_contains(&ir, "while_cond:");
        assert_ir_contains(&ir, "br i1 true, label %while_body, label %while_exit");
        assert_ir_contains(&ir, "while_body:");
        assert_ir_contains(&ir, "store double 5.000000e+00, ptr %while_result");
        assert_ir_contains(&ir, "br label %while_cond");
        assert_ir_contains(&ir, "while_exit:");
        assert_ir_contains(&ir, "load double, ptr %while_result");
    }

    // ─── Assign ──────────────────────────────────────────────────────────

    #[test]
    fn test_assign_variable() {
        // let x = 0 in x := 5; x
        let init = num(0.0);
        let assign_expr = assign(
            AssignTarget::Variable("x".to_string()),
            num(5.0),
            Type::Number,
        );
        let body = block(vec![assign_expr, var("x", Type::Number)], Type::Number);
        let expr = let_expr(vec![("x".to_string(), init)], body, Type::Number);
        let ir = lower_expr_to_ir(expr);
        assert_ir_contains(&ir, "store double 5.000000e+00, ptr %x");
        assert_ir_contains(&ir, "load double, ptr %x");
    }

    // ─── Edge cases ──────────────────────────────────────────────────────

    #[test]
    fn test_empty_block() {
        let block_expr = block(vec![], Type::Boolean);
        let ir = lower_expr_to_ir(block_expr);
        assert_ir_contains(&ir, "i1 false");
    }

    #[test]
    fn test_while_default_value() {
        let cond = bool_lit(false);
        let body = num(5.0);
        let expr = while_expr(cond, body, Type::Number);
        let ir = lower_expr_to_ir(expr);
        assert_ir_contains(&ir, "store double 0.000000e+00, ptr %while_result");
        assert_ir_contains(&ir, "load double, ptr %while_result");
    }

    // ─── Function calls ──────────────────────────────────────────────────────

    #[test]
    fn test_call_function_no_args() {
        // Declare a function f(): Number that returns 42.
        let context = Context::create();
        let mut codegen = CodegenCtx::new(&context, "test", OptLevel::None).expect("codegen ctx");
        let i32_type = context.i32_type();
        let main_fn = codegen
            .module
            .add_function("main", i32_type.fn_type(&[], false), None);
        let entry_bb = context.append_basic_block(main_fn, "entry");
        codegen.builder.position_at_end(entry_bb);

        // Declare f(): Number -> Number (return type Number = f64).
        let f64_type = context.f64_type();
        let f_fn_type = f64_type.fn_type(&[], false);
        let f_fn = codegen.module.add_function("f", f_fn_type, None);
        // Define f: return 42.0.
        let f_entry = context.append_basic_block(f_fn, "entry");
        codegen.builder.position_at_end(f_entry);
        let const_42 = f64_type.const_float(42.0);
        codegen
            .builder
            .build_return(Some(&const_42))
            .expect("return");
        // Reset builder to main entry.
        codegen.builder.position_at_end(entry_bb);

        codegen.functions.insert("f".to_string(), f_fn);

        // Now create a call expression to f().
        let call_expr = call(
            var(
                "f",
                Type::Function {
                    params: vec![],
                    return_type: Box::new(Type::Number),
                },
            ),
            vec![],
            Type::Number,
        );
        let dprog = dummy_program(call_expr.clone());

        let registry = seeded_registry();
        {
            let mut lower_ctx = LowerCtx::new(&mut codegen, &registry, &dprog);
            let _val = lower_expr(&mut lower_ctx, &call_expr).expect("lowering failed");
            // Store result to make it a real operand.
            let slot = lower_ctx
                .codegen
                .builder
                .build_alloca(f64_type, "result")
                .expect("alloca");
            lower_ctx
                .codegen
                .builder
                .build_store(slot, _val)
                .expect("store");
        }

        // Return 0.
        codegen
            .builder
            .build_return(Some(&i32_type.const_int(0, false)))
            .expect("return");
        codegen.module.verify().expect("module verification failed");
        let ir = codegen.module.print_to_string().to_string();

        // Check that the call instruction appears.
        assert_ir_contains(&ir, "call double @f()");
    }

    #[test]
    fn test_call_function_with_args() {
        // Declare a function add(x: Number, y: Number): Number that returns x + y.
        let context = Context::create();
        let mut codegen = CodegenCtx::new(&context, "test", OptLevel::None).expect("codegen ctx");
        let i32_type = context.i32_type();
        let main_fn = codegen
            .module
            .add_function("main", i32_type.fn_type(&[], false), None);
        let entry_bb = context.append_basic_block(main_fn, "entry");
        codegen.builder.position_at_end(entry_bb);

        let f64_type = context.f64_type();
        let add_fn_type = f64_type.fn_type(&[f64_type.into(), f64_type.into()], false);
        let add_fn = codegen.module.add_function("add", add_fn_type, None);

        codegen.functions.insert("add".to_string(), add_fn);
        // Define add: load parameters, fadd, return.
        let add_entry = context.append_basic_block(add_fn, "entry");
        codegen.builder.position_at_end(add_entry);
        let params = add_fn.get_params();
        let x_param = params[0].into_float_value();
        let y_param = params[1].into_float_value();
        let sum = codegen
            .builder
            .build_float_add(x_param, y_param, "add")
            .expect("fadd");
        codegen.builder.build_return(Some(&sum)).expect("return");
        codegen.builder.position_at_end(entry_bb);

        // Create a call expression to add(2.0, 3.0).
        let call_expr = call(
            var(
                "add",
                Type::Function {
                    params: vec![Type::Number, Type::Number],
                    return_type: Box::new(Type::Number),
                },
            ),
            vec![num(2.0), num(3.0)],
            Type::Number,
        );

        let registry = seeded_registry();
        let dprog = dummy_program(call_expr.clone());
        {
            let mut lower_ctx = LowerCtx::new(&mut codegen, &registry, &dprog);
            let _val = lower_expr(&mut lower_ctx, &call_expr).expect("lowering failed");
            let slot = lower_ctx
                .codegen
                .builder
                .build_alloca(f64_type, "result")
                .expect("alloca");
            lower_ctx
                .codegen
                .builder
                .build_store(slot, _val)
                .expect("store");
        }

        codegen
            .builder
            .build_return(Some(&i32_type.const_int(0, false)))
            .expect("return");
        codegen.module.verify().expect("module verification failed");
        let ir = codegen.module.print_to_string().to_string();

        // Check that the call instruction appears with arguments.
        assert_ir_contains(
            &ir,
            "call double @add(double 2.000000e+00, double 3.000000e+00)",
        );
    }

    // ─── Object-oriented features ─────────────────────────────────────────

    #[test]
    fn test_new_object() {
        let src = "
            type A { }
            let x = new A() in x;
        ";
        let ir = lower_source_to_ir(src);
        assert_ir_contains(&ir, "call ptr @hulk_rt_alloc(i64");
        assert_ir_contains(&ir, "store ptr @A__vtable, ptr");
    }

    #[test]
    fn test_attribute_read() {
        let src = "
            type A {
                x = 42;
                getX(): Number => self.x;
            }
            let a = new A() in a.getX();
        ";
        let ir = lower_source_to_ir(src);
        assert_ir_contains(&ir, "load double, ptr");
    }

    #[test]
    fn test_method_call() {
        let src = "
            type A {
                f(): Number => 42;
            }
            let a = new A() in a.f();
        ";
        let ir = lower_source_to_ir(src);
        assert_ir_contains(&ir, "load ptr, ptr"); // load vtable
        assert_ir_contains(&ir, "load ptr, ptr"); // load function pointer
                                                  // Check that the IR contains an indirect call with the correct return type.
        assert_ir_contains(&ir, "call double");
        // Also ensure the vtable is loaded.
        assert_ir_contains(&ir, "load ptr");
    }

    #[test]
    fn test_inheritance_method_call() {
        let src = "
            type A {
                f(): Number => 1;
            }
            type B inherits A {
                f(): Number => 2;
            }
            let x: A = new B() in x.f();
        ";
        let ir = lower_source_to_ir(src);
        // Should call B::f (vtable dispatch)
        assert_ir_contains(&ir, "B::f");
    }

    #[test]
    fn test_base_call() {
        let src = "
            type A {
                f(): Number => 1;
            }
            type B inherits A {
                f(): Number => base();
            }
            let b = new B() in b.f();
        ";
        let ir = lower_source_to_ir(src);
        // Should call A::f directly (not via vtable)
        assert!(ir.contains("call double"));
        assert!(ir.contains("A::f"));
    }

    #[test]
    fn test_bare_method_reference() {
        let src = "
            type A {
                f(): Number => 42;
            }
            let a = new A() in
            let g = a.f in
            g();
        ";
        let ir = lower_source_to_ir(src);
        // Should produce fat pointer and indirect call
        assert_ir_contains(&ir, "store ptr");
        assert_ir_contains(&ir, "store ptr");
        assert_ir_contains(&ir, "call");
        assert_ir_contains(&ir, "%a");
    }

    #[test]
    fn test_assign_member() {
        let src = "
            type A {
                x = 0;
                setX(v: Number) => self.x := v;
                getX(): Number => self.x;
            }
            let a = new A() in {
                a.setX(42);
                a.getX();
            }
        ";
        let ir = lower_source_to_ir(src);
        assert_ir_contains(&ir, "store double");
        assert!(ir.contains("4.200000e+01") || ir.contains("4.2") || ir.contains("42.0"));
        assert_ir_contains(&ir, "load double, ptr");
    }

    #[test]
    fn test_typetest() {
        let src = "
            type A { }
            type B inherits A { }
            let x = new B() in x is A;
        ";
        let ir = lower_source_to_ir(src);
        assert_ir_contains(&ir, "call i1 @hulk_rt_downcast_check");
    }

    #[test]
    fn test_downcast() {
        let src = "
            type A { }
            type B inherits A { }
            let x: A = new B() in x as B;
        ";
        let ir = lower_source_to_ir(src);
        // Should contain downcast check and branch
        assert_ir_contains(&ir, "call i1 @hulk_rt_downcast_check");
        assert_ir_contains(&ir, "br i1");
        assert_ir_contains(&ir, "downcast_ok:");
        assert_ir_contains(&ir, "downcast_trap:");
        assert_ir_contains(&ir, "call void @hulk_rt_downcast_fail");
    }

    // ─── For Loops and Vector Comprehension ──────────────────────────────────────

    #[test]
    fn test_for_loop() {
        // for (x in [1,2,3]) print(x)
        // We'll use a simple iterable: `range(1,4)` (builtin)
        let iterable = call(
            var(
                "range",
                Type::Function {
                    params: vec![Type::Number, Type::Number],
                    return_type: Box::new(Type::Named("Range".to_string())),
                },
            ),
            vec![num(1.0), num(4.0)],
            Type::Named("Range".to_string()),
        );
        let body = call(
            var(
                "print",
                Type::Function {
                    params: vec![Type::Object],
                    return_type: Box::new(Type::Object),
                },
            ),
            vec![var("x", Type::Number)],
            Type::Object,
        );
        let for_loop = for_expr("x", iterable, body, Type::Object);
        let ir = lower_expr_to_ir(for_loop);
        assert_ir_contains(&ir, "for_cond:");
        assert_ir_contains(&ir, "call i1 @hulk_rt_range_next(ptr");
        assert_ir_contains(&ir, "call double @hulk_rt_range_current(ptr");
        assert_ir_contains(&ir, "for_exit:");
    }

    #[test]
    fn test_vector_comprehension() {
        // [x^2 | x in range(1,4)]
        let iterable = call(
            var(
                "range",
                Type::Function {
                    params: vec![Type::Number, Type::Number],
                    return_type: Box::new(Type::Named("Range".to_string())),
                },
            ),
            vec![num(1.0), num(4.0)],
            Type::Named("Range".to_string()),
        );
        let head = bin_op(
            var("x", Type::Number),
            BinaryOp::Power,
            num(2.0),
            Type::Number,
        );
        let comp = comprehension(head, "x", iterable, Type::Vector(Box::new(Type::Number)));
        let ir = lower_expr_to_ir(comp);
        assert_ir_contains(&ir, "call ptr @hulk_rt_dynamic_vector_new()");
        assert_ir_contains(&ir, "call void @hulk_rt_dynamic_vector_append(ptr");
        assert_ir_contains(&ir, "call ptr @hulk_rt_dynamic_vector_to_vector(ptr");
    }

    #[test]
    fn test_for_over_range() {
        let src = "
            let sum = 0 in {
                for (x in range(1,10)) sum := sum + x;
                print(sum);
            }
        ";
        let ir = lower_source_to_ir(src);
        assert_ir_contains(&ir, "call i1 @hulk_rt_range_next(ptr");
        assert_ir_contains(&ir, "call double @hulk_rt_range_current(ptr");
    }

    #[test]
    fn test_for_over_vector() {
        let src = "
            let sum = 0 in {
                for (x in [1,2,3]) sum := sum + x;
                print(sum);
            }
        ";
        let ir = lower_source_to_ir(src);
        assert_ir_contains(&ir, "for_cond:");
        assert_ir_contains(&ir, "call i1 @hulk_rt_vector_next(ptr");
        assert_ir_contains(&ir, "call ptr @hulk_rt_vector_current(ptr");
        assert_ir_contains(&ir, "call void @hulk_rt_internal_error()");
    }

    #[test]
    fn test_vector_index_primitive_null_check() {
        // let xs = [1,2,3] in xs[0]
        let xs = vector_lit(vec![num(1.0), num(2.0), num(3.0)], Type::Vector(Box::new(Type::Number)));
        let body = index_expr(var("xs", Type::Vector(Box::new(Type::Number))), num(0.0), Type::Number);
        let expr = let_expr(vec![("xs".to_string(), xs)], body, Type::Number);
        let ir = lower_expr_to_ir(expr);
        assert_ir_contains(&ir, "call ptr @hulk_rt_vector_get(ptr");
        assert_ir_contains(&ir, "br i1");
        assert_ir_contains(&ir, "unbox_null:");
        assert_ir_contains(&ir, "call void @hulk_rt_internal_error()");
    }

    #[test]
    fn test_vector_comprehension_source() {
        let src = "
            let xs = [x^2 | x in range(1,4)] in print(xs);
        ";
        let ir = lower_source_to_ir(src);
        assert_ir_contains(&ir, "call ptr @hulk_rt_dynamic_vector_new()");
        assert_ir_contains(&ir, "call void @hulk_rt_dynamic_vector_append(ptr");
        assert_ir_contains(&ir, "call ptr @hulk_rt_dynamic_vector_to_vector(ptr");
    }

    // ─── Pattern Matching ──────────────────────────────────────

    #[test]
    fn test_match_literal() {
        // match x { 1 => 10, 2 => 20, _ => 0 }
        let value = var("x", Type::Number);
        let cases = vec![
            match_case(Pattern::Literal(Literal::Number(1.0)), num(10.0)),
            match_case(Pattern::Literal(Literal::Number(2.0)), num(20.0)),
            match_case(Pattern::Wildcard, num(0.0)),
        ];
        let mat = match_expr(value, cases, Type::Number);
        // Bind x = 5 (any number) so the variable is defined.
        let expr = let_expr(vec![("x".to_string(), num(5.0))], mat, Type::Number);
        let ir = lower_expr_to_ir(expr);
        assert_ir_contains(&ir, "case_0_check:");
        assert_ir_contains(&ir, "fcmp oeq double");
        assert_ir_contains(&ir, "case_1_check:");
        assert_ir_contains(&ir, "case_2_check:"); // wildcard
        assert_ir_contains(&ir, "match_merge:");
        assert!(!ir.contains("hulk_rt_match_fail")); // exhaustive (wildcard)
    }

    #[test]
    fn test_match_non_exhaustive() {
        let value = var("x", Type::Number);
        let cases = vec![
            match_case(Pattern::Literal(Literal::Number(1.0)), num(10.0)),
            match_case(Pattern::Literal(Literal::Number(2.0)), num(20.0)),
        ];
        let mat = match_expr(value, cases, Type::Number);
        let expr = let_expr(vec![("x".to_string(), num(5.0))], mat, Type::Number);
        let ir = lower_expr_to_ir(expr);
        assert_ir_contains(&ir, "match_fail:");
        assert_ir_contains(&ir, "call void @hulk_rt_match_fail()");
    }

    #[test]
    fn test_match_type_pattern() {
        // let x: Object = ...; match x { case y: A => y; }
        // We need a type `A` in the registry.
        let src = "
            type A { }
            let x: Object = new A() in
            match x {
                case y: A => y;
            }
        ";
        let ir = lower_source_to_ir(src);
        assert_ir_contains(&ir, "case_0_check:");
        assert_ir_contains(&ir, "call i1 @hulk_rt_downcast_check");
        assert_ir_contains(&ir, "case_0_body:");
        assert_ir_contains(&ir, "store ptr");
        assert_ir_contains(&ir, "match_merge:");
    }

    #[test]
    fn test_match_string() {
        let src = "
            let s = \"hello\" in
            match s {
                case \"hello\" => 1;
                case \"world\" => 2;
                case _ => 0;
            }
        ";
        let ir = lower_source_to_ir(src);
        assert_ir_contains(&ir, "call i1 @hulk_rt_string_equals");
        assert_ir_contains(&ir, "match_merge:");
    }

    // ─── Protocols ──────────────────────────────────────

    #[test]
    fn test_protocol_dispatch() {
        let src = "
            protocol P { f(): Number; }
            type A { f(): Number => 1; }
            type B { f(): Number => 2; }
            {
                let x: P = new A() in print(x.f());
                let y: P = new B() in print(y.f());
            }
        ";
        let ir = lower_source_to_ir(src);
        // Should see itable load for P
        assert_ir_contains(&ir, "A__itable__P");
        assert_ir_contains(&ir, "B__itable__P");
        // The call should be indirect via itable
        assert_ir_contains(&ir, "call double %");
        // The itable should contain pointers to A::f and B::f
        assert_ir_contains(&ir, "A::f");
        assert_ir_contains(&ir, "B::f");
    }
}
