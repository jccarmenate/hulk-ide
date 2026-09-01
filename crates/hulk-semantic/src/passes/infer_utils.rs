// Utilities for type inference pass.

use std::collections::HashMap;

use hulk_ast::{AssignTarget, BinaryOp, ExprKind, Literal, TypeRef, UnaryOp, VectorExpr};

use crate::typed::TypedExpr;
use crate::types::registry::TypeRegistry;
use crate::types::{lowest_common_ancestor, Type};

/// Walks a typed expression and replaces `Type::Unknown` annotations with
/// resolved types for parameters and self‑recursive calls.
pub fn patch_unknowns(
    expr: &mut TypedExpr,
    param_types: &HashMap<String, Type>,
    current_function: &str,
    return_type: &Type,
) {
    // Patch the current node if it's a variable or a recursive call.
    match &mut expr.kind {
        ExprKind::Variable(name) => {
            if let Some(ty) = param_types.get(name) {
                expr.anno = ty.clone();
            }
        }
        ExprKind::Call(call) => {
            if let ExprKind::Variable(callee_name) = &call.callee.kind {
                if callee_name == current_function {
                    expr.anno = return_type.clone();
                    match &call.callee.anno {
                        Type::Function { params, .. } => {
                            call.callee.anno = Type::Function {
                                params: params.clone(),
                                return_type: Box::new(return_type.clone()),
                            };
                        }
                        _ => call.callee.anno = return_type.clone(),
                    }
                }
            }
            // Cover methods that call themselves (e. g. via self.m())
            if let ExprKind::Member(member) = &call.callee.kind {
                if member.member == *current_function {
                    if let ExprKind::SelfRef = member.object.kind {
                        expr.anno = return_type.clone();
                        match &call.callee.anno {
                            Type::Function { params, .. } => {
                                call.callee.anno = Type::Function {
                                    params: params.clone(),
                                    return_type: Box::new(return_type.clone()),
                                };
                            }
                            _ => call.callee.anno = return_type.clone(),
                        }
                    }
                }
            }
            // Recurse into children.
            patch_unknowns(&mut call.callee, param_types, current_function, return_type);
            for arg in &mut call.args {
                patch_unknowns(arg, param_types, current_function, return_type);
            }
            // Return early because we already recursed into children.
            return;
        }
        _ => {}
    }

    // For all other node types, recurse into their children.
    match &mut expr.kind {
        ExprKind::Unary(unary) => {
            patch_unknowns(&mut unary.expr, param_types, current_function, return_type);
        }
        ExprKind::Binary(binary) => {
            patch_unknowns(&mut binary.left, param_types, current_function, return_type);
            patch_unknowns(
                &mut binary.right,
                param_types,
                current_function,
                return_type,
            );
        }
        ExprKind::Let(let_expr) => {
            for binding in &mut let_expr.bindings {
                patch_unknowns(
                    &mut binding.initializer,
                    param_types,
                    current_function,
                    return_type,
                );
            }
            patch_unknowns(
                &mut let_expr.body,
                param_types,
                current_function,
                return_type,
            );
        }
        ExprKind::Assign(assign) => {
            patch_unknowns(
                &mut assign.value,
                param_types,
                current_function,
                return_type,
            );
            // Target contains expressions; patch them.
            match &mut assign.target {
                AssignTarget::Variable(_) => {}
                AssignTarget::Member { object, .. } => {
                    patch_unknowns(object, param_types, current_function, return_type);
                }
                AssignTarget::Index { object, index } => {
                    patch_unknowns(object, param_types, current_function, return_type);
                    patch_unknowns(index, param_types, current_function, return_type);
                }
            }
        }
        ExprKind::Block(block) => {
            for e in &mut block.expressions {
                patch_unknowns(e, param_types, current_function, return_type);
            }
        }
        ExprKind::If(if_expr) => {
            patch_unknowns(
                &mut if_expr.condition,
                param_types,
                current_function,
                return_type,
            );
            patch_unknowns(
                &mut if_expr.then_branch,
                param_types,
                current_function,
                return_type,
            );
            for elif in &mut if_expr.elif_branches {
                patch_unknowns(
                    &mut elif.condition,
                    param_types,
                    current_function,
                    return_type,
                );
                patch_unknowns(&mut elif.body, param_types, current_function, return_type);
            }
            patch_unknowns(
                &mut if_expr.else_branch,
                param_types,
                current_function,
                return_type,
            );
        }
        ExprKind::While(while_expr) => {
            patch_unknowns(
                &mut while_expr.condition,
                param_types,
                current_function,
                return_type,
            );
            patch_unknowns(
                &mut while_expr.body,
                param_types,
                current_function,
                return_type,
            );
        }
        ExprKind::For(for_expr) => {
            patch_unknowns(
                &mut for_expr.iterable,
                param_types,
                current_function,
                return_type,
            );
            patch_unknowns(
                &mut for_expr.body,
                param_types,
                current_function,
                return_type,
            );
        }
        ExprKind::Call(call) => {
            // We already handled Call in the earlier match; this is a catch‑all for safety.
            patch_unknowns(&mut call.callee, param_types, current_function, return_type);
            for arg in &mut call.args {
                patch_unknowns(arg, param_types, current_function, return_type);
            }
        }
        ExprKind::Lambda(lambda) => {
            patch_unknowns(&mut lambda.body, param_types, current_function, return_type);
        }
        ExprKind::Member(member) => {
            patch_unknowns(
                &mut member.object,
                param_types,
                current_function,
                return_type,
            );
        }
        ExprKind::New(new_expr) => {
            for arg in &mut new_expr.args {
                patch_unknowns(arg, param_types, current_function, return_type);
            }
        }
        ExprKind::TypeTest(type_test) => {
            patch_unknowns(
                &mut type_test.expr,
                param_types,
                current_function,
                return_type,
            );
        }
        ExprKind::Downcast(downcast) => {
            patch_unknowns(
                &mut downcast.expr,
                param_types,
                current_function,
                return_type,
            );
        }
        ExprKind::Vector(vector) => match vector {
            VectorExpr::Literal(items) => {
                for item in items {
                    patch_unknowns(item, param_types, current_function, return_type);
                }
            }
            VectorExpr::Comprehension(comp) => {
                patch_unknowns(&mut comp.expr, param_types, current_function, return_type);
                patch_unknowns(
                    &mut comp.iterable,
                    param_types,
                    current_function,
                    return_type,
                );
            }
        },
        ExprKind::Index(index) => {
            patch_unknowns(
                &mut index.object,
                param_types,
                current_function,
                return_type,
            );
            patch_unknowns(&mut index.index, param_types, current_function, return_type);
        }
        ExprKind::Match(match_expr) => {
            patch_unknowns(
                &mut match_expr.value,
                param_types,
                current_function,
                return_type,
            );
            for case in &mut match_expr.cases {
                patch_unknowns(&mut case.body, param_types, current_function, return_type);
            }
        }
        // Leaves: Literal, Variable, SelfRef, BaseRef already handled above.
        _ => {}
    }
}

