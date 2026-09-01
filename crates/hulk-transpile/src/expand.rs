//! Macro expansion: walks the program AST, replacing every `MacroCallExpr`
//! with the expanded body of the corresponding macro declaration.
//!
//! Expansion is iterative: after one full pass, if any `MacroCallExpr` nodes
//! remain (from macros-inside-macros), the pass repeats. A recursion guard
//! prevents infinite loops.

use std::collections::HashSet;
use hulk_ast::{
    DeclarationKind, Expr, ExprKind, MacroArg, MacroCallExpr,
    MacroParam, MacroParamKind, Program, TypeMemberKind, MacroMatchExpr,
};

use crate::collect::MacroRegistry;
use crate::error::{MacroError, MacroErrorKind};
use crate::substitute::{collect_let_bindings, substitute, SubstMap};
use crate::pattern::try_match;

/// Hard-coded expansion passes limit
const MAX_EXPANSION_PASSES: usize = 64;

/// Entry point: expand all macro calls in `program`.
///
/// Mutates `program` in place. After this call, no `MacroCallExpr` or
/// `MacroDecl` nodes should remain in the AST. (MacroDecls are removed
/// by `collect::collect` before this runs.)
pub fn expand(
    program: &mut Program,
    registry: &MacroRegistry,
    errors: &mut Vec<MacroError>,
) {
    // Expand declarations (function/method bodies may contain macro calls).
    let mut decls = std::mem::take(&mut program.declarations);
    for decl in &mut decls {
        match &mut decl.kind {
            DeclarationKind::Function(f) => {
                f.body = expand_expr(f.body.clone(), registry, errors, 0);
            }
            DeclarationKind::Type(t) => {
                for member in &mut t.members {
                    match &mut member.kind {
                        TypeMemberKind::Attribute(a) => {
                            a.initializer = expand_expr(a.initializer.clone(), registry, errors, 0);
                        }
                        TypeMemberKind::Method(m) => {
                            m.body = expand_expr(m.body.clone(), registry, errors, 0);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    program.declarations = decls;
    program.entry = expand_expr(program.entry.clone(), registry, errors, 0);
}

/// Recursively expands one expression, up to `depth` nested macro invocations.
fn expand_expr(
    expr: Expr,
    registry: &MacroRegistry,
    errors: &mut Vec<MacroError>,
    depth: usize,
) -> Expr {
    if depth > MAX_EXPANSION_PASSES {
        // Already reported as recursive; return unchanged to stop recursion.
        return expr;
    }

    match expr.kind {
        ExprKind::MacroCall(ref mc) => {
            let name = mc.name.clone();
            let span = expr.span;
            match registry.get(&name) {
                None => {
                    // May be a regular function call that was mis-classified by the parser 
                    // (e.g., function name followed by a block that isn't a macro). 
                    //
                    // Convert to a regular Call.
                    if mc.body.is_some() {
                        errors.push(MacroError::new(
                            MacroErrorKind::UndefinedMacro(name.clone()),
                            span,
                        ));
                        return expr;
                    }
                    // No trailing body — convert to a normal Call expression.
                    let args: Vec<Expr> = mc.args.iter().map(|a| match a {
                        MacroArg::Expr(e) => expand_expr(e.clone(), registry, errors, depth),
                        _ => {
                            errors.push(MacroError::new(
                                MacroErrorKind::MacroArgInNonMacroCall { name: name.clone() },
                                span,
                            ));
                            Expr::new(ExprKind::Variable("__error__".to_string()), span)
                        }
                    }).collect();
                    let callee = Expr::variable(name, span);
                    return Expr::call(callee, args, span);
                }
                Some((_macro_decl, _def_span)) => {
                    // Expand the macro call.
                    match expand_macro_call(&expr, registry, errors, depth) {
                        Some(expanded) => {
                            // Recursively expand in case the expansion itself
                            // contains macro calls.
                            return expand_expr(expanded, registry, errors, depth + 1);
                        }
                        None => return expr, // error already pushed
                    }
                }
            }
        }
        // Treat plain calls to macro names as macro calls
        ExprKind::Call(ref call) => {
            if let ExprKind::Variable(name) = &call.callee.kind {
                if registry.contains_key(name) {
                    // Convert arguments to MacroArg::Expr
                    let macro_args: Vec<MacroArg> = call
                        .args
                        .iter()
                        .map(|arg| MacroArg::Expr(arg.clone()))
                        .collect();
                    let mc = MacroCallExpr::new(name.clone(), macro_args, None);
                    let macro_expr = Expr::new(ExprKind::MacroCall(mc), expr.span);
                    // Expand the newly constructed macro call (depth+1)
                    return expand_expr(macro_expr, registry, errors, depth + 1);
                }
            }
            // Not a macro call -> recursively expand children and keep as Call
            rebuild_expr_children(expr, registry, errors, depth)
        }
       ExprKind::MacroMatch(mm) => {
            // Clone the scrutinee to avoid moving out of mm.
            let scrutinee = expand_expr((*mm.scrutinee).clone(), registry, errors, depth);

            // Try each case in order.
            for case in &mm.cases {
                match try_match(&case.pattern, &scrutinee) {
                    Ok(Some(bindings)) => {
                        let mut subst = SubstMap::new();
                        for (name, expr) in bindings {
                            subst.insert_expr(name, expr);
                        }
                        let body = substitute(&case.body, &subst);
                        return expand_expr(body, registry, errors, depth + 1);
                    }
                    Ok(None) => continue,
                    Err(err_kind) => {
                        errors.push(MacroError::new(err_kind, expr.span));
                        // Return the expanded expression with the error.
                        let new_mm = MacroMatchExpr {
                            scrutinee: Box::new(scrutinee),
                            cases: mm.cases.clone(),
                        };
                        return Expr::new(ExprKind::MacroMatch(new_mm), expr.span);
                    }
                }
            }

            // No case matched – report error and return the expanded expression.
            errors.push(MacroError::new(
                MacroErrorKind::NonExhaustiveMacroMatch,
                expr.span,
            ));
            let new_mm = MacroMatchExpr {
                scrutinee: Box::new(scrutinee),
                cases: mm.cases.clone(),
            };
            Expr::new(ExprKind::MacroMatch(new_mm), expr.span)
        }
        // For all other expression kinds, recursively expand children.
        _ => rebuild_expr_children(expr, registry, errors, depth),
    }
}

/// Builds the substitution map for a single macro call and applies it.
fn expand_macro_call(
    expr: &Expr,
    registry: &MacroRegistry,
    errors: &mut Vec<MacroError>,
    depth: usize,
) -> Option<Expr> {
    let mc = match &expr.kind {
        ExprKind::MacroCall(mc) => mc,
        _ => unreachable!(),
    };
    let span = expr.span;

    let (macro_decl, _) = registry.get(&mc.name)?;

    // ── 1. Validate arity ────────────────────────────────────────────────────
    //
    // Count non-BodyExpr params (the BodyExpr is the trailing `{ }` block).
    let explicit_params: Vec<&MacroParam> = macro_decl
        .params.iter().filter(|p| p.kind != MacroParamKind::BodyExpr).collect();
    let has_body_param = macro_decl
        .params.iter().any(|p| p.kind == MacroParamKind::BodyExpr);

    if mc.args.len() != explicit_params.len() {
        errors.push(MacroError::new(
            MacroErrorKind::ArityMismatch {
                name: mc.name.clone(),
                expected: explicit_params.len(),
                got: mc.args.len(),
            },
            span,
        ));
        return None;
    }

    if has_body_param && mc.body.is_none() {
        errors.push(MacroError::new(
            MacroErrorKind::MissingBodyBlock(mc.name.clone()),
            span,
        ));
        return None;
    }

    // ── 2. Build the substitution map ─────────────────────────────────────────
    let mut subst = SubstMap::new();

    // Names that must not be sanitized (placeholders and symbolic params).
    let mut protected: HashSet<String> = HashSet::new();

    for (param, arg) in explicit_params.iter().zip(&mc.args) {
        match param.kind {
            MacroParamKind::Regular => {
                let arg_expr = match arg {
                    MacroArg::Expr(e) => expand_expr(e.clone(), registry, errors, depth),
                    _ => {
                        errors.push(MacroError::new(
                            MacroErrorKind::SymbolicArgNotVariable {
                                macro_name: mc.name.clone(),
                                param: param.name.clone(),
                            },
                            span,
                        ));
                        return None;
                    }
                };
                subst.insert_expr(param.name.clone(), arg_expr);
            }
            MacroParamKind::Symbolic => {
                // `@x` → the caller must pass `@varname`.
                let var_name = match arg {
                    MacroArg::Symbolic(n) => n.clone(),
                    MacroArg::Expr(e) => match &e.kind {
                        ExprKind::Variable(n) => n.clone(),
                        _ => {
                            errors.push(MacroError::new(
                                MacroErrorKind::SymbolicArgNotVariable {
                                    macro_name: mc.name.clone(),
                                    param: param.name.clone(),
                                },
                                e.span,
                            ));
                            return None;
                        }
                    },
                    _ => {
                        errors.push(MacroError::new(
                            MacroErrorKind::SymbolicArgNotVariable {
                                macro_name: mc.name.clone(),
                                param: param.name.clone(),
                            },
                            span,
                        ));
                        return None;
                    }
                };
                // Replace uses of the symbolic param with the caller's variable.
                subst.insert_var(param.name.clone(), var_name.clone());
                protected.insert(var_name);
            }
            MacroParamKind::Placeholder => {
                // `$ph` → the caller supplies the name (as an Expr::Variable or
                // MacroArg::Placeholder).
                let user_name = match arg {
                    MacroArg::Placeholder(n) => n.clone(),
                    MacroArg::Expr(e) => match &e.kind {
                        ExprKind::Variable(n) => n.clone(),
                        _ => {
                            errors.push(MacroError::new(
                                MacroErrorKind::SymbolicArgNotVariable {
                                    macro_name: mc.name.clone(),
                                    param: param.name.clone(),
                                },
                                e.span,
                            ));
                            return None;
                        }
                    },
                    _ => param.name.clone(), // fallback
                };
                // All uses of the placeholder param name in the body will be
                // replaced with user_name.
                subst.insert_var(param.name.clone(), user_name.clone());
                protected.insert(user_name);
            }
            MacroParamKind::BodyExpr => unreachable!("filtered out above"),
        }
    }

    // Handle the `*expr` body parameter.
    if has_body_param {
        let body_param = macro_decl.params.iter()
            .find(|p| p.kind == MacroParamKind::BodyExpr).unwrap();
        let body_block = mc.body.as_deref().unwrap().clone();
        subst.insert_expr(body_param.name.clone(), body_block);
    }

    // ── 3. Variable sanitization ───────────────────────────────────
    //
    // Walk the template body, find all `let` binding names that are not in
    // `protected`, and give them unique generated names. This prevents the
    // macro's internal variables from shadowing or being shadowed by variables
    // at the call site.
    collect_let_bindings(&macro_decl.body, &protected, &mut subst);

    // ── 4. Apply substitution ─────────────────────────────────────────────────
    let expanded = substitute(&macro_decl.body, &subst);
    Some(expanded)
}

/// Rebuilds an expression by recursively expanding macro calls in all children.
/// This is a large but mechanical match — all expression forms are covered.
fn rebuild_expr_children(
    expr: Expr,
    registry: &MacroRegistry,
    errors: &mut Vec<MacroError>,
    depth: usize,
) -> Expr {
    let span = expr.span;
    macro_rules! ex {
        ($e:expr) => { expand_expr($e, registry, errors, depth) }
    }
    match expr.kind {
        ExprKind::Block(b) => Expr::new(
            ExprKind::Block(hulk_ast::BlockExpr::new(
                b.expressions.into_iter().map(|e| ex!(e)).collect()
            )),
            span,
        ),
        ExprKind::Let(l) => Expr::new(
            ExprKind::Let(hulk_ast::LetExpr::new(
                l.bindings.into_iter().map(|b| hulk_ast::LetBinding::new(
                    b.name,
                    b.type_annotation,
                    ex!(b.initializer),
                )).collect(),
                ex!(*l.body),
            )),
            span,
        ),
        ExprKind::If(i) => Expr::new(
            ExprKind::If(hulk_ast::IfExpr::new(
                ex!(*i.condition),
                ex!(*i.then_branch),
                i.elif_branches.into_iter().map(|e| hulk_ast::ElifBranch::new(
                    ex!(e.condition), ex!(e.body)
                )).collect(),
                ex!(*i.else_branch),
            )),
            span,
        ),
        ExprKind::While(w) => Expr::new(
            ExprKind::While(hulk_ast::WhileExpr::new(ex!(*w.condition), ex!(*w.body))),
            span,
        ),
        ExprKind::Call(c) => Expr::new(
            ExprKind::Call(hulk_ast::CallExpr {
                callee: Box::new(ex!(*c.callee)),
                args: c.args.into_iter().map(|a| ex!(a)).collect(),
            }),
            span,
        ),
        ExprKind::Binary(b) => Expr::new(
            ExprKind::Binary(hulk_ast::BinaryExpr {
                op: b.op,
                left: Box::new(ex!(*b.left)),
                right: Box::new(ex!(*b.right)),
            }),
            span,
        ),
        ExprKind::Assign(a) => {
            let new_target = match a.target {
                hulk_ast::AssignTarget::Member { object, field } =>
                    hulk_ast::AssignTarget::Member { object: Box::new(ex!(*object)), field },
                hulk_ast::AssignTarget::Index { object, index } =>
                    hulk_ast::AssignTarget::Index {
                        object: Box::new(ex!(*object)),
                        index: Box::new(ex!(*index)),
                    },
                other => other,
            };
            Expr::new(
                ExprKind::Assign(hulk_ast::AssignExpr::new(new_target, ex!(*a.value))),
                span,
            )
        }
        // Leaf and identity nodes
        other => Expr { kind: other, anno: (), span },
    }
}