//! Pass 2: Type Inference
//!
//! This pass assigns a concrete `Type` to every expression and to every
//! symbol declaration that was not explicitly annotated. It builds the
//! `TypedProgram` node by node, using a post‑order traversal where each
//! visit returns the freshly built typed node and its type.
//!
//! The inference is **bounded**: we do not implement a general fixed‑point
//! solver. Instead, we use a simple constraint‑collection strategy for
//! unannotated parameters and a one‑pass‑with‑placeholder algorithm for
//! recursive functions.

use std::collections::{HashMap, HashSet};

use hulk_ast::{
    AssignExpr, AssignTarget, AttributeDecl, BinaryExpr, BinaryOp, BlockExpr, CallExpr,
    Declaration, DeclarationKind, DowncastExpr, ElifBranch, Expr, ExprKind, ForExpr, FunctionDecl,
    IfExpr, IndexExpr, LambdaExpr, LetBinding, LetExpr, Literal, MatchCase, MatchExpr, MemberExpr,
    NewExpr, Pattern, Program, SourceSpan, TypeDecl, TypeMember, TypeMemberKind, TypeParent,
    TypeRef, TypeTestExpr, UnaryExpr, UnaryOp, VectorComprehension, VectorExpr, VectorGenerator, 
    WhileExpr, MacroArg, MacroDecl, MacroCallExpr, MacroMatchExpr
};

use crate::environment::Environment;
use crate::error::{SemanticError, SemanticErrorKind};
use crate::passes::infer_utils::{patch_unknowns, recompute_annotations};
use crate::typed::{TypedExpr, TypedProgram};
use crate::types::registry::{MethodSignature, TypeRegistry};
use crate::types::{lowest_common_ancestor, Type};

// -----------------------------------------------------------------------------
// Public entry point
// -----------------------------------------------------------------------------

/// Runs type inference on the untyped program.
///
/// # Arguments
/// * `program` – The untyped AST (`Program<()>`).
/// * `registry` – The registry (mutated to fill in inferred return types).
/// * `errors` – Vector to append inference errors.
///
/// # Returns
/// A fully typed program (`Program<Type>`) with every expression annotated.
pub fn run(
    program: &Program,
    registry: &mut TypeRegistry,
    errors: &mut Vec<SemanticError>,
) -> TypedProgram {
    let mut state = InferState::new(registry, errors);

    // Infer each declaration.
    let mut typed_decls = Vec::new();
    for decl in &program.declarations {
        let typed = state.infer_declaration(decl);
        typed_decls.push(typed);
    }

    // Infer the entry expression in a fresh environment (no local variables).
    let mut env = Environment::new();
    let typed_entry = state.infer_expr(&program.entry, &mut env);

    TypedProgram::new(typed_decls, typed_entry)
}

// -----------------------------------------------------------------------------
// Inference state
// -----------------------------------------------------------------------------

/// Mutable state for the inference pass.
struct InferState<'a> {
    registry: &'a mut TypeRegistry,
    errors: &'a mut Vec<SemanticError>,
    /// The type of `self` when inside a method body.
    self_type: Option<Type>,
    /// The name of the currently inferred method (for `base` resolution).
    current_method_name: Option<String>,
    /// The type owner of the currently inferred method.
    current_type_owner: Option<String>,
    /// Stack of function names currently being inferred (for recursion detection).
    recursion_stack: HashSet<String>,
    /// Constraint map for unannotated parameters: parameter name -> candidate types.
    /// Keyed by the parameter name (which is unique within a function/method).
    param_constraints: HashMap<String, Vec<Type>>,
}

impl<'a> InferState<'a> {
    fn new(registry: &'a mut TypeRegistry, errors: &'a mut Vec<SemanticError>) -> Self {
        Self {
            registry,
            errors,
            self_type: None,
            current_method_name: None,
            current_type_owner: None,
            recursion_stack: HashSet::new(),
            param_constraints: HashMap::new(),
        }
    }

    // -------------------------------------------------------------------------
    // Declaration inference
    // -------------------------------------------------------------------------

    fn infer_declaration(&mut self, decl: &Declaration) -> Declaration<Type> {
        let span = decl.span;
        match &decl.kind {
            DeclarationKind::Function(f) => {
                let typed = self.infer_function(f);
                Declaration::new(DeclarationKind::Function(typed), span)
            }
            DeclarationKind::Type(t) => {
                let typed = self.infer_type(t, span);
                Declaration::new(DeclarationKind::Type(typed), span)
            }
            DeclarationKind::Protocol(p) => {
                // Protocols have no bodies, so they remain untyped.
                Declaration::new(DeclarationKind::Protocol(p.clone()), span)
            }
            DeclarationKind::Macro(m) => {
                // Should never be reached if hulk-transpile ran before the semantic pass.
                self.errors.push(SemanticError::error(
                    SemanticErrorKind::MacroReferenceFound {
                        macro_expr: m.name.clone(),
                    },
                    span,
                ));
                // Infer the macro body in a fresh environment (no local variables).
                let mut env = Environment::new();
                let typed_body = self.infer_expr(&m.body, &mut env);
                let typed_macro = MacroDecl::new(
                    m.name.clone(),
                    m.params.clone(),
                    m.return_type.clone(),
                    typed_body,
                );
                Declaration::new(DeclarationKind::Macro(typed_macro), span)
        }
        }
    }

    // -------------------------------------------------------------------------
    // Function inference (with recursive placeholder)
    // -------------------------------------------------------------------------

    /// Infers the type of a function or method declaration.
    ///
    /// If the function is a method (i.e., `self.self_type` is `Some`), the `self`
    /// symbol is bound in the environment before parameters are declared. This
    /// allows `self` to be shadowed by a parameter of the same name, matching
    /// HULK's semantics (§A.7.1). The body is inferred in this environment.
    ///
    /// The function's return type is inferred using the bounded placeholder
    /// strategy: if unannotated, the body's type is used (unless it remains
    /// `Unknown`, in which case `CannotInferType` is reported). Unannotated
    /// parameters are resolved from collected constraints.
    fn infer_function(&mut self, func: &FunctionDecl) -> FunctionDecl<Type> {
        let name = func.name.clone();
        let return_type_was_unknown = func.return_type.is_none();

        // Seed registry with Unknown for the return type if unannotated.
        if return_type_was_unknown {
            if let Some(sig) = self.registry.functions.get_mut(&name) {
                sig.return_type = Type::Unknown;
            }
        }

        self.param_constraints.clear();
        self.recursion_stack.insert(name.clone());

        let mut env = Environment::new();

        // Bind `self` if this is a method body (shadowable by a parameter named `self`).
        if let Some(self_ty) = &self.self_type {
            env.declare_with_self("self", self_ty.clone(), func.body.span, true);
        }

        // Declare parameters; unannotated parameters start as Unknown and collect constraints.
        for p in &func.params {
            let ty = p
                .type_annotation
                .as_ref()
                .map(|tr| self.resolve_type_ref(tr))
                .unwrap_or(Type::Unknown);
            env.declare(&p.name, ty.clone(), SourceSpan::new(0, 0));
            if p.type_annotation.is_none() {
                self.param_constraints.insert(p.name.clone(), Vec::new());
            }
        }

        // 1. Infer the body (placeholder Unknowns).
        let typed_body = self.infer_expr(&func.body, &mut env);
        self.recursion_stack.remove(&name);

        // 2. Resolve unannotated parameters from constraints.
        let mut resolved_param_types = Vec::new();
        for p in &func.params {
            let mut ty = p
                .type_annotation
                .as_ref()
                .map(|tr| self.resolve_type_ref(tr))
                .unwrap_or(Type::Unknown);
            if p.type_annotation.is_none() {
                if let Some(candidates) = self.param_constraints.get(&p.name) {
                    let mut unique = HashSet::new();
                    for c in candidates {
                        if !matches!(c, Type::Unknown | Type::Error) {
                            unique.insert(c.clone());
                        }
                    }
                    let unique_types: Vec<Type> = unique.into_iter().collect();
                    if unique_types.is_empty() {
                        self.errors.push(SemanticError::error(
                            SemanticErrorKind::CannotInferType {
                                symbol: p.name.clone(),
                            },
                            func.body.span,
                        ));
                        ty = Type::Error;
                    } else if unique_types.len() == 1 {
                        ty = unique_types[0].clone();
                    } else {
                        self.errors.push(SemanticError::error(
                            SemanticErrorKind::AmbiguousInference {
                                symbol: p.name.clone(),
                                candidates: unique_types,
                            },
                            func.body.span,
                        ));
                        ty = Type::Error;
                    }
                }
            }
            resolved_param_types.push((p.name.clone(), ty));
        }

        let param_map: HashMap<String, Type> = resolved_param_types.iter().cloned().collect();

        // 3. Patch variables with their resolved types (recursive calls remain Unknown).
        let mut patched_body = typed_body;
        patch_unknowns(&mut patched_body, &param_map, &name, &Type::Unknown);

        // 4. Recompute annotations of the whole body (this fixes intermediate nodes).
        recompute_annotations(&mut patched_body, self.registry);

        // 5. Determine final return type from the recomputed body.
        let resolved_return_type = if return_type_was_unknown {
            let body_type = patched_body.anno.clone();
            if matches!(body_type, Type::Unknown | Type::Error) {
                self.errors.push(SemanticError::error(
                    SemanticErrorKind::CannotInferType {
                        symbol: name.clone(),
                    },
                    func.body.span,
                ));
                Type::Error
            } else {
                body_type
            }
        } else {
            let annotated = func.return_type.as_ref().unwrap();
            let ann_type = self.resolve_type_ref(annotated);
            if !patched_body.anno.conforms_to(&ann_type, self.registry) {
                self.errors.push(SemanticError::error(
                    SemanticErrorKind::TypeMismatch {
                        expected: ann_type.clone(),
                        found: patched_body.anno.clone(),
                    },
                    func.body.span,
                ));
            }
            ann_type
        };

        // 6. Update registry with resolved parameter and return types.
        if let Some(sig) = self.registry.functions.get_mut(&name) {
            for (i, (_, ty)) in resolved_param_types.iter().enumerate() {
                if i < sig.params.len() {
                    sig.params[i].1 = ty.clone();
                }
            }
            sig.return_type = resolved_return_type.clone();
        }

        if let Some(owner) = &self.current_type_owner {
            if let Some(type_info) = self.registry.lookup_type_mut(owner) {
                if let Some(method_sig) = type_info.methods.get_mut(&name) {
                    // Update parameter types.
                    for (i, (_, ty)) in resolved_param_types.iter().enumerate() {
                        if i < method_sig.params.len() {
                            method_sig.params[i].1 = ty.clone();
                        }
                    }
                    // Update return type.
                    method_sig.return_type = resolved_return_type.clone();
                }
                // Also update flattened_methods.
                if let Some(flat_sig) = type_info.flattened_methods.get_mut(&name) {
                    for (i, (_, ty)) in resolved_param_types.iter().enumerate() {
                        if i < flat_sig.params.len() {
                            flat_sig.params[i].1 = ty.clone();
                        }
                    }
                    flat_sig.return_type = resolved_return_type.clone();
                }
            }
        }

        // 7. Patch recursive calls with the now‑known return type.
        patch_unknowns(&mut patched_body, &param_map, &name, &resolved_return_type);

        // Preserve original syntactic annotations in the typed AST.
        FunctionDecl::new(
            name,
            func.params.clone(),
            func.return_type.clone(),
            patched_body,
        )
    }