/// Recomputes the type annotation of every node in the expression tree after
/// child annotations have been updated (e.g., by `patch_unknowns`).
pub fn recompute_annotations(expr: &mut TypedExpr, registry: &TypeRegistry) {
    // Post‑order: recurse into children first.
    match &mut expr.kind {
        ExprKind::Unary(unary) => recompute_annotations(&mut unary.expr, registry),
        ExprKind::Binary(binary) => {
            recompute_annotations(&mut binary.left, registry);
            recompute_annotations(&mut binary.right, registry);
        }
        ExprKind::Let(let_expr) => {
            for binding in &mut let_expr.bindings {
                recompute_annotations(&mut binding.initializer, registry);
            }
            recompute_annotations(&mut let_expr.body, registry);
        }
        ExprKind::Assign(assign) => {
            // Recurse into target sub‑expressions.
            match &mut assign.target {
                AssignTarget::Variable(_) => {}
                AssignTarget::Member { object, .. } => recompute_annotations(object, registry),
                AssignTarget::Index { object, index } => {
                    recompute_annotations(object, registry);
                    recompute_annotations(index, registry);
                }
            }
            recompute_annotations(&mut assign.value, registry);
        }
        ExprKind::Block(block) => {
            for e in &mut block.expressions {
                recompute_annotations(e, registry);
            }
        }
        ExprKind::If(if_expr) => {
            recompute_annotations(&mut if_expr.condition, registry);
            recompute_annotations(&mut if_expr.then_branch, registry);
            for elif in &mut if_expr.elif_branches {
                recompute_annotations(&mut elif.condition, registry);
                recompute_annotations(&mut elif.body, registry);
            }
            recompute_annotations(&mut if_expr.else_branch, registry);
        }
        ExprKind::While(while_expr) => {
            recompute_annotations(&mut while_expr.condition, registry);
            recompute_annotations(&mut while_expr.body, registry);
        }
        ExprKind::For(for_expr) => {
            recompute_annotations(&mut for_expr.iterable, registry);
            recompute_annotations(&mut for_expr.body, registry);
        }
        ExprKind::Call(call) => {
            recompute_annotations(&mut call.callee, registry);
            for arg in &mut call.args {
                recompute_annotations(arg, registry);
            }
        }
        ExprKind::Lambda(lambda) => recompute_annotations(&mut lambda.body, registry),
        ExprKind::Member(member) => recompute_annotations(&mut member.object, registry),
        ExprKind::New(new_expr) => {
            for arg in &mut new_expr.args {
                recompute_annotations(arg, registry);
            }
        }
        ExprKind::TypeTest(type_test) => recompute_annotations(&mut type_test.expr, registry),
        ExprKind::Downcast(downcast) => recompute_annotations(&mut downcast.expr, registry),
        ExprKind::Vector(vector) => match vector {
            VectorExpr::Literal(items) => {
                for item in items {
                    recompute_annotations(item, registry);
                }
            }
            VectorExpr::Comprehension(comp) => {
                recompute_annotations(&mut comp.expr, registry);
                recompute_annotations(&mut comp.iterable, registry);
            }
        },
        ExprKind::Index(index) => {
            recompute_annotations(&mut index.object, registry);
            recompute_annotations(&mut index.index, registry);
        }
        ExprKind::Match(match_expr) => {
            recompute_annotations(&mut match_expr.value, registry);
            for case in &mut match_expr.cases {
                recompute_annotations(&mut case.body, registry);
            }
        }
        // Leaves: Literal, Variable, SelfRef, BaseRef – no children.
        _ => {}
    }

    // Then recompute this node's annotation.
    expr.anno = compute_node_type(expr, registry);
}

/// Computes the type of a node given that its children already have final annotations.
/// This mirrors the type‑computation logic from `infer_expr` but without environment
/// or constraint collection.
fn compute_node_type(expr: &TypedExpr, registry: &TypeRegistry) -> Type {
    match &expr.kind {
        // ─── Leaves ──────────────────────────────────────────────────────────
        ExprKind::Literal(lit) => match lit {
            Literal::Number(_) => Type::Number,
            Literal::String(_) => Type::String,
            Literal::Boolean(_) => Type::Boolean,
        },
        ExprKind::Variable(_) | ExprKind::SelfRef | ExprKind::BaseRef => expr.anno.clone(),

        // ─── Unary ──────────────────────────────────────────────────────────
        ExprKind::Unary(unary) => {
            let operand_ty = &unary.expr.anno;
            match unary.op {
                UnaryOp::Negate
                    if matches!(operand_ty, Type::Number | Type::Unknown | Type::Error) =>
                {
                    Type::Number
                }
                UnaryOp::Negate => Type::Error,
                UnaryOp::Not
                    if matches!(operand_ty, Type::Boolean | Type::Unknown | Type::Error) =>
                {
                    Type::Boolean
                }
                UnaryOp::Not => Type::Error,
            }
        }

        // ─── Binary ──────────────────────────────────────────────────────────
        ExprKind::Binary(binary) => {
            let left_ty = &binary.left.anno;
            let right_ty = &binary.right.anno;
            match binary.op {
                BinaryOp::Add
                | BinaryOp::Subtract
                | BinaryOp::Multiply
                | BinaryOp::Divide
                | BinaryOp::Modulo
                | BinaryOp::Power => {
                    if matches!(left_ty, Type::Number | Type::Unknown | Type::Error)
                        && matches!(right_ty, Type::Number | Type::Unknown | Type::Error)
                    {
                        Type::Number
                    } else {
                        Type::Error
                    }
                }
                BinaryOp::Equal | BinaryOp::NotEqual => {
                    let valid = |t: &Type| {
                        matches!(
                            t,
                            Type::Number
                                | Type::String
                                | Type::Boolean
                                | Type::Unknown
                                | Type::Error
                        )
                    };
                    if valid(left_ty) && valid(right_ty) {
                        Type::Boolean
                    } else {
                        Type::Error
                    }
                }
                BinaryOp::Less
                | BinaryOp::LessEqual
                | BinaryOp::Greater
                | BinaryOp::GreaterEqual => {
                    if matches!(left_ty, Type::Number | Type::Unknown | Type::Error)
                        && matches!(right_ty, Type::Number | Type::Unknown | Type::Error)
                    {
                        Type::Boolean
                    } else {
                        Type::Error
                    }
                }
                BinaryOp::And | BinaryOp::Or => {
                    if matches!(left_ty, Type::Boolean | Type::Unknown | Type::Error)
                        && matches!(right_ty, Type::Boolean | Type::Unknown | Type::Error)
                    {
                        Type::Boolean
                    } else {
                        Type::Error
                    }
                }
                BinaryOp::Concat | BinaryOp::ConcatSpace => {
                    let allowed = |t: &Type| {
                        matches!(
                            t,
                            Type::Number
                                | Type::String
                                | Type::Boolean
                                | Type::Unknown
                                | Type::Error
                        )
                    };
                    if allowed(left_ty) && allowed(right_ty) {
                        Type::String
                    } else {
                        Type::Error
                    }
                }
            }
        }

        // ─── Control flow ────────────────────────────────────────────────────
        ExprKind::Let(let_expr) => let_expr.body.anno.clone(),
        ExprKind::Block(block) => block
            .expressions
            .last()
            .map(|e| e.anno.clone())
            .unwrap_or(Type::Object),
        ExprKind::If(if_expr) => {
            let mut branch_types = vec![if_expr.then_branch.anno.clone()];
            for elif in &if_expr.elif_branches {
                branch_types.push(elif.body.anno.clone());
            }
            branch_types.push(if_expr.else_branch.anno.clone());
            lowest_common_ancestor(&branch_types, registry)
        }
        ExprKind::While(while_expr) => while_expr.body.anno.clone(),
        ExprKind::For(for_expr) => for_expr.body.anno.clone(),

        // ─── Call ─────────────────────────────────────────────────────────────
        ExprKind::Call(call) => {
            let callee_ty = &call.callee.anno;
            if let Type::Function { return_type, .. } = callee_ty {
                *return_type.clone()
            } else {
                // For global functions, the callee's annotation is already the return type.
                callee_ty.clone()
            }
        }

        // ─── Lambda ─────────────────────────────────────────────────────────
        ExprKind::Lambda(lambda) => match &expr.anno {
            Type::Function { params, .. } => Type::Function {
                params: params.clone(),
                return_type: Box::new(lambda.body.anno.clone()),
            },
            other => other.clone(),
        },

        // ─── Member ──────────────────────────────────────────────────────────
        ExprKind::Member(member) => {
            let obj_ty = &member.object.anno;
            lookup_member_type(obj_ty, &member.member, registry).unwrap_or(Type::Error)
        }

        // ─── New ─────────────────────────────────────────────────────────────
        ExprKind::New(new_expr) => Type::Named(new_expr.type_name.name.clone()),

        // ─── TypeTest ────────────────────────────────────────────────────────
        ExprKind::TypeTest(_) => Type::Boolean,

        // ─── Downcast ────────────────────────────────────────────────────────
        ExprKind::Downcast(downcast) => resolve_type_ref_static(&downcast.type_name, registry),

        // ─── Vector ──────────────────────────────────────────────────────────
        ExprKind::Vector(vector) => match vector {
            VectorExpr::Literal(items) => {
                let elem_types: Vec<Type> = items.iter().map(|e| e.anno.clone()).collect();
                let elem = if elem_types.is_empty() {
                    Type::Unknown
                } else {
                    lowest_common_ancestor(&elem_types, registry)
                };
                Type::Vector(Box::new(elem))
            }
            VectorExpr::Comprehension(comp) => Type::Vector(Box::new(comp.expr.anno.clone())),
        },

        // ─── Index ───────────────────────────────────────────────────────────
        ExprKind::Index(index) => match &index.object.anno {
            Type::Vector(inner) => *inner.clone(),
            _ => Type::Error,
        },

        // ─── Match ───────────────────────────────────────────────────────────
        ExprKind::Match(match_expr) => {
            let case_types: Vec<Type> = match_expr
                .cases
                .iter()
                .map(|c| c.body.anno.clone())
                .collect();
            if case_types.is_empty() {
                Type::Error
            } else {
                lowest_common_ancestor(&case_types, registry)
            }
        }

        // ─── Assignment ─────────────────────────────────────────────────────
        ExprKind::Assign(assign) => assign.value.anno.clone(),

        // ─── Macro ──────────────────────────────────────────────────────────
        // Macro calls and matches should be eliminated by the expander before semantic
        // analysis. If one reaches this point, it's a pipeline error; we return Error.
        ExprKind::MacroCall(_) | ExprKind::MacroMatch(_) => Type::Error,
    }
}

/// Returns the type of a member (attribute or method) on the given type.
/// For attributes, returns the attribute's declared type; for methods, returns a `Type::Function`.
fn lookup_member_type(ty: &Type, member_name: &str, registry: &TypeRegistry) -> Option<Type> {
    match ty {
        Type::Named(name) => {
            if let Some(info) = registry.lookup_type(name) {
                if let Some(attr) = info.attributes.get(member_name) {
                    return attr.declared_type.clone();
                }
            }
            // Method -> return Function type
            if let Some(method_sig) = registry.lookup_method(ty, member_name) {
                let param_types: Vec<Type> =
                    method_sig.params.iter().map(|(_, t)| t.clone()).collect();
                return Some(Type::Function {
                    params: param_types,
                    return_type: Box::new(method_sig.return_type),
                });
            }
            None
        }
        Type::Vector(_) | Type::Iterable(_) => {
            if let Some(method_sig) = registry.lookup_method(ty, member_name) {
                let param_types: Vec<Type> =
                    method_sig.params.iter().map(|(_, t)| t.clone()).collect();
                return Some(Type::Function {
                    params: param_types,
                    return_type: Box::new(method_sig.return_type),
                });
            }
            None
        }
        _ => None,
    }
}

/// Resolves a syntactic `TypeRef` to a semantic `Type` without an inference state.
#[allow(clippy::only_used_in_recursion)]
fn resolve_type_ref_static(tr: &TypeRef, registry: &TypeRegistry) -> Type {
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
                    .map(|arg| resolve_type_ref_static(arg, registry))
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
