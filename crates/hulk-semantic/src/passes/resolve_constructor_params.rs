//! Pass 1.5: Resolve Constructor Parameters
//!
//! This pass resolves unannotated type constructor parameters using constraints
//! collected from `new` expressions and `inherits` clauses. It runs after
//! hierarchy resolution (Pass 1) and before type inference (Pass 2).

use std::collections::{HashMap, HashSet};

use hulk_ast::{DeclarationKind, Expr, ExprKind, Literal, Program, TypeMemberKind, MacroArg};

use crate::error::{SemanticError, SemanticErrorKind};
use crate::passes::utils::topological_order;
use crate::types::registry::TypeRegistry;
use crate::types::Type;

/// Runs the constructor parameter resolution pass.
///
/// # Arguments
/// * `program` – The untyped AST.
/// * `registry` – The registry (mutated: `TypeInfo.params` are updated).
/// * `errors` – Vector to append any inference errors.
pub fn run(program: &Program, registry: &mut TypeRegistry, errors: &mut Vec<SemanticError>) {
    // Step 1: Collect constraints from all `new` expressions.
    let mut constraints: HashMap<(String, usize), Vec<Type>> = HashMap::new();
    collect_new_constraints(program, errors, registry, &mut constraints);

    // Step 2: Get topological order (parents first) and reverse it.
    let order = topological_order(registry);
    for type_name in order.iter().rev() {
        // 2a. Resolve this type's own unannotated parameters.
        resolve_type_params(type_name, registry, &mut constraints, errors);

        // 2b. Propagate resolved parameters to parent constructor.
        propagate_to_parent(type_name, registry, &mut constraints);
    }
}

/// Collects constraints from `new T(args)` expressions.
///
/// Only literals and references to already‑resolved constructor parameters
/// (handled later) produce useful constraints; other expressions are ignored.
fn collect_new_constraints(
    program: &Program,
    errors: &mut Vec<SemanticError>,
    registry: &TypeRegistry,
    
    constraints: &mut HashMap<(String, usize), Vec<Type>>,
) {
    traverse_exprs(program, errors, |expr| {
        if let ExprKind::New(new_expr) = &expr.kind {
            let type_name = new_expr.type_name.name.clone();
            // Only if the type exists in the registry.
            if let Some(_info) = registry.lookup_type(&type_name) {
                for (idx, arg) in new_expr.args.iter().enumerate() {
                    if let Some(ty) = infer_literal_or_known_param(arg) {
                        constraints
                            .entry((type_name.clone(), idx))
                            .or_default()
                            .push(ty);
                    }
                }
            }
        }
    });
}

/// Infers the type of an expression if it is a literal or a reference to a constructor parameter.
fn infer_literal_or_known_param(expr: &Expr) -> Option<Type> {
    match &expr.kind {
        ExprKind::Literal(lit) => match lit {
            Literal::Number(_) => Some(Type::Number),
            Literal::String(_) => Some(Type::String),
            Literal::Boolean(_) => Some(Type::Boolean),
        },
        // Variable references will be handled in `propagate_to_parent` using the resolved types.
        _ => None,
    }
}

/// Resolves unannotated parameters of a single type using collected constraints.
///
/// Uses the same unique‑type resolution logic as function parameters.
fn resolve_type_params(
    type_name: &str,
    registry: &mut TypeRegistry,
    constraints: &mut HashMap<(String, usize), Vec<Type>>,
    errors: &mut Vec<SemanticError>,
) {
    let info = match registry.lookup_type_mut(type_name) {
        Some(i) => i,
        None => return,
    };

    for (idx, (name, ty)) in info.params.iter_mut().enumerate() {
        if !matches!(ty, Type::Unknown) {
            continue; // already annotated
        }
        let key = (type_name.to_string(), idx);
        if let Some(candidates) = constraints.get(&key) {
            let mut unique = HashSet::new();
            for c in candidates {
                if !matches!(c, Type::Unknown | Type::Error) {
                    unique.insert(c.clone());
                }
            }
            let unique_types: Vec<Type> = unique.into_iter().collect();
            if unique_types.is_empty() {
                errors.push(SemanticError::error(
                    SemanticErrorKind::CannotInferType {
                        symbol: name.clone(),
                    },
                    info.span,
                ));
                *ty = Type::Error;
            } else if unique_types.len() == 1 {
                *ty = unique_types[0].clone();
            } else {
                errors.push(SemanticError::error(
                    SemanticErrorKind::AmbiguousInference {
                        symbol: name.clone(),
                        candidates: unique_types,
                    },
                    info.span,
                ));
                *ty = Type::Error;
            }
        }
        // If no constraints, keep as Unknown (will be caught in Pass 3).
    }
}