    // -------------------------------------------------------------------------
    // Type inference (attributes and methods)
    // -------------------------------------------------------------------------

    /// Infers the types of a type declaration: each attribute initializer,
    /// each method body, and the parent constructor arguments.
    ///
    /// Attributes are inferred in a scope containing only the type's own constructor
    /// parameters (not `self`, not sibling attributes). Methods are inferred with
    /// `self` bound to the type's name. Parent constructor arguments are inferred
    /// in a scope containing the inheriting type's own constructor parameters.
    /// Use the type declaration's span for error reporting.
    ///
    /// Errors for arity mismatches or type mismatches in parent constructor
    /// arguments are reported using the span of the entire type declaration.
    fn infer_type(&mut self, ty_decl: &TypeDecl, decl_span: SourceSpan) -> TypeDecl<Type> {
        let name = ty_decl.name.clone();

        // Perform two passes to ensure that every attribute is resolved before any method body sees it.

        // ─── First pass: infer all attributes ────────────────────────────────
        let mut typed_members = Vec::new();
        for member in &ty_decl.members {
            if let TypeMemberKind::Attribute(_) = &member.kind {
                let typed = self.infer_type_member(member, &name);
                typed_members.push(typed);
            }
        }

        // ─── Second pass: infer all methods ──────────────────────────────────
        for member in &ty_decl.members {
            if let TypeMemberKind::Method(_) = &member.kind {
                let typed = self.infer_type_member(member, &name);
                typed_members.push(typed);
            }
        }

        // Infer parent constructor arguments (if any) in a scope containing
        // the inheriting type's constructor parameters.
        let typed_parent = ty_decl.parent.as_ref().map(|p| {
            let mut env = Environment::new();
            // Declare the type's own constructor parameters.
            for param in &ty_decl.params {
                let ty = param
                    .type_annotation
                    .as_ref()
                    .map(|tr| self.resolve_type_ref(tr))
                    .unwrap_or(Type::Unknown);
                env.declare(&param.name, ty, SourceSpan::new(0, 0));
            }
            let args: Vec<TypedExpr> = p
                .args
                .iter()
                .map(|e| self.infer_expr(e, &mut env))
                .collect();
            let arg_types: Vec<Type> = args.iter().map(|e| e.anno.clone()).collect();

            // Check arity and conformance against parent constructor.
            let parent_info = self.registry.lookup_type(&p.name);
            if let Some(info) = parent_info {
                if arg_types.len() != info.params.len() {
                    self.errors.push(SemanticError::error(
                        SemanticErrorKind::ArityMismatch {
                            expected: info.params.len(),
                            found: arg_types.len(),
                        },
                        decl_span,
                    ));
                } else {
                    for ((_, expected), found) in info.params.iter().zip(arg_types) {
                        if !found.conforms_to(expected, self.registry) {
                            self.errors.push(SemanticError::error(
                                SemanticErrorKind::NotConforming {
                                    found: found.clone(),
                                    expected: expected.clone(),
                                },
                                decl_span,
                            ));
                        }
                    }
                }
            }
            TypeParent::new(&p.name, args)
        });

        TypeDecl::new(name, ty_decl.params.clone(), typed_parent, typed_members)
    }

    fn infer_type_member(&mut self, member: &TypeMember, type_name: &str) -> TypeMember<Type> {
        let span = member.span;
        match &member.kind {
            TypeMemberKind::Attribute(attr) => {
                // Attribute initializer is inferred in a scope with only
                // the type's constructor parameters (not self, not siblings).
                let mut env = Environment::new();
                // The type's params are not in the AST easily; we use the registry.
                if let Some(info) = self.registry.lookup_type(type_name) {
                    for (name, ty) in &info.params {
                        env.declare(name, ty.clone(), SourceSpan::new(0, 0));
                    }
                }
                let typed_init = self.infer_expr(&attr.initializer, &mut env);
                // Check annotation if present.
                let annotated = attr
                    .type_annotation
                    .as_ref()
                    .map(|tr| self.resolve_type_ref(tr));
                if let Some(ann) = &annotated {
                    if !typed_init.anno.conforms_to(ann, self.registry) {
                        self.errors.push(SemanticError::error(
                            SemanticErrorKind::TypeMismatch {
                                expected: ann.clone(),
                                found: typed_init.anno.clone(),
                            },
                            attr.initializer.span,
                        ));
                    }
                }
                // Store the inferred type in the registry for later lookup.
                if let Some(type_info) = self.registry.lookup_type_mut(type_name) {
                    if let Some(attr_info) = type_info.attributes.get_mut(&attr.name) {
                        attr_info.declared_type = Some(typed_init.anno.clone());
                    }
                }
                // Keep the original TypeRef annotation in the typed AST.
                let attr_typed =
                    AttributeDecl::new(&attr.name, attr.type_annotation.clone(), typed_init);
                TypeMember::new(TypeMemberKind::Attribute(attr_typed), span)
            }
            TypeMemberKind::Method(method) => {
                // Infer method body with self bound.
                let old_self = self.self_type.clone();
                let old_method_name = self.current_method_name.clone();
                let old_owner = self.current_type_owner.clone();

                self.self_type = Some(Type::Named(type_name.to_string()));
                self.current_method_name = Some(method.name.clone());
                self.current_type_owner = Some(type_name.to_string());

                let typed_method = self.infer_function(method);

                self.self_type = old_self;
                self.current_method_name = old_method_name;
                self.current_type_owner = old_owner;

                TypeMember::new(TypeMemberKind::Method(typed_method), span)
            }
        }
    }

    // -------------------------------------------------------------------------
    // Expression inference (core dispatcher)
    // -------------------------------------------------------------------------

    fn infer_expr(&mut self, expr: &Expr, env: &mut Environment) -> TypedExpr {
        let span = expr.span;
        match &expr.kind {
            ExprKind::Literal(lit) => self.infer_literal(lit, span),
            ExprKind::Variable(name) => self.infer_variable(name, span, env),
            ExprKind::SelfRef => self.infer_self_ref(span),
            ExprKind::BaseRef => self.infer_base_ref(span, env),
            ExprKind::Unary(unary) => self.infer_unary(unary, env),
            ExprKind::Binary(binary) => self.infer_binary(binary, env),
            ExprKind::Let(let_expr) => self.infer_let(let_expr, env),
            ExprKind::Assign(assign) => self.infer_assign(assign, env),
            ExprKind::Block(block) => self.infer_block(block, env),
            ExprKind::If(if_expr) => self.infer_if(if_expr, env),
            ExprKind::While(while_expr) => self.infer_while(while_expr, env),
            ExprKind::For(for_expr) => self.infer_for(for_expr, env),
            ExprKind::Call(call) => self.infer_call(call, env),
            ExprKind::Lambda(lambda) => self.infer_lambda(lambda, span, env),
            ExprKind::Member(member) => self.infer_member(member, env),
            ExprKind::New(new_expr) => self.infer_new(new_expr, span, env),
            ExprKind::TypeTest(type_test) => self.infer_type_test(type_test, span, env),
            ExprKind::Downcast(downcast) => self.infer_downcast(downcast, span, env),
            ExprKind::Vector(vector) => self.infer_vector(vector, env),
            ExprKind::Index(index) => self.infer_index(index, env),
            ExprKind::Match(match_expr) => self.infer_match(match_expr, env),
            ExprKind::MacroCall(mc) => {
                // Should never be reached if hulk-transpile ran before the semantic pass.
                self.errors.push(SemanticError::error(
                    SemanticErrorKind::MacroReferenceFound {
                        macro_expr: mc.name.clone(),
                    },
                    expr.span,
                ));

                // Infer any expression arguments; symbolic and placeholder args stay unchanged.
                let typed_args: Vec<MacroArg<Type>> = mc
                    .args
                    .iter()
                    .map(|arg| match arg {
                        MacroArg::Expr(e) => MacroArg::Expr(self.infer_expr(e, env)),
                        MacroArg::Symbolic(s) => MacroArg::Symbolic(s.clone()),
                        MacroArg::Placeholder(p) => MacroArg::Placeholder(p.clone()),
                    })
                    .collect();

                // Infer the trailing body block if present.
                let typed_body = mc.body.as_ref().map(|b| self.infer_expr(b, env));

                let mc_typed = MacroCallExpr {
                    name: mc.name.clone(),
                    args: typed_args,
                    body: typed_body.map(Box::new),
                };

                typed_expr(ExprKind::MacroCall(mc_typed), Type::Error, expr.span)
            }
            ExprKind::MacroMatch(_mm) => {
                self.errors.push(SemanticError::error(
                    SemanticErrorKind::MacroReferenceFound {
                        macro_expr: "macro match".to_string(),
                    },
                    expr.span,
                ));

                // Return a dummy MacroMatchExpr annotated with Error to satisfy the type system.
                let dummy_scrutinee = Expr {
                    kind: ExprKind::Variable("".to_string()),
                    anno: Type::Error,
                    span: expr.span,
                };
                let macro_match = MacroMatchExpr {
                    scrutinee: Box::new(dummy_scrutinee),
                    cases: Vec::new(),
                };
                typed_expr(ExprKind::MacroMatch(macro_match), Type::Error, expr.span)
            }
        }
    }

    // -------------------------------------------------------------------------
    // Literals
    // -------------------------------------------------------------------------

    fn infer_literal(&self, lit: &Literal, span: SourceSpan) -> TypedExpr {
        let ty = match lit {
            Literal::Number(_) => Type::Number,
            Literal::String(_) => Type::String,
            Literal::Boolean(_) => Type::Boolean,
        };
        typed_expr(ExprKind::Literal(lit.clone()), ty, span)
    }

    // -------------------------------------------------------------------------
    // Lambda expressions
    // -------------------------------------------------------------------------

    fn infer_lambda(
        &mut self,
        lambda: &LambdaExpr,
        span: SourceSpan,
        env: &mut Environment,
    ) -> TypedExpr {
        let mut lambda_env = env.clone();
        lambda_env.push_scope();

        let mut param_types = Vec::new();
        for param in &lambda.params {
            let ty = param
                .type_annotation
                .as_ref()
                .map(|tr| self.resolve_type_ref(tr))
                .unwrap_or(Type::Unknown);
            lambda_env.declare(&param.name, ty.clone(), span);
            param_types.push(ty);
        }

        let typed_body = self.infer_expr(&lambda.body, &mut lambda_env);

        let return_type = if let Some(annotation) = &lambda.return_type {
            let annotated = self.resolve_type_ref(annotation);
            if !typed_body.anno.conforms_to(&annotated, self.registry) {
                self.errors.push(SemanticError::error(
                    SemanticErrorKind::TypeMismatch {
                        expected: annotated.clone(),
                        found: typed_body.anno.clone(),
                    },
                    lambda.body.span,
                ));
            }
            annotated
        } else {
            typed_body.anno.clone()
        };

        let lambda_type = Type::Function {
            params: param_types,
            return_type: Box::new(return_type),
        };

        typed_expr(
            ExprKind::Lambda(LambdaExpr::new(
                lambda.params.clone(),
                lambda.return_type.clone(),
                typed_body,
            )),
            lambda_type,
            span,
        )
    }

    // -------------------------------------------------------------------------
    // Variables and Self/Base
    // -------------------------------------------------------------------------

    /// Looks up a variable name in the current environment.
    ///
    /// Returns the variable's resolved type if found. If not found, checks the
    /// registry for a zero‑arity function (global constant). Otherwise, reports
    /// `UndefinedVariable`.
    fn infer_variable(&mut self, name: &str, span: SourceSpan, env: &Environment) -> TypedExpr {
        if let Some(binding) = env.lookup(name) {
            let ty = binding.ty.clone();
            typed_expr(ExprKind::Variable(name.to_string()), ty, span)
        } else {
            // Special case: `base` inside an overriding method -> resolve to BaseRef
            if name == "base" && self.current_method_name.is_some() {
                return self.infer_base_ref(span, env);
            }
            // Check if it is a function in the registry.
            if let Some(sig) = self.registry.lookup_function(name) {
                if sig.is_constant {
                    // Constants are values, not callables.
                    let ty = sig.return_type.clone();
                    return typed_expr(ExprKind::Variable(name.to_string()), ty, span);
                } else {
                    // All other functions are callables.
                    let param_types = sig.params.iter().map(|(_, ty)| ty.clone()).collect();
                    let return_type = sig.return_type.clone();
                    let func_type = Type::Function {
                        params: param_types,
                        return_type: Box::new(return_type),
                    };
                    return typed_expr(ExprKind::Variable(name.to_string()), func_type, span);
                }
            }
            self.errors.push(SemanticError::error(
                SemanticErrorKind::UndefinedVariable(name.to_string()),
                span,
            ));
            typed_expr(ExprKind::Variable(name.to_string()), Type::Error, span)
        }
    }

    fn infer_self_ref(&mut self, span: SourceSpan) -> TypedExpr {
        if let Some(ty) = &self.self_type {
            typed_expr(ExprKind::SelfRef, ty.clone(), span)
        } else {
            self.errors.push(SemanticError::error(
                SemanticErrorKind::UndefinedVariable("self".to_string()),
                span,
            ));
            typed_expr(ExprKind::SelfRef, Type::Error, span)
        }
    }

    /// Resolves a `base` reference inside an overriding method.
    ///
    /// Returns the return type of the parent method if the current method overrides
    /// a parent method of the same name. Otherwise, reports
    /// `BaseOutsideOverridingMethod` and returns `Type::Error`.
    fn infer_base_ref(&mut self, span: SourceSpan, _env: &Environment) -> TypedExpr {
        if let (Some(owner), Some(method_name)) =
            (&self.current_type_owner, &self.current_method_name)
        {
            if let Some(info) = self.registry.lookup_type(owner) {
                if let Some(parent) = &info.parent {
                    if let Some(parent_info) = self.registry.lookup_type(&parent.name) {
                        if let Some(parent_sig) = parent_info.methods.get(method_name) {
                            let return_type = parent_sig.return_type.clone();
                            return typed_expr(ExprKind::BaseRef, return_type, span);
                        }
                    }
                }
            }
            self.errors.push(SemanticError::error(
                SemanticErrorKind::BaseOutsideOverridingMethod,
                span,
            ));
            typed_expr(ExprKind::BaseRef, Type::Error, span)
        } else {
            self.errors.push(SemanticError::error(
                SemanticErrorKind::BaseOutsideOverridingMethod,
                span,
            ));
            typed_expr(ExprKind::BaseRef, Type::Error, span)
        }
    }

    // -------------------------------------------------------------------------
    // Unary and Binary operators
    // -------------------------------------------------------------------------

    /// Infers the type of a unary expression.
    ///
    /// For `-`, the operand must be `Number` (or `Unknown` during inference);
    /// for `!`, the operand must be `Boolean` (or `Unknown` during inference).
    /// If the operand is `Unknown` and it is an unannotated parameter, a constraint
    /// is added to pin it to the required type.
    fn infer_unary(&mut self, unary: &UnaryExpr, env: &mut Environment) -> TypedExpr {
        let type_infered_expr = self.infer_expr(&unary.expr, env);
        let op = unary.op;
        let operand_type = type_infered_expr.anno.clone();

        let result_type = match op {
            UnaryOp::Negate => {
                if matches!(operand_type, Type::Number | Type::Unknown) {
                    self.constrain_if_variable(&type_infered_expr, Type::Number);
                    Type::Number
                } else {
                    self.errors.push(SemanticError::error(
                        SemanticErrorKind::InvalidOperator {
                            op: "-".to_string(),
                            operand_types: vec![operand_type],
                        },
                        unary.expr.span,
                    ));
                    Type::Error
                }
            }
            UnaryOp::Not => {
                if matches!(operand_type, Type::Boolean | Type::Unknown) {
                    self.constrain_if_variable(&type_infered_expr, Type::Boolean);
                    Type::Boolean
                } else {
                    self.errors.push(SemanticError::error(
                        SemanticErrorKind::InvalidOperator {
                            op: "!".to_string(),
                            operand_types: vec![operand_type],
                        },
                        unary.expr.span,
                    ));
                    Type::Error
                }
            }
        };
        typed_expr(
            ExprKind::Unary(UnaryExpr {
                op,
                expr: Box::new(type_infered_expr),
            }),
            result_type,
            unary.expr.span,
        )
    }

    /// Infers the type of a binary expression.
    ///
    /// Each operator has a fixed required operand type (or set of types).
    /// If an operand is `Type::Unknown` and corresponds to an unannotated parameter,
    /// a constraint is recorded so that the parameter can be inferred later.
    ///
    /// - Arithmetic operators require both operands to be `Number`.
    /// - Equality (`==`, `!=`) accepts `Number`, `String`, or `Boolean` operands.
    /// - Ordinal comparisons (`<`, `<=`, `>`, `>=`) require both operands `Number`.
    /// - Logical operators (`&`, `|`) require both operands `Boolean`.
    /// - Concatenation (`@`, `@@`) accepts operands of type `Number`, `String`, or `Boolean`.
    ///   No constraint is recorded because the required type is not unique.
    fn infer_binary(&mut self, binary: &BinaryExpr, env: &mut Environment) -> TypedExpr {
        let left = self.infer_expr(&binary.left, env);
        let right = self.infer_expr(&binary.right, env);
        let left_type = left.anno.clone();
        let right_type = right.anno.clone();

        let op = binary.op;
        let result_type = match op {
            BinaryOp::Add
            | BinaryOp::Subtract
            | BinaryOp::Multiply
            | BinaryOp::Divide
            | BinaryOp::Modulo
            | BinaryOp::Power => {
                let left_ok = matches!(left_type, Type::Number | Type::Unknown);
                let right_ok = matches!(right_type, Type::Number | Type::Unknown);
                if left_ok && right_ok {
                    self.constrain_if_variable(&left, Type::Number);
                    self.constrain_if_variable(&right, Type::Number);
                    Type::Number
                } else {
                    self.errors.push(SemanticError::error(
                        SemanticErrorKind::InvalidOperator {
                            op: format!("{:?}", op),
                            operand_types: vec![left_type, right_type],
                        },
                        binary.left.span,
                    ));
                    Type::Error
                }
            }
            // == and != apply to all primitive types per HULK spec §A.5;
            // ordinal comparisons (<, <=, >, >=) are Number-only and handled below.
            BinaryOp::Equal | BinaryOp::NotEqual => {
                let valid_eq = |t: &Type| {
                    matches!(
                        t,
                        Type::Number | Type::String | Type::Boolean | Type::Unknown
                    )
                };
                if valid_eq(&left_type) && valid_eq(&right_type) {
                    // Preserve Number constraint inference when both sides are numeric.
                    let is_numeric = |t: &Type| matches!(t, Type::Number | Type::Unknown);
                    if is_numeric(&left_type) && is_numeric(&right_type) {
                        self.constrain_if_variable(&left, Type::Number);
                        self.constrain_if_variable(&right, Type::Number);
                    }
                    Type::Boolean
                } else {
                    self.errors.push(SemanticError::error(
                        SemanticErrorKind::InvalidOperator {
                            op: format!("{:?}", op),
                            operand_types: vec![left_type, right_type],
                        },
                        binary.left.span,
                    ));
                    Type::Error
                }
            }
            BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual => {
                let left_ok = matches!(left_type, Type::Number | Type::Unknown);
                let right_ok = matches!(right_type, Type::Number | Type::Unknown);
                if left_ok && right_ok {
                    self.constrain_if_variable(&left, Type::Number);
                    self.constrain_if_variable(&right, Type::Number);
                    Type::Boolean
                } else {
                    self.errors.push(SemanticError::error(
                        SemanticErrorKind::InvalidOperator {
                            op: format!("{:?}", op),
                            operand_types: vec![left_type, right_type],
                        },
                        binary.left.span,
                    ));
                    Type::Error
                }
            }
            BinaryOp::And | BinaryOp::Or => {
                let left_ok = matches!(left_type, Type::Boolean | Type::Unknown);
                let right_ok = matches!(right_type, Type::Boolean | Type::Unknown);
                if left_ok && right_ok {
                    self.constrain_if_variable(&left, Type::Boolean);
                    self.constrain_if_variable(&right, Type::Boolean);
                    Type::Boolean
                } else {
                    self.errors.push(SemanticError::error(
                        SemanticErrorKind::InvalidOperator {
                            op: format!("{:?}", op),
                            operand_types: vec![left_type, right_type],
                        },
                        binary.left.span,
                    ));
                    Type::Error
                }
            }
            BinaryOp::Concat | BinaryOp::ConcatSpace => {
                let allowed = |t: &Type| {
                    matches!(
                        t,
                        Type::Number | Type::String | Type::Boolean | Type::Unknown
                    )
                };
                if allowed(&left_type) && allowed(&right_type) {
                    // No constraint: the required type is not unique (could be Number, String, or Boolean).
                    Type::String
                } else {
                    let op_str = if matches!(op, BinaryOp::Concat) {
                        "@"
                    } else {
                        "@@"
                    };
                    self.errors.push(SemanticError::error(
                        SemanticErrorKind::InvalidOperator {
                            op: op_str.to_string(),
                            operand_types: vec![left_type, right_type],
                        },
                        binary.left.span,
                    ));
                    Type::Error
                }
            }
        };
        typed_expr(
            ExprKind::Binary(BinaryExpr {
                op,
                left: Box::new(left),
                right: Box::new(right),
            }),
            result_type,
            binary.left.span,
        )
    }
    // -------------------------------------------------------------------------
    // Let expression
    // -------------------------------------------------------------------------

    /// Infers the type of a `let` expression.
    ///
    /// Creates a new lexical scope for the bindings. Each binding's initializer
    /// is inferred in the current environment (before the binding is declared),
    /// then the binding is added to the newly created scope. Subsequent bindings
    /// can see earlier ones. The body is inferred in the same scope, and the
    /// scope is popped after the body.
    ///
    /// The expression's type is the type of the body.
    fn infer_let(&mut self, let_expr: &LetExpr, env: &mut Environment) -> TypedExpr {
        // 1. Push a new scope for all let bindings.
        env.push_scope();

        let mut typed_bindings = Vec::new();

        // 2. Process bindings left‑to‑right.
        for binding in &let_expr.bindings {
            // 2a. Infer initializer in the current environment (before declaring this binding).
            let typed_init = self.infer_expr(&binding.initializer, env);
            let init_type = typed_init.anno.clone();

            // 2b. Determine the declared type: annotation or inferred.
            let declared_type = if let Some(ann) = &binding.type_annotation {
                let ann_type = self.resolve_type_ref(ann);
                if !init_type.conforms_to(&ann_type, self.registry) {
                    self.errors.push(SemanticError::error(
                        SemanticErrorKind::TypeMismatch {
                            expected: ann_type.clone(),
                            found: init_type,
                        },
                        binding.initializer.span,
                    ));
                    ann_type
                } else {
                    ann_type
                }
            } else {
                init_type
            };

            // 2c. Declare the binding in the current (new) scope.
            env.declare(
                &binding.name,
                declared_type.clone(),
                binding.initializer.span,
            );

            // 2d. Store the typed binding.
            typed_bindings.push(LetBinding::new(
                &binding.name,
                binding.type_annotation.clone(),
                typed_init,
            ));
        }

        // 3. Infer the body in the same scope (all bindings are now visible).
        let typed_body = self.infer_expr(&let_expr.body, env);

        // 4. Pop the scope after the body has been inferred.
        env.pop_scope();

        // 5. The let expression's type is the body's type.
        let body_type = typed_body.anno.clone();
        let let_typed = LetExpr::new(typed_bindings, typed_body);

        typed_expr(ExprKind::Let(let_typed), body_type, let_expr.body.span)
    }

    // -------------------------------------------------------------------------
    // Assignment
    // -------------------------------------------------------------------------

    /// Infers the type of a destructive assignment (`:=`).
    ///
    /// The target is resolved first to determine its declared type. The value is inferred,
    /// and its type must conform to the target's type. The whole expression's type is the
    /// type of the value (the assigned value).
    ///
    /// Errors are reported for: assigning to `self` (`SelfIsNotAssignable`), undefined
    /// variables, unknown members, index on non‑vector, and type mismatches.
    fn infer_assign(&mut self, assign: &AssignExpr, env: &mut Environment) -> TypedExpr {
        let typed_value = self.infer_expr(&assign.value, env);
        let value_type = typed_value.anno.clone();

        // Resolve target, passing the value's span as a fallback for target errors.
        let (target_ty, typed_target) =
            self.infer_assign_target(&assign.target, env, assign.value.span);

        if !value_type.conforms_to(&target_ty, self.registry) {
            self.errors.push(SemanticError::error(
                SemanticErrorKind::TypeMismatch {
                    expected: target_ty,
                    found: value_type.clone(),
                },
                assign.value.span,
            ));
        }

        let typed_assign = AssignExpr::new(typed_target, typed_value);
        typed_expr(
            ExprKind::Assign(typed_assign),
            value_type,
            assign.value.span,
        )
    }

    /// Resolves the target of an assignment and returns its type and a typed target.
    ///
    /// The target can be a variable, a member access, or an index access.
    /// For variables, the name is looked up in the environment; `self` is not assignable.
    /// For members, the member is looked up on the object's type.
    /// For indices, the object must be a `Vector` and the index must be `Number`.
    ///
    /// The `span` parameter is used as a fallback for errors that do not have a more
    /// specific span (e.g., `SelfIsNotAssignable`, `UndefinedVariable`).
    fn infer_assign_target(
        &mut self,
        target: &AssignTarget,
        env: &mut Environment,
        span: SourceSpan,
    ) -> (Type, AssignTarget<Type>) {
        match target {
            AssignTarget::Variable(name) => {
                if let Some(binding) = env.lookup(name) {
                    if binding.is_self {
                        self.errors.push(SemanticError::error(
                            SemanticErrorKind::SelfIsNotAssignable,
                            span,
                        ));
                        (Type::Error, AssignTarget::Variable(name.clone()))
                    } else {
                        let ty = binding.ty.clone();
                        (ty, AssignTarget::Variable(name.clone()))
                    }
                } else {
                    // Check if the name is a global function or type
                    if self.registry.lookup_function(name).is_some()
                        || self.registry.lookup_type(name).is_some()
                    {
                        self.errors.push(SemanticError::error(
                            SemanticErrorKind::InvalidAssignTarget,
                            span,
                        ));
                        (Type::Error, AssignTarget::Variable(name.clone()))
                    } else {
                        self.errors.push(SemanticError::error(
                            SemanticErrorKind::UndefinedVariable(name.clone()),
                            span,
                        ));
                        (Type::Error, AssignTarget::Variable(name.clone()))
                    }
                }
            }
            AssignTarget::Member { object, field } => {
                let typed_obj = self.infer_expr(object, env);
                if let Some((_owner_type, member_type)) = self.lookup_member(&typed_obj.anno, field)
                {
                    if matches!(member_type, Type::Function { .. }) {
                        self.errors.push(SemanticError::error(
                            SemanticErrorKind::AssignToMethod {
                                method: field.clone(),
                            },
                            object.span,
                        ));
                        (
                            Type::Error,
                            AssignTarget::Member {
                                object: Box::new(typed_obj),
                                field: field.clone(),
                            },
                        )
                    } else {
                        (
                            member_type,
                            AssignTarget::Member {
                                object: Box::new(typed_obj),
                                field: field.clone(),
                            },
                        )
                    }
                } else {
                    self.errors.push(SemanticError::error(
                        SemanticErrorKind::UnknownMember {
                            ty: typed_obj.anno.clone(),
                            member: field.clone(),
                        },
                        object.span,
                    ));
                    (
                        Type::Error,
                        AssignTarget::Member {
                            object: Box::new(typed_obj),
                            field: field.clone(),
                        },
                    )
                }
            }
            AssignTarget::Index { object, index } => {
                let typed_obj = self.infer_expr(object, env);
                let typed_idx = self.infer_expr(index, env);
                if let Type::Vector(inner) = &typed_obj.anno {
                    if matches!(typed_idx.anno, Type::Number) {
                        let elem_type = *inner.clone();
                        (
                            elem_type,
                            AssignTarget::Index {
                                object: Box::new(typed_obj),
                                index: Box::new(typed_idx),
                            },
                        )
                    } else {
                        self.errors.push(SemanticError::error(
                            SemanticErrorKind::TypeMismatch {
                                expected: Type::Number,
                                found: typed_idx.anno.clone(),
                            },
                            index.span,
                        ));
                        (
                            Type::Error,
                            AssignTarget::Index {
                                object: Box::new(typed_obj),
                                index: Box::new(typed_idx),
                            },
                        )
                    }
                } else {
                    self.errors.push(SemanticError::error(
                        SemanticErrorKind::IndexOnNonVector(typed_obj.anno.clone()),
                        object.span,
                    ));
                    (
                        Type::Error,
                        AssignTarget::Index {
                            object: Box::new(typed_obj),
                            index: Box::new(typed_idx),
                        },
                    )
                }
            }
        }
    }

    // -------------------------------------------------------------------------
    // Block
    // -------------------------------------------------------------------------

    fn infer_block(&mut self, block: &BlockExpr, env: &mut Environment) -> TypedExpr {
        let mut typed_exprs = Vec::new();
        let mut last_type = Type::Object; // default for empty block
        for expr in &block.expressions {
            let typed = self.infer_expr(expr, env);
            last_type = typed.anno.clone();
            typed_exprs.push(typed);
        }
        typed_expr(
            ExprKind::Block(BlockExpr::new(typed_exprs)),
            last_type,
            block
                .expressions
                .first()
                .map(|e| e.span)
                .unwrap_or(SourceSpan::new(0, 0)),
        )
    }

    // -------------------------------------------------------------------------
    // If
    // -------------------------------------------------------------------------

    fn infer_if(&mut self, if_expr: &IfExpr, env: &mut Environment) -> TypedExpr {
        let cond = self.infer_expr(&if_expr.condition, env);
        if !matches!(cond.anno, Type::Boolean) {
            self.errors.push(SemanticError::error(
                SemanticErrorKind::NonBooleanCondition(cond.anno.clone()),
                if_expr.condition.span,
            ));
        }
        let then_branch = self.infer_expr(&if_expr.then_branch, env);
        let mut elif_branches = Vec::new();
        for elif in &if_expr.elif_branches {
            let elif_cond = self.infer_expr(&elif.condition, env);
            if !matches!(elif_cond.anno, Type::Boolean) {
                self.errors.push(SemanticError::error(
                    SemanticErrorKind::NonBooleanCondition(elif_cond.anno.clone()),
                    elif.condition.span,
                ));
            }
            let elif_body = self.infer_expr(&elif.body, env);
            elif_branches.push(ElifBranch::new(elif_cond, elif_body));
        }
        let else_branch = self.infer_expr(&if_expr.else_branch, env);

        // Compute LCA of all branch types.
        let mut all_types = vec![then_branch.anno.clone()];
        for elif in &elif_branches {
            all_types.push(elif.body.anno.clone());
        }
        all_types.push(else_branch.anno.clone());
        let result_type = lowest_common_ancestor(&all_types, self.registry);

        let if_typed = IfExpr::new(cond, then_branch, elif_branches, else_branch);
        typed_expr(ExprKind::If(if_typed), result_type, if_expr.condition.span)
    }

    // -------------------------------------------------------------------------
    // While
    // -------------------------------------------------------------------------

    fn infer_while(&mut self, while_expr: &WhileExpr, env: &mut Environment) -> TypedExpr {
        let cond = self.infer_expr(&while_expr.condition, env);
        if !matches!(cond.anno, Type::Boolean) {
            self.errors.push(SemanticError::error(
                SemanticErrorKind::NonBooleanCondition(cond.anno.clone()),
                while_expr.condition.span,
            ));
        }
        let body = self.infer_expr(&while_expr.body, env);
        let result_type = body.anno.clone();
        let while_typed = WhileExpr::new(cond, body);
        typed_expr(
            ExprKind::While(while_typed),
            result_type,
            while_expr.condition.span,
        )
    }

    // -------------------------------------------------------------------------
    // For
    // -------------------------------------------------------------------------

    /// Infers the type of a `for` loop.
    ///
    /// The iterable expression is inferred first. The loop variable's type is
    /// determined by looking up the `current()` method on the iterable's type.
    /// This uses the generic method‑table helper in the registry, which handles
    /// `Vector<T>`, `Iterable<T>`, and any nominal type that structurally
    /// implements the `Iterable` protocol (including `Range` and user‑defined
    /// types). If `current()` is not found, a `NotIterable` error is reported.
    ///
    /// The loop variable is declared in a new scope, the body is inferred,
    /// and the result type is the body's type.
    fn infer_for(&mut self, for_expr: &ForExpr, env: &mut Environment) -> TypedExpr {
        let iterable = self.infer_expr(&for_expr.iterable, env);
        let iterable_type = iterable.anno.clone();

        // Determine the element type by looking up the `current()` method.
        let element_type =
            if let Some(method_sig) = self.registry.lookup_method(&iterable_type, "current") {
                method_sig.return_type
            } else {
                self.errors.push(SemanticError::error(
                    SemanticErrorKind::NotIterable(iterable_type.clone()),
                    for_expr.iterable.span,
                ));
                Type::Error
            };

        // Push scope, declare loop variable.
        env.push_scope();
        env.declare(&for_expr.var, element_type.clone(), for_expr.iterable.span);
        let body = self.infer_expr(&for_expr.body, env);
        env.pop_scope();

        let result_type = body.anno.clone();
        let for_typed = ForExpr::new(&for_expr.var, iterable, body);
        typed_expr(
            ExprKind::For(for_typed),
            result_type,
            for_expr.iterable.span,
        )
    }