/// Propagates this type's resolved parameters to its parent constructor.
///
/// For each argument in the `inherits Parent(args)` clause, if the argument
/// is a bare reference to one of this type's own parameters, adds that
/// parameter's resolved type as a constraint for the corresponding parent
/// parameter.
fn propagate_to_parent(
    type_name: &str,
    registry: &mut TypeRegistry,
    constraints: &mut HashMap<(String, usize), Vec<Type>>,
) {
    let (parent_name, args) = match registry.lookup_type(type_name) {
        Some(info) if info.parent.is_some() => {
            let parent = info.parent.as_ref().unwrap();
            (parent.name.clone(), parent.args.clone())
        }
        _ => return,
    };

    // Check that parent exists.
    if !registry.types.contains_key(&parent_name) {
        return;
    }

    // For each argument in the `inherits` clause, try to resolve its type.
    for (idx, arg) in args.iter().enumerate() {
        if let Some(resolved_ty) = resolve_argument_type(arg, type_name, registry) {
            constraints
                .entry((parent_name.clone(), idx))
                .or_default()
                .push(resolved_ty);
        }
    }
}

/// Resolves the type of an expression in the context of a constructor call.
///
/// If the expression is a variable referencing one of the type's own
/// parameters, returns that parameter's resolved type (which should have been
/// set by `resolve_type_params`). Otherwise, returns `None`.
fn resolve_argument_type(expr: &Expr, current_type: &str, registry: &TypeRegistry) -> Option<Type> {
    match &expr.kind {
        ExprKind::Variable(name) => {
            if let Some(info) = registry.lookup_type(current_type) {
                for (p_name, p_ty) in &info.params {
                    if p_name == name && !matches!(p_ty, Type::Unknown | Type::Error) {
                        return Some(p_ty.clone());
                    }
                }
            }
            None
        }
        // Literals are handled by the initial collector; they don't need propagation.
        _ => None,
    }
}

/// Walks all expressions in the program and calls `f` on each one.
fn traverse_exprs<F>(program: &Program, errors: &mut Vec<SemanticError>, mut f: F)
where
    F: FnMut(&Expr),
{
    for decl in &program.declarations {
        match &decl.kind {
            DeclarationKind::Function(func) => traverse_expr(&func.body, errors,  &mut f),
            DeclarationKind::Type(ty) => {
                for member in &ty.members {
                    match &member.kind {
                        TypeMemberKind::Attribute(attr) => traverse_expr(&attr.initializer, errors, &mut f),
                        TypeMemberKind::Method(method) => traverse_expr(&method.body, errors, &mut f),
                    }
                }
                if let Some(parent) = &ty.parent {
                    for arg in &parent.args {
                        traverse_expr(arg, errors, &mut f);
                    }
                }
            }
            DeclarationKind::Protocol(_) => {}
            DeclarationKind::Macro(m) => {
                // Macro declarations must be expanded before semantic analysis.
                errors.push(SemanticError::error(
                    SemanticErrorKind::MacroReferenceFound {
                        macro_expr: m.name.clone(),
                    },
                    decl.span,
                ));
                // Still traverse the body to find nested macro calls.
                traverse_expr(&m.body, errors, &mut f);
            }
        }
    }
    traverse_expr(&program.entry, errors, &mut f);
}