    // -------------------------------------------------------------------------
    // Call
    // -------------------------------------------------------------------------

    /// Resolves a function or method call.
    ///
    /// If the callee is a variable name naming a global function, it is handled
    /// via the registry. If the callee is a `base` reference, it is handled as
    /// an overriding method call. Otherwise, the callee is inferred, and if its
    /// type is `Type::Function`, it is called with the provided arguments.
    /// Reports arity and type mismatches.
    fn infer_call(&mut self, call: &CallExpr, env: &mut Environment) -> TypedExpr {
        // ─── Infer the callee expression ─────────────────────────────────────
        let typed_callee = self.infer_expr(&call.callee, env);

        // ─── Special case: base() call ──────────────────────────────────────
        if let ExprKind::BaseRef = &typed_callee.kind {
            if let (Some(owner), Some(method_name)) =
                (&self.current_type_owner, &self.current_method_name)
            {
                if let Some(info) = self.registry.lookup_type(owner) {
                    if let Some(parent) = &info.parent {
                        if let Some(parent_info) = self.registry.lookup_type(&parent.name) {
                            if let Some(parent_sig) = parent_info.methods.get(method_name) {
                                let params: Vec<(String, Type)> = parent_sig.params.clone();
                                let return_type = parent_sig.return_type.clone();

                                let mut typed_args = Vec::new();
                                for arg in &call.args {
                                    typed_args.push(self.infer_expr(arg, env));
                                }
                                self.check_call_arity_and_types(
                                    &typed_args,
                                    &params,
                                    call.callee.span,
                                );
                                return typed_expr(
                                    ExprKind::Call(CallExpr {
                                        callee: Box::new(typed_callee),
                                        args: typed_args,
                                    }),
                                    return_type,
                                    call.callee.span,
                                );
                            }
                        }
                    }
                }
                // If we reach here, the base call is invalid (not an override).
                self.errors.push(SemanticError::error(
                    SemanticErrorKind::BaseOutsideOverridingMethod,
                    call.callee.span,
                ));
                let mut typed_args = Vec::new();
                for arg in &call.args {
                    typed_args.push(self.infer_expr(arg, env));
                }
                return typed_expr(
                    ExprKind::Call(CallExpr {
                        callee: Box::new(typed_callee),
                        args: typed_args,
                    }),
                    Type::Error,
                    call.callee.span,
                );
            }
            // Fallback (should not happen)
            self.errors.push(SemanticError::error(
                SemanticErrorKind::BaseOutsideOverridingMethod,
                call.callee.span,
            ));
            let mut typed_args = Vec::new();
            for arg in &call.args {
                typed_args.push(self.infer_expr(arg, env));
            }
            return typed_expr(
                ExprKind::Call(CallExpr {
                    callee: Box::new(typed_callee),
                    args: typed_args,
                }),
                Type::Error,
                call.callee.span,
            );
        }

        // ─── Generic callable: callee has Function type ─────────────────────
        if let Type::Function {
            params,
            return_type,
        } = typed_callee.anno.clone()
        {
            let mut typed_args = Vec::new();
            for arg in &call.args {
                typed_args.push(self.infer_expr(arg, env));
            }
            // Convert params to (String, Type) for arity/type checking.
            let param_names: Vec<String> = (0..params.len()).map(|i| format!("arg{}", i)).collect();
            let param_types: Vec<(String, Type)> = param_names.into_iter().zip(params).collect();
            self.check_call_arity_and_types(&typed_args, &param_types, call.callee.span);
            return typed_expr(
                ExprKind::Call(CallExpr {
                    callee: Box::new(typed_callee),
                    args: typed_args,
                }),
                *return_type,
                call.callee.span,
            );
        }

        // ─── Fallback: not callable ─────────────────────────────────────────
        self.errors.push(SemanticError::error(
            SemanticErrorKind::CallOnNonFunction {
                ty: typed_callee.anno.clone(),
            },
            call.callee.span,
        ));
        let mut typed_args = Vec::new();
        for arg in &call.args {
            typed_args.push(self.infer_expr(arg, env));
        }
        typed_expr(
            ExprKind::Call(CallExpr {
                callee: Box::new(typed_callee),
                args: typed_args,
            }),
            Type::Error,
            call.callee.span,
        )
    }

    /// Helper: checks arity and type conformance of call arguments.
    /// Pushes errors and adds parameter constraints for unannotated parameters.
    fn check_call_arity_and_types(
        &mut self,
        typed_args: &[TypedExpr],
        params: &[(String, Type)],
        span: SourceSpan,
    ) {
        if typed_args.len() != params.len() {
            self.errors.push(SemanticError::error(
                SemanticErrorKind::ArityMismatch {
                    expected: params.len(),
                    found: typed_args.len(),
                },
                span,
            ));
            return;
        }
        for (arg, (param_name, param_type)) in typed_args.iter().zip(params) {
            // Add constraint if argument is a variable and param type is concrete.
            if let ExprKind::Variable(var_name) = &arg.kind {
                if !matches!(param_type, Type::Unknown) {
                    self.add_constraint_if_parameter(var_name, param_type.clone());
                }
            }
            // If param is Unknown and argument is concrete, add constraint.
            if matches!(param_type, Type::Unknown)
                && !matches!(arg.anno, Type::Unknown | Type::Error)
            {
                self.add_constraint_if_parameter(param_name, arg.anno.clone());
            }
            // Conformance check.
            if !arg.anno.conforms_to(param_type, self.registry) {
                self.errors.push(SemanticError::error(
                    SemanticErrorKind::NotConforming {
                        found: arg.anno.clone(),
                        expected: param_type.clone(),
                    },
                    arg.span,
                ));
            }
        }
    }

    // -------------------------------------------------------------------------
    // Member
    // -------------------------------------------------------------------------

    /// Infers the type of a member access (`obj.member`).
    ///
    /// The object is inferred first. If the member is found (attribute or method),
    /// its type is returned. If the object expression is a variable with an
    /// `Unknown` type, and the member uniquely identifies a specific type in the
    /// registry, a constraint is added to infer the object's type.
    ///
    /// Attributes are private and will be checked in a later pass.
    fn infer_member(&mut self, member: &MemberExpr, env: &mut Environment) -> TypedExpr {
        let typed_obj = self.infer_expr(&member.object, env);
        let obj_type = typed_obj.anno.clone();

        if let Some((owner_type, member_type)) = self.lookup_member(&obj_type, &member.member) {
            // If the object is a variable with Unknown type, add a constraint.
            if matches!(obj_type, Type::Unknown) {
                self.constrain_if_variable(&typed_obj, owner_type);
            }
            let typed_member = MemberExpr::new(typed_obj, &member.member);
            typed_expr(
                ExprKind::Member(typed_member),
                member_type,
                member.object.span,
            )
        } else {
            self.errors.push(SemanticError::error(
                SemanticErrorKind::UnknownMember {
                    ty: obj_type,
                    member: member.member.clone(),
                },
                member.object.span,
            ));
            typed_expr(
                ExprKind::Member(MemberExpr {
                    object: Box::new(typed_obj),
                    member: member.member.clone(),
                }),
                Type::Error,
                member.object.span,
            )
        }
    }

    // -------------------------------------------------------------------------
    // New
    // -------------------------------------------------------------------------

    /// Infers the type of a `new` expression.
    ///
    /// The type name is resolved in the registry. Each constructor argument is
    /// inferred in the current environment. The arity and type of each argument
    /// are checked against the type's constructor parameters. If the type does
    /// not exist, an `UndefinedType` error is reported. The expression's type is
    /// the resolved `Named` type.
    ///
    /// Errors are reported using the `span` of the `new` expression itself.
    fn infer_new(
        &mut self,
        new_expr: &NewExpr,
        span: SourceSpan,
        env: &mut Environment,
    ) -> TypedExpr {
        // Special case: new vector with size.
        if let Some(size) = &new_expr.size {
            return self.infer_new_vector(new_expr, size, span, env);
        }

        let type_name = new_expr.type_name.name.clone();

        // Look up the type in the registry; clone needed data before mutating self.
        let params = match self.registry.lookup_type(&type_name) {
            Some(info) => info.params.clone(),
            None => {
                self.errors.push(SemanticError::error(
                    SemanticErrorKind::UndefinedType(type_name.clone()),
                    span,
                ));
                return typed_expr(
                    ExprKind::New(NewExpr::new(new_expr.type_name.clone(), Vec::new())),
                    Type::Error,
                    span,
                );
            }
        };

        // Infer each argument.
        let mut typed_args = Vec::new();
        for arg in &new_expr.args {
            typed_args.push(self.infer_expr(arg, env));
        }

        // Check arity.
        if typed_args.len() != params.len() {
            self.errors.push(SemanticError::error(
                SemanticErrorKind::ArityMismatch {
                    expected: params.len(),
                    found: typed_args.len(),
                },
                span,
            ));
        } else {
            // Check conformance of each argument.
            for (arg, (_, param_type)) in typed_args.iter().zip(&params) {
                if !arg.anno.conforms_to(param_type, self.registry) {
                    self.errors.push(SemanticError::error(
                        SemanticErrorKind::NotConforming {
                            found: arg.anno.clone(),
                            expected: param_type.clone(),
                        },
                        arg.span,
                    ));
                }
            }
        }

        let result_type = Type::Named(type_name.clone());
        typed_expr(
            ExprKind::New(NewExpr::new(new_expr.type_name.clone(), typed_args)),
            result_type,
            span,
        )
    }

    
    /// Infers `new ElemType[size]` / `new ElemType[size]{ i -> expr }`.
    /// 
    /// The size expression must be a `Number`. The declared element type is resolved
    /// from the `TypeRef`. If there's a generator, its body is type-checked against 
    /// the declared element type. The result type is `Vector<ElemType>`.
    fn infer_new_vector(
        &mut self,
        new_expr: &NewExpr,
        size: &Expr,
        span: SourceSpan,
        env: &mut Environment,
    ) -> TypedExpr {
            
        // Infer the size expression normally — its type should be Number and will be checked later.
        let typed_size = self.infer_expr(size, env);
        self.constrain_if_variable(&typed_size, Type::Number);

        // 2. Resolve the declared element type from the TypeRef built by the parser
        //    (reuses the exact same logic as `Number[]` annotations).
        let declared_elem_ty = self.resolve_type_ref(&new_expr.type_name);

        // 3. If there's a generator, type-check its body against the declared
        //    element type and use the typed generator in the result.
        let typed_generator = if let Some(gen) = &new_expr.generator {
            env.push_scope();
            env.declare(&gen.var, Type::Number, size.span); // index variable
            let typed_body = self.infer_expr(&gen.body, env);
            if !typed_body.anno.conforms_to(&declared_elem_ty, self.registry) {
                self.errors.push(SemanticError::error(
                    SemanticErrorKind::NotConforming {
                        found: typed_body.anno.clone(),
                        expected: declared_elem_ty.clone(),
                    },
                    gen.body.span,
                ));
            }
            env.pop_scope();
            Some(VectorGenerator::new(&gen.var, typed_body))
        } else {
            None
        };

        let result_type = Type::Vector(Box::new(declared_elem_ty));
        typed_expr(
            ExprKind::New(NewExpr::new_vector(
                new_expr.type_name.clone(),
                typed_size,
                typed_generator,
            )),
            result_type,
            span,
        )
    }

    // -------------------------------------------------------------------------
    // TypeTest (is)
    // -------------------------------------------------------------------------

    /// Infers the type of an `is` type test expression.
    ///
    /// The receiver expression is inferred first. If its type is a builtin value
    /// type (`Number`, `String`, `Boolean`), a `TypeMismatch` error is reported
    /// because such types have no dynamic subtyping. The target type is resolved
    /// in the registry; an `UndefinedType` error is reported if it does not exist.
    /// The expression's type is always `Boolean`.
    ///
    /// The `span` parameter is the span of the entire `is` expression, used for
    /// error reporting when the target type is undefined.
    fn infer_type_test(
        &mut self,
        type_test: &TypeTestExpr,
        span: SourceSpan,
        env: &mut Environment,
    ) -> TypedExpr {
        let type_infered_expr = self.infer_expr(&type_test.expr, env);
        let expr_type = type_infered_expr.anno.clone();
        // Check receiver is Object-rooted (not builtin value).
        match expr_type {
            Type::Number | Type::String | Type::Boolean => {
                self.errors.push(SemanticError::error(
                    SemanticErrorKind::TypeMismatch {
                        expected: Type::Object,
                        found: expr_type,
                    },
                    type_test.expr.span,
                ));
            }
            _ => {}
        }
        // Check type exists.
        let target_name = type_test.type_name.name.clone();
        if self.registry.lookup_type(&target_name).is_none()
            && self.registry.lookup_protocol(&target_name).is_none()
        {
            self.errors.push(SemanticError::error(
                SemanticErrorKind::UndefinedType(target_name.clone()),
                span,
            ));
        }
        typed_expr(
            ExprKind::TypeTest(TypeTestExpr::new(
                type_infered_expr,
                type_test.type_name.clone(),
            )),
            Type::Boolean,
            type_test.expr.span,
        )
    }

    // -------------------------------------------------------------------------
    // Downcast (as)
    // -------------------------------------------------------------------------

    /// Infers the type of an `as` downcast expression.
    ///
    /// The receiver expression is inferred first. Like `is`, if the receiver type
    /// is a builtin value type, a `TypeMismatch` error is reported. The target type
    /// is resolved in the registry; an `UndefinedType` error is reported if it does
    /// not exist. The expression's static type is the target type.
    ///
    /// If the receiver static type and the target type are unrelated in the
    /// hierarchy (neither conforms to the other), an `UnreachableDowncast` warning
    /// is emitted, as the cast can never succeed at runtime. This warning does not
    /// block compilation.
    ///
    /// The `span` parameter is the span of the entire `as` expression, used for
    /// error reporting when the target type is undefined.
    fn infer_downcast(
        &mut self,
        downcast: &DowncastExpr,
        span: SourceSpan,
        env: &mut Environment,
    ) -> TypedExpr {
        let type_infered_expr = self.infer_expr(&downcast.expr, env);
        let expr_type = type_infered_expr.anno.clone();
        // Receiver must be Object-rooted.
        match &expr_type {
            Type::Number | Type::String | Type::Boolean => {
                self.errors.push(SemanticError::error(
                    SemanticErrorKind::TypeMismatch {
                        expected: Type::Object,
                        found: expr_type.clone(),
                    },
                    downcast.expr.span,
                ));
            }
            _ => {}
        }
        let target_name = downcast.type_name.name.clone();
        let target_type = self.resolve_type_ref(&downcast.type_name);
        if self.registry.lookup_type(&target_name).is_none()
            && self.registry.lookup_protocol(&target_name).is_none()
        {
            self.errors.push(SemanticError::error(
                SemanticErrorKind::UndefinedType(target_name.clone()),
                span,
            ));
        }
        // Optional warning: unreachable downcast.
        if !expr_type.conforms_to(&target_type, self.registry)
            && !target_type.conforms_to(&expr_type, self.registry)
        {
            self.errors.push(SemanticError::warning(
                SemanticErrorKind::UnreachableDowncast {
                    from: expr_type.clone(),
                    to: target_type.clone(),
                },
                downcast.expr.span,
            ));
        }
        typed_expr(
            ExprKind::Downcast(DowncastExpr::new(
                type_infered_expr,
                downcast.type_name.clone(),
            )),
            target_type,
            downcast.expr.span,
        )
    }

    // -------------------------------------------------------------------------
    // Vector
    // -------------------------------------------------------------------------

    /// Infers the type of a vector literal or comprehension.
    ///
    /// For a literal, each element is inferred, and the element type is the
    /// lowest common ancestor of all element types (or `Unknown` if empty).
    ///
    /// For a comprehension, the iterable is inferred, and the bound variable’s
    /// type is determined by looking up the `current()` method on the iterable’s
    /// type via the registry’s generic method‑table helper. This handles
    /// `Vector<T>`, `Iterable<T>`, and any nominal type that structurally
    /// implements the `Iterable` protocol (e.g., `Range`). If `current()` is
    /// not found, a `NotIterable` error is reported.
    fn infer_vector(&mut self, vector: &VectorExpr, env: &mut Environment) -> TypedExpr {
        match vector {
            VectorExpr::Literal(items) => {
                let mut typed_items = Vec::new();
                for item in items {
                    typed_items.push(self.infer_expr(item, env));
                }
                let item_types: Vec<Type> = typed_items.iter().map(|e| e.anno.clone()).collect();
                let elem_type = if item_types.is_empty() {
                    Type::Unknown
                } else {
                    lowest_common_ancestor(&item_types, self.registry)
                };
                let result_type = Type::Vector(Box::new(elem_type));
                typed_expr(
                    ExprKind::Vector(VectorExpr::Literal(typed_items)),
                    result_type,
                    items
                        .first()
                        .map(|e| e.span)
                        .unwrap_or(SourceSpan::new(0, 0)),
                )
            }
            VectorExpr::Comprehension(comp) => {
                let typed_iterable = self.infer_expr(&comp.iterable, env);

                // Determine the element type by looking up `current()`.
                let elem_type = if let Some(method_sig) =
                    self.registry.lookup_method(&typed_iterable.anno, "current")
                {
                    method_sig.return_type
                } else {
                    self.errors.push(SemanticError::error(
                        SemanticErrorKind::NotIterable(typed_iterable.anno.clone()),
                        comp.iterable.span,
                    ));
                    Type::Error
                };
                // Push scope for the bound variable.
                env.push_scope();
                env.declare(&comp.var, elem_type.clone(), comp.iterable.span);
                let typed_head = self.infer_expr(&comp.expr, env);
                env.pop_scope();

                let result_type = Type::Vector(Box::new(typed_head.anno.clone()));
                let comp_typed = VectorComprehension::new(typed_head, &comp.var, typed_iterable);
                typed_expr(
                    ExprKind::Vector(VectorExpr::Comprehension(comp_typed)),
                    result_type,
                    comp.iterable.span,
                )
            }
        }
    }

    // -------------------------------------------------------------------------
    // Index
    // -------------------------------------------------------------------------

    fn infer_index(&mut self, index: &IndexExpr, env: &mut Environment) -> TypedExpr {
        let typed_obj = self.infer_expr(&index.object, env);
        let typed_idx = self.infer_expr(&index.index, env);
        let obj_type = typed_obj.anno.clone();
        let idx_type = typed_idx.anno.clone();

        if let Type::Vector(inner) = &obj_type {
            if matches!(idx_type, Type::Number) {
                let result_type = *inner.clone();
                typed_expr(
                    ExprKind::Index(IndexExpr::new(typed_obj, typed_idx)),
                    result_type,
                    index.object.span,
                )
            } else {
                self.errors.push(SemanticError::error(
                    SemanticErrorKind::TypeMismatch {
                        expected: Type::Number,
                        found: idx_type,
                    },
                    index.index.span,
                ));
                typed_expr(
                    ExprKind::Index(IndexExpr::new(typed_obj, typed_idx)),
                    Type::Error,
                    index.object.span,
                )
            }
        } else {
            self.errors.push(SemanticError::error(
                SemanticErrorKind::IndexOnNonVector(obj_type),
                index.object.span,
            ));
            typed_expr(
                ExprKind::Index(IndexExpr::new(typed_obj, typed_idx)),
                Type::Error,
                index.object.span,
            )
        }
    }

    // -------------------------------------------------------------------------
    // Match (project extension)
    // -------------------------------------------------------------------------

    /// Infers the type of a `match` expression (project extension).
    ///
    /// The scrutinee is inferred once, then each case pattern is checked against its type.
    /// Each case body is inferred in its own environment that includes any bindings from the pattern.
    /// The whole expression's type is the LCA of all case body types.
    ///
    /// A warning is emitted if no catch‑all pattern (wildcard or variable) is present.
    fn infer_match(&mut self, match_expr: &MatchExpr, env: &mut Environment) -> TypedExpr {
        let typed_value = self.infer_expr(&match_expr.value, env);
        let value_type = typed_value.anno.clone();

        let mut typed_cases = Vec::new();
        let mut case_types = Vec::new();
        let mut has_catch_all = false;

        for case in &match_expr.cases {
            // Pass the body span as the fallback for pattern errors.
            let (pattern_typed, mut case_env, catch) =
                self.infer_pattern(&case.pattern, &value_type, env, case.body.span);

            let typed_body = self.infer_expr(&case.body, &mut case_env);
            typed_cases.push(MatchCase::new(pattern_typed, typed_body.clone()));
            case_types.push(typed_body.anno.clone());

            if catch {
                has_catch_all = true;
            }
        }

        if !has_catch_all {
            self.errors.push(SemanticError::warning(
                SemanticErrorKind::NonExhaustiveMatch,
                match_expr.value.span,
            ));
        }

        let result_type = lowest_common_ancestor(&case_types, self.registry);
        let match_typed = MatchExpr::new(typed_value, typed_cases);
        typed_expr(
            ExprKind::Match(match_typed),
            result_type,
            match_expr.value.span,
        )
    }