/// Recursive helper for `traverse_exprs`.
fn traverse_expr<F>(expr: &Expr, errors: &mut Vec<SemanticError>, f: &mut F)
where
    F: FnMut(&Expr),
{
    f(expr);
    match &expr.kind {
        ExprKind::Literal(_) | ExprKind::Variable(_) | ExprKind::SelfRef | ExprKind::BaseRef => {}
        ExprKind::Unary(unary) => traverse_expr(&unary.expr, errors, f),
        ExprKind::Binary(binary) => {
            traverse_expr(&binary.left, errors, f);
            traverse_expr(&binary.right, errors, f);
        }
        ExprKind::Let(let_expr) => {
            for binding in &let_expr.bindings {
                traverse_expr(&binding.initializer, errors, f);
            }
            traverse_expr(&let_expr.body, errors, f);
        }
        ExprKind::Assign(assign) => {
            traverse_assign_target(&assign.target, errors, f);
            traverse_expr(&assign.value, errors, f);
        }
        ExprKind::Block(block) => {
            for e in &block.expressions {
                traverse_expr(e, errors, f);
            }
        }
        ExprKind::If(if_expr) => {
            traverse_expr(&if_expr.condition, errors, f);
            traverse_expr(&if_expr.then_branch, errors, f);
            for elif in &if_expr.elif_branches {
                traverse_expr(&elif.condition, errors, f);
                traverse_expr(&elif.body, errors, f);
            }
            traverse_expr(&if_expr.else_branch, errors, f);
        }
        ExprKind::While(while_expr) => {
            traverse_expr(&while_expr.condition, errors, f);
            traverse_expr(&while_expr.body, errors, f);
        }
        ExprKind::For(for_expr) => {
            traverse_expr(&for_expr.iterable, errors, f);
            traverse_expr(&for_expr.body, errors, f);
        }
        ExprKind::Call(call) => {
            traverse_expr(&call.callee, errors, f);
            for arg in &call.args {
                traverse_expr(arg, errors, f);
            }
        }
        ExprKind::Lambda(lambda) => traverse_expr(&lambda.body, errors, f),
        ExprKind::Member(member) => traverse_expr(&member.object, errors, f),
        ExprKind::New(new_expr) => {
            for arg in &new_expr.args {
                traverse_expr(arg, errors, f);
            }
        }
        ExprKind::TypeTest(type_test) => traverse_expr(&type_test.expr, errors, f),
        ExprKind::Downcast(downcast) => traverse_expr(&downcast.expr, errors, f),
        ExprKind::Vector(vector) => match vector {
            hulk_ast::VectorExpr::Literal(items) => {
                for item in items {
                    traverse_expr(item, errors, f);
                }
            }
            hulk_ast::VectorExpr::Comprehension(comp) => {
                traverse_expr(&comp.expr, errors, f);
                traverse_expr(&comp.iterable, errors, f);
            }
        },
        ExprKind::Index(index) => {
            traverse_expr(&index.object, errors, f);
            traverse_expr(&index.index, errors, f);
        }
        ExprKind::Match(match_expr) => {
            traverse_expr(&match_expr.value, errors, f);
            for case in &match_expr.cases {
                traverse_expr(&case.body, errors, f);
            }
        }
        ExprKind::MacroCall(mc) => {
            // Macro calls must be expanded before semantic analysis.
            errors.push(SemanticError::error(
                SemanticErrorKind::MacroReferenceFound {
                    macro_expr: mc.name.clone(),
                },
                expr.span,
            ));
            // Traverse any expression arguments and the optional body.
            for arg in &mc.args {
                if let MacroArg::Expr(e) = arg {
                    traverse_expr(e, errors, f);
                }
                // Symbolic and placeholder arguments have no expressions to traverse.
            }
            if let Some(body) = &mc.body {
                traverse_expr(body, errors, f);
            }
        }
        ExprKind::MacroMatch(mm) => {
            errors.push(SemanticError::error(
                SemanticErrorKind::MacroReferenceFound {
                    macro_expr: "macro match".to_string(),
                },
                expr.span,
            ));
            traverse_expr(&mm.scrutinee, errors, f);
            for case in &mm.cases {
                traverse_expr(&case.body, errors, f);
            }
        }
    }
}

/// Helper to traverse assignment targets.
fn traverse_assign_target<F>(target: &hulk_ast::AssignTarget, errors: &mut Vec<SemanticError>, f: &mut F)
where
    F: FnMut(&Expr),
{
    match target {
        hulk_ast::AssignTarget::Variable(_) => {}
        hulk_ast::AssignTarget::Member { object, .. } => traverse_expr(object, errors, f),
        hulk_ast::AssignTarget::Index { object, index } => {
            traverse_expr(object, errors, f);
            traverse_expr(index, errors, f);
        }
    }
}