    /// Checks a pattern against the scrutinee type and produces a typed pattern,
    /// an environment for the case body, and a flag indicating whether the pattern
    /// is a catch‑all (wildcard or bare variable).
    ///
    /// For literal patterns, the literal type must match the scrutinee exactly.
    /// For variable patterns, the variable is bound to the scrutinee type.
    /// For type patterns, the scrutinee must be `Object`‑rooted and the alias
    /// (if present) is bound to the narrowed type.
    ///
    /// The `span` parameter is used for error reporting and should point to the
    /// location of the pattern or its enclosing case body.
    fn infer_pattern(
        &mut self,
        pattern: &Pattern,
        scrutinee_type: &Type,
        env: &Environment,
        span: SourceSpan,
    ) -> (Pattern, Environment, bool) {
        let mut case_env = env.clone();
        let catch_all = match pattern {
            Pattern::Wildcard => (Pattern::Wildcard, true),

            Pattern::Literal(lit) => {
                let lit_type = match lit {
                    Literal::Number(_) => Type::Number,
                    Literal::String(_) => Type::String,
                    Literal::Boolean(_) => Type::Boolean,
                };
                if lit_type != *scrutinee_type {
                    self.errors.push(SemanticError::error(
                        SemanticErrorKind::TypeMismatch {
                            expected: scrutinee_type.clone(),
                            found: lit_type,
                        },
                        span,
                    ));
                }
                (Pattern::Literal(lit.clone()), false)
            }

            Pattern::Variable(name) => {
                case_env.declare(name, scrutinee_type.clone(), span);
                (Pattern::Variable(name.clone()), true)
            }

            Pattern::Type(type_ref, alias) => {
                // Scrutinee must be Object‑rooted (not a builtin value type).
                match scrutinee_type {
                    Type::Number | Type::String | Type::Boolean => {
                        self.errors.push(SemanticError::error(
                            SemanticErrorKind::TypeMismatch {
                                expected: Type::Object,
                                found: scrutinee_type.clone(),
                            },
                            span,
                        ));
                    }
                    _ => {}
                }

                let target_type = self.resolve_type_ref(type_ref);
                if let Some(alias_name) = alias {
                    case_env.declare(alias_name, target_type.clone(), span);
                }
                (Pattern::Type(type_ref.clone(), alias.clone()), false)
            }
        };
        (catch_all.0, case_env, catch_all.1)
    }

    // -------------------------------------------------------------------------
    // Helper: resolve TypeRef to Type
    // -------------------------------------------------------------------------

    fn resolve_type_ref(&self, tr: &TypeRef) -> Type {
        match tr.name.as_str() {
            "Number" => Type::Number,
            "String" => Type::String,
            "Boolean" => Type::Boolean,
            "Object" => Type::Object,
            _ => {
                if tr.args.is_empty() {
                    Type::Named(tr.name.clone())
                } else {
                    let args: Vec<Type> = tr
                        .args
                        .iter()
                        .map(|arg| self.resolve_type_ref(arg))
                        .collect();
                    match tr.name.as_str() {
                        "Vector" if !args.is_empty() => Type::Vector(Box::new(args[0].clone())),
                        "Iterable" if !args.is_empty() => Type::Iterable(Box::new(args[0].clone())),
                        "Function" if !args.is_empty() => {
                            let return_type = args.last().cloned().unwrap_or(Type::Object);
                            let params = args[..args.len() - 1].to_vec();
                            Type::Function {
                                params,
                                return_type: Box::new(return_type),
                            }
                        }
                        _ => Type::Named(tr.name.clone()),
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------------

    // Helper to look up a method on a type or protocol using TypeRegistry functionality.
    fn lookup_method(&self, ty: &Type, method_name: &str) -> Option<MethodSignature> {
        self.registry.lookup_method(ty, method_name)
    }

    /// Looks up a member (attribute or method) on a type.
    /// Returns `Some((owner_type, member_type))` if found, where `owner_type`
    /// is the type that defines the member and `member_type` is the type of the
    /// member (attribute type or method return type).
    fn lookup_member(&self, ty: &Type, member_name: &str) -> Option<(Type, Type)> {
        match ty {
            Type::Named(name) => {
                // 1. Attribute lookup (only on classes).
                if let Some(info) = self.registry.lookup_type(name) {
                    if let Some(attr) = info.attributes.get(member_name) {
                        if let Some(declared) = &attr.declared_type {
                            return Some((Type::Named(name.clone()), declared.clone()));
                        }
                    }
                }
                // 2. Method lookup -> return Function type.
                if let Some(method_sig) = self.lookup_method(ty, member_name) {
                    let param_types: Vec<Type> =
                        method_sig.params.iter().map(|(_, t)| t.clone()).collect();
                    let return_type = method_sig.return_type.clone();
                    let func_type = Type::Function {
                        params: param_types,
                        return_type: Box::new(return_type),
                    };
                    return Some((Type::Named(name.clone()), func_type));
                }
                None
            }
            // For Vector and Iterable, we still need method lookup (no attributes).
            Type::Vector(_) | Type::Iterable(_) => {
                if let Some(method_sig) = self.lookup_method(ty, member_name) {
                    let param_types: Vec<Type> =
                        method_sig.params.iter().map(|(_, t)| t.clone()).collect();
                    let return_type = method_sig.return_type.clone();
                    let func_type = Type::Function {
                        params: param_types,
                        return_type: Box::new(return_type),
                    };
                    return Some((ty.clone(), func_type));
                }
                None
            }
            _ => None,
        }
    }

    /// If `name` is an unannotated parameter, record that it must be compatible with `ty`.
    fn add_constraint_if_parameter(&mut self, name: &str, ty: Type) {
        if let Some(constraints) = self.param_constraints.get_mut(name) {
            constraints.push(ty);
        }
    }

    /// If the expression is a variable, and it is an unannotated parameter, add a constraint.
    fn constrain_if_variable(&mut self, expr: &TypedExpr, required_type: Type) {
        if let ExprKind::Variable(name) = &expr.kind {
            self.add_constraint_if_parameter(name, required_type);
        }
    }
}

/// Helper to create a typed expression with the given kind, annotation, and span.
fn typed_expr(kind: ExprKind<Type>, anno: Type, span: SourceSpan) -> TypedExpr {
    TypedExpr { kind, anno, span }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze;
    use crate::error::Severity;
    use crate::passes::{collect, hierarchy, infer};
    use crate::seeded_registry;
    use hulk_lexer::Lexer;
    use hulk_parser::parse;

    fn parse_and_infer(src: &str) -> (TypeRegistry, Vec<SemanticError>) {
        let tokens = Lexer::new(src).tokenize().expect("lex ok");
        let program = parse(tokens).expect("parse ok");
        let mut registry = seeded_registry();
        let mut errors = Vec::new();
        collect::run(&program, &mut registry, &mut errors);
        if errors.iter().any(|e| e.severity == Severity::Error) {
            return (registry, errors);
        }
        hierarchy::run(&mut registry, &mut errors);
        if errors.iter().any(|e| e.severity == Severity::Error) {
            return (registry, errors);
        }
        let _ = infer::run(&program, &mut registry, &mut errors);
        (registry, errors)
    }

    // ---- Name resolution ----
    #[test]
    fn undefined_variable() {
        let src = "print(x);";
        let (_, errors) = parse_and_infer(src);
        assert!(errors.iter().any(
            |e| matches!(e.kind, SemanticErrorKind::UndefinedVariable(ref name) if name == "x")
        ));
    }

    #[test]
    fn self_ref_inside_method() {
        let src = "type A { f() => self; } print (0);";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(
            result.is_ok(),
            "self reference should work: {:?}",
            result.err()
        );
    }

    #[test]
    fn shadowing_example_from_spec() {
        // §A.4.5: let a = 20 in { let a = 42 in print(a); print(a); }
        let src = "let a = 20 in { let a = 42 in print(a); print(a); }";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok(), "shadowing should be allowed");
    }

    // ---- Operators ----
    #[test]
    fn arithmetic_operators() {
        let src = "print(1 + 2 * 3 / 4 - 5);";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok());
    }

    #[test]
    fn comparison_operators() {
        let src = "print(1 < 2 & 3 >= 4);";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok());
    }

    #[test]
    fn invalid_operator_operands() {
        let src = "print(\"hello\" + 1);";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_err());
        let errs = result.err().unwrap();
        assert!(errs
            .iter()
            .any(|e| matches!(e.kind, SemanticErrorKind::InvalidOperator { .. })));
    }

    // ---- Control flow ----
    #[test]
    fn if_expression_branch_unification() {
        let src = "print(if (true) 1 else 2);";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok());
    }

    #[test]
    fn non_boolean_condition() {
        let src = "if (42) print(1) else print(2);";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_err());
        let errs = result.err().unwrap();
        assert!(errs
            .iter()
            .any(|e| matches!(e.kind, SemanticErrorKind::NonBooleanCondition(Type::Number))));
    }

    #[test]
    fn for_loop_over_range() {
        let src = "
            let sum = 0 in {
                for (x in range(1,4)) sum := sum + x;
                print(sum);
            }
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok(), "for loop over range should work");
    }

    // ---- Inference examples from §A.9.4 ----
    #[test]
    fn fib_inference() {
        let src = "
            function fib(n) => if (n == 0 | n == 1) 1 else fib(n-1) + fib(n-2);
            print(fib(10));
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok(), "fib inference failed: {:?}", result.err());
    }

    #[test]
    fn fact_inference() {
        let src = "
            function fact(x) => let f = 1 in for (i in range(1, x+1)) f := f * i;
            print(fact(5));
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok());
    }

    #[test]
    fn let_x_42_inference() {
        let src = "let x = 42 in print(x);";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok());
    }

    // ---- Vectors ----
    #[test]
    fn vector_literal_lca() {
        let src = "let v = [1, 2, 3] in print(v[0]);";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok());
    }

    #[test]
    fn vector_comprehension() {
        let src = "let xs = [x^2 | x in range(1,10)] in print(xs[0]);";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok(), "vector comprehension should work");
    }

    #[test]
    fn index_on_non_vector() {
        let src = "let x = 42 in print(x[0]);";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_err());
        let errs = result.err().unwrap();
        assert!(errs
            .iter()
            .any(|e| matches!(e.kind, SemanticErrorKind::IndexOnNonVector(Type::Number))));
    }

    // ---- is/as/match ----
    #[test]
    fn is_expression() {
        let src = "
            type A { }
            let x = new A() in print(x is A);
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok());
    }

    #[test]
    fn downcast_unreachable_warning() {
        let src = "
            type A { }
            type B { }
            let x = new A() in let y = x as B in print(y);
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok()); // warning, not error
        let warnings = result.unwrap().warnings;
        assert!(warnings.iter().any(|e| matches!(e.kind, SemanticErrorKind::UnreachableDowncast { from: Type::Named(ref a), to: Type::Named(ref b) } if a == "A" && b == "B")));
    }

    #[test]
    fn match_non_exhaustive_warning() {
        let src = "
            let x = 5 in match x {
                case 1 => print(1);
                case 2 => print(2);
            }
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok());
        let warnings = result.unwrap().warnings;
        assert!(warnings
            .iter()
            .any(|e| matches!(e.kind, SemanticErrorKind::NonExhaustiveMatch)));
    }

    // ---- Recursive unresolvable ----
    #[test]
    fn unresolvable_recursion() {
        // A function that only calls itself without base case
        let src = "function loop() => loop(); print(loop());";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_err());
        let errs = result.err().unwrap();
        assert!(errs.iter().any(
            |e| matches!(&e.kind, SemanticErrorKind::CannotInferType { symbol } if symbol == "loop")
        ));
    }

    // ---- Extended tests ----

    /// Tests that a method calling itself via `self` with an unannotated return type
    /// infers correctly, and that `patch_unknowns` handles Member callees.
    #[test]
    fn recursive_method_self_call_unannotated_return_type() {
        let src = "
            type Counter(n) {
                n = n;
                tick() => if (self.n == 0) 0 else self.tick();
            }
            let c = new Counter(5) in print(c.tick());
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(
            result.is_ok(),
            "recursive self‑call via self should work: {:?}",
            result.err()
        );
    }

    /// Tests that mutual recursion between two functions without explicit return types
    /// is currently a known limitation. This test documents the gap and ensures it fails
    /// predictably rather than silently producing wrong results.
    #[test]
    fn mutual_recursion_two_functions() {
        let src = "
            function isEven(n) => if (n == 0) true else isOdd(n-1);
            function isOdd(n) => if (n == 0) false else isEven(n-1);
            print(isEven(4));
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(
            result.is_err(),
            "mutual recursion should fail with CannotInferType"
        );
        let errors = result.err().unwrap();
        // At least one of the functions should have a CannotInferType error.
        assert!(
            errors
                .iter()
                .any(|e| matches!(e.kind, SemanticErrorKind::CannotInferType { .. })),
            "expected CannotInferType for mutual recursion"
        );
    }

    /// Tests that an unannotated function parameter used in two incompatible contexts
    /// inside the same body produces an `AmbiguousInference` error.
    #[test]
    fn ambiguous_function_parameter() {
        // The parameter `x` is used in arithmetic (requires Number) and as an argument
        // to a function expecting a class type `A` (requires `x` to be `A` or a subtype).
        let src = "
            type A { }
            function g(y: A) => y;
            function f(x) => if (true) x + 1 else g(x);
            print(f(42));
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_err(), "expected ambiguity error");
        let errors = result.err().unwrap();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e.kind, SemanticErrorKind::AmbiguousInference { .. })),
            "missing AmbiguousInference error; got errors: {:?}",
            errors
        );
    }

    /// Tests that `patch_unknowns` correctly handles deep member‑access chains.
    /// A method that returns `self` and is called multiple times creates nested `Member`
    /// expressions; all of them must be patched without panicking or leaving `Unknown`.
    #[test]
    fn deep_member_chain_recursion() {
        let src = "
            type A {
                f(): A => self;
                g(): A => self.f().f().g();
            }
            let x = new A().f().f().g() in print(x);
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(
            result.is_ok(),
            "deep member chain should type‑check: {:?}",
            result.err()
        );
    }

    /// Tests that vector literals containing instances of a type whose constructor
    /// parameters are resolved (via Pass 1.5) correctly produce a vector type with
    /// the resolved element type, rather than leaving `Unknown` in the LCA.
    #[test]
    fn vector_of_unannotated_constructor_type() {
        let src = "
            type Person(firstname, lastname) {
                firstname = firstname;
                lastname = lastname;
            }
            let v = [new Person(\"a\", \"b\"), new Person(\"c\", \"d\")] in print(v);
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        if let Err(errs) = &result {
            println!("Errors: {:#?}", errs);
        }
        assert!(
            result.is_ok(),
            "vector of unresolved constructor types should work: {:?}",
            result.err()
        );

        // Verify that the vector's element type is `Named("Person")`.
        let typed_program = result.unwrap().typed_program;
        let entry = &typed_program.entry;

        // Entry is a LetExpr: let v = [new Person(...), new Person(...)] in print(v)
        if let ExprKind::Let(let_expr) = &entry.kind {
            // The binding initializer is the vector literal.
            let binding = &let_expr.bindings[0];
            if let ExprKind::Vector(vector) = &binding.initializer.kind {
                match vector {
                    VectorExpr::Literal(_) => {
                        if let Type::Vector(inner) = &binding.initializer.anno {
                            assert_eq!(**inner, Type::Named("Person".to_string()));
                        } else {
                            panic!(
                                "vector type should be Vector, got {:?}",
                                binding.initializer.anno
                            );
                        }
                    }
                    _ => panic!("expected vector literal"),
                }
            } else {
                panic!("expected vector expression as initializer");
            }
        } else {
            panic!("entry expression is not a let");
        }
    }

    // ---- Generic method lookup for Vector and Iterable ----

    #[test]
    fn vector_size_method_call() {
        let src = "let v = [1,2,3] in print(v.size());";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok(), "v.size() should resolve");
    }

    #[test]
    fn vector_get_method_call() {
        let src = "let v = [1,2,3] in print(v.get(1));";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok(), "v.get(1) should resolve");
    }

    #[test]
    fn vector_set_method_call() {
        let src = "let v = [1,2,3] in v.set(1, 42);";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok(), "v.set(1, 42) should resolve");
    }

    #[test]
    fn vector_current_method_call() {
        // Vector implements Iterable, so current() should be available.
        let src = "let v = [1,2,3] in print(v.current());";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok(), "v.current() should resolve");
    }

    #[test]
    fn iterable_protocol_method_call() {
        let src = "
            type T {
                next(): Boolean => true;
                current(): Number => 42;
            }
            let it: Iterable<Number> = new T() in print(it.current());
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(
            result.is_ok(),
            "it.current() should resolve with correct return type"
        );
    }

    #[test]
    fn for_loop_over_vector() {
        let src = "
            let sum = 0 in {
                for (x in [1,2,3]) sum := sum + x;
                print(sum);
            }
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok(), "for loop over vector should work");
    }

    #[test]
    fn for_loop_over_user_iterable() {
        let src = "
            type MyIter {
                next(): Boolean => true;
                current(): Number => 42;
            }
            let sum = 0 in {
                for (x in new MyIter()) sum := sum + x;
                print(sum);
            }
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(
            result.is_ok(),
            "for loop over user-defined iterable should work"
        );
    }

    #[test]
    fn vector_index_assignment() {
        let src = "let v = [1,2,3] in { v[0] := 42; print(v[0]); }";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok(), "index assignment should work");
    }

    // ---- Error cases ----

    #[test]
    fn method_not_found_on_vector() {
        let src = "let v = [1,2,3] in print(v.foo());";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_err(), "calling unknown method should fail");
        let errors = result.err().unwrap();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e.kind, SemanticErrorKind::UnknownMember { .. })),
            "expected UnknownMember error"
        );
    }

    #[test]
    fn current_on_non_iterable() {
        let src = "
            type A { }
            let x = new A() in print(x.current());
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(
            result.is_err(),
            "calling current() on non-iterable should fail"
        );
        let errors = result.err().unwrap();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e.kind, SemanticErrorKind::UnknownMember { .. })),
            "expected UnknownMember error"
        );
    }

    #[test]
    fn for_loop_on_non_iterable() {
        let src = "
            type A { }
            for (x in new A()) print(x);
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_err(), "for loop on non-iterable should fail");
        let errors = result.err().unwrap();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e.kind, SemanticErrorKind::NotIterable(_))),
            "expected NotIterable error"
        );
    }

    #[test]
    fn type_of_loop_variable_is_correct() {
        // We check that the loop variable is inferred as Number by using it in a context
        // that requires Number (e.g., addition).
        let src = "
            let sum = 0 in {
                for (x in [1,2,3]) sum := sum + x;  // x must be Number
                print(sum);
            }
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(
            result.is_ok(),
            "loop variable should be Number and usable in arithmetic"
        );
    }

    // ─── Function type / method reference tests ────────────────────────────

    /// Tests that a method reference can be stored and called later.
    #[test]
    fn method_reference_stored_and_called() {
        let src = "
            type Counter(n) {
                n = n;
                tick(): Number => self.n + 1;
            }
            let c = new Counter(5) in {
                let f = c.tick in   // method reference
                print(f());
            }
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(
            result.is_ok(),
            "method reference should work: {:?}",
            result.err()
        );
    }

    /// Tests that a method reference can be called with arguments.
    #[test]
    fn method_reference_with_arguments() {
        let src = "
            type Calc {
                add(x: Number, y: Number): Number => x + y;
            }
            let c = new Calc() in {
                let f = c.add in
                print(f(3, 4));
            }
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(
            result.is_ok(),
            "method reference with args should work: {:?}",
            result.err()
        );
    }

    /// Tests that assigning to a method is rejected.
    #[test]
    fn assign_to_method_rejected() {
        let src = "
            type Counter(n) {
                n = n;
                tick(): Number => self.n + 1;
            }
            let c = new Counter(5) in {
                c.tick := 42;   // ERROR: cannot assign to method
                print(c.tick());
            }
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_err(), "assignment to method should fail");
        let errors = result.err().unwrap();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e.kind, SemanticErrorKind::AssignToMethod { .. })),
            "expected AssignToMethod error; got: {:?}",
            errors
        );
    }

    /// Tests that calling a non‑function value is rejected.
    #[test]
    fn call_on_non_function_rejected() {
        let src = "
            let x = 5 in
            x();   // ERROR: cannot call number
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_err(), "calling a non‑function should fail");
        let errors = result.err().unwrap();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e.kind, SemanticErrorKind::CallOnNonFunction { .. })),
            "expected CallOnNonFunction error; got: {:?}",
            errors
        );
    }

    /// Tests that a method reference is correctly typed as a function
    /// by using it in a context that expects a function type.
    #[test]
    fn method_reference_type_is_function() {
        let src = "
            type A {
                f(): Number => 42;
            }
            let a = new A() in {
                let g = a.f in {
                    print(g());
                }
            }
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(
            result.is_ok(),
            "method reference with function type annotation should work: {:?}",
            result.err()
        );
    }

    /// Validates that multi‑function inference (where the return type of one function
    /// is used as an argument to another) works correctly after types recomputation.
    #[test]
    fn min_max_clamp_inference() {
        let src = r#"
            function min_val(a, b) {
                if (a < b) a else b;
            }

            function max_val(a, b) {
                if (a > b) a else b;
            }

            function clamp(val, lo, hi) {
                min_val(max_val(val, lo), hi);
            }

            {
                if (min_val(3, 5) == 3) print("ok") else print("fail");
                if (max_val(3, 5) == 5) print("ok") else print("fail");
                if (clamp(10, 0, 5) == 5) print("ok") else print("fail");
                if (clamp(-3, 0, 5) == 0) print("ok") else print("fail");
                if (clamp(3, 0, 5) == 3) print("ok") else print("fail");
            }
        "#;

        let tokens = Lexer::new(src).tokenize().unwrap();
        let program = parse(tokens).unwrap();
        let result = analyze(&program);

        assert!(
            result.is_ok(),
            "min/max/clamp should infer correctly; got errors: {:?}",
            result.err()
        );

        // Optional: verify the inferred return types are Number.
        let verified = result.unwrap();
        let registry = &verified.registry;

        for name in ["min_val", "max_val", "clamp"] {
            let sig = registry
                .lookup_function(name)
                .expect("function should exist");
            assert_eq!(
                sig.return_type,
                Type::Number,
                "{} should return Number, got {:?}",
                name,
                sig.return_type
            );
        }
    }
}
