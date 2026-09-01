//! Core substitution and variable sanitization for macro expansion.
//! 
//! Hndles:
//! 1. Cloning the macro body template
//! 2. Renaming internal (non-placeholder, non-symbolic) `let` bindings to unique
//!    sanitized names and updating all their references inside the cloned body
//! 3. Substituting each parameter name occurrence with the corresponding argument

use std::collections::HashMap;
use hulk_ast::{
    AssignExpr, AssignTarget, BlockExpr, DowncastExpr, ElifBranch, Expr, ExprKind, ForExpr,
    IndexExpr, LetBinding, LetExpr, MacroArg, MatchCase, MemberExpr, NewExpr, TypeTestExpr, 
    UnaryExpr, VectorComprehension, VectorExpr, WhileExpr, MacroCase, MacroMatchExpr,
};

/// Substitution map for a single macro expansion.
///
/// For each macro parameter, this function records what to substitute when 
/// encounters `Variable(param_name)` in the cloned body:
///
/// - Regular  -> replace with the arg expression (deep clone).
/// - BodyExpr -> replace with the trailing block expression.
/// - Symbolic -> replace with `Variable(bound_name)` — the callee's variable name.
/// - Placeholder -> replace with `Variable(user_name)` — the user-chosen name.
///
/// Variable sanitization is handled separately: for every `let` binding
/// inside the macro body (excluding placeholder-named bindings)
/// a fresh unique name and record an additional `Variable->Variable` mapping.
pub struct SubstMap {
    /// Maps macro param name -> replacement Expr.
    pub expr_subst: HashMap<String, Expr>,
}

impl SubstMap {
    pub fn new() -> Self {
        Self { expr_subst: HashMap::new() }
    }

    pub fn insert_expr(&mut self, name: String, expr: Expr) {
        self.expr_subst.insert(name, expr);
    }

    pub fn insert_var(&mut self, from: String, to: String) {
        // A Variable->Variable substitution.
        use hulk_ast::SourceSpan;
        self.expr_subst.insert(
            from,
            Expr::variable(to, SourceSpan::default()),
        );
    }
}

/// Counter for generating unique sanitized names.
static SANITIZE_COUNTER: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn fresh_name(original: &str) -> String {
    let n = SANITIZE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("__hulk_m{}_{}", n, original)
}

/// Walks `expr`, collecting all `LetBinding` names that are not already
/// in `protected` (i.e., not placeholder-named bindings), and adds
/// `Variable(old) -> Variable(fresh)` entries to `subst`.
///
/// This implements variable sanitization.
pub fn collect_let_bindings(
    expr: &Expr,
    protected: &std::collections::HashSet<String>,
    subst: &mut SubstMap,
) {
    match &expr.kind {
        ExprKind::Let(let_expr) => {
            for binding in &let_expr.bindings {
                if !protected.contains(&binding.name) && !subst.expr_subst.contains_key(&binding.name) {
                    let fresh = fresh_name(&binding.name);
                    subst.insert_var(binding.name.clone(), fresh);
                }
            }
            for binding in &let_expr.bindings {
                collect_let_bindings(&binding.initializer, protected, subst);
            }
            collect_let_bindings(&let_expr.body, protected, subst);
        }
        ExprKind::Block(b) => {
            for e in &b.expressions { collect_let_bindings(e, protected, subst); }
        }
        ExprKind::If(i) => {
            collect_let_bindings(&i.condition, protected, subst);
            collect_let_bindings(&i.then_branch, protected, subst);
            for elif in &i.elif_branches {
                collect_let_bindings(&elif.condition, protected, subst);
                collect_let_bindings(&elif.body, protected, subst);
            }
            collect_let_bindings(&i.else_branch, protected, subst);
        }
        ExprKind::While(w) => {
            collect_let_bindings(&w.condition, protected, subst);
            collect_let_bindings(&w.body, protected, subst);
        }
        ExprKind::For(f) => {
            collect_let_bindings(&f.iterable, protected, subst);
            collect_let_bindings(&f.body, protected, subst);
        }
        _ => { /* Non-binding node */ }
    }
}

/// Deep-clones `expr`, applying all substitutions in `subst`.
///
/// - When `Variable(name)` is found and `name` is in `subst.expr_subst`,
///   the entire expression is replaced with the mapped replacement (cloned).
/// - `LetBinding` names are rewritten to their sanitized equivalents.
/// - All other nodes are recursively cloned.
///
/// Note: substitutions are applied inside-out (body -> bindings), which means
/// the replacement expressions are themselves substituted. This handles macro
/// calls to other macros inside macro bodies correctly — they will be picked
/// up by a subsequent expansion pass.
pub fn substitute(expr: &Expr, subst: &SubstMap) -> Expr {
    match &expr.kind {
        ExprKind::Variable(name) => {
            if let Some(replacement) = subst.expr_subst.get(name) {
                // Preserve the original span so error messages point to the
                // macro call site rather than the macro definition.
                let mut replaced = replacement.clone();
                replaced.span = expr.span;
                replaced
            } else {
                expr.clone()
            }
        }
        ExprKind::Let(let_expr) => {
            // Rewrite binding names if they have a sanitized substitute.
            let new_bindings: Vec<LetBinding> = let_expr.bindings.iter().map(|b| {
                let new_name = if let Some(ExprKind::Variable(mapped)) =
                    subst.expr_subst.get(&b.name).map(|e| &e.kind)
                {
                    mapped.clone()
                } else {
                    b.name.clone()
                };
                LetBinding::new(
                    new_name,
                    b.type_annotation.clone(),
                    substitute(&b.initializer, subst),
                )
            }).collect();
            let new_body = substitute(&let_expr.body, subst);
            Expr::new(
                ExprKind::Let(LetExpr::new(new_bindings, new_body)),
                expr.span,
            )
        }
        // All compound forms: recursively substitute children.
        ExprKind::Block(b) => Expr::new(
            ExprKind::Block(BlockExpr::new(
                b.expressions.iter().map(|e| substitute(e, subst)).collect()
            )),
            expr.span,
        ),
        ExprKind::If(i) => Expr::new(
            ExprKind::If(hulk_ast::IfExpr::new(
                substitute(&i.condition, subst),
                substitute(&i.then_branch, subst),
                i.elif_branches.iter().map(|elif| ElifBranch::new(
                    substitute(&elif.condition, subst),
                    substitute(&elif.body, subst),
                )).collect(),
                substitute(&i.else_branch, subst),
            )),
            expr.span,
        ),
        ExprKind::While(w) => Expr::new(
            ExprKind::While(WhileExpr::new(
                substitute(&w.condition, subst),
                substitute(&w.body, subst),
            )),
            expr.span,
        ),
        ExprKind::For(f) => Expr::new(
            ExprKind::For(ForExpr::new(
                f.var.clone(),
                substitute(&f.iterable, subst),
                substitute(&f.body, subst),
            )),
            expr.span,
        ),
        ExprKind::Assign(a) => {
            let new_target = match &a.target {
                AssignTarget::Variable(v) => {
                    // Symbolic args can be assigned to — apply the same subst.
                    if let Some(ExprKind::Variable(mapped)) =
                        subst.expr_subst.get(v).map(|e| &e.kind)
                    {
                        AssignTarget::Variable(mapped.clone())
                    } else {
                        AssignTarget::Variable(v.clone())
                    }
                }
                AssignTarget::Member { object, field } =>
                    AssignTarget::Member {
                        object: Box::new(substitute(object, subst)),
                        field: field.clone(),
                    },
                AssignTarget::Index { object, index } =>
                    AssignTarget::Index {
                        object: Box::new(substitute(object, subst)),
                        index: Box::new(substitute(index, subst)),
                    },
            };
            Expr::new(
                ExprKind::Assign(AssignExpr::new(new_target, substitute(&a.value, subst))),
                expr.span,
            )
        }
        ExprKind::Unary(u) => Expr::new(
            ExprKind::Unary(UnaryExpr { op: u.op, expr: Box::new(substitute(&u.expr, subst)) }),
            expr.span,
        ),
        ExprKind::Binary(b) => Expr::new(
            ExprKind::Binary(hulk_ast::BinaryExpr {
                op: b.op,
                left: Box::new(substitute(&b.left, subst)),
                right: Box::new(substitute(&b.right, subst)),
            }),
            expr.span,
        ),
        ExprKind::Call(c) => Expr::new(
            ExprKind::Call(hulk_ast::CallExpr {
                callee: Box::new(substitute(&c.callee, subst)),
                args: c.args.iter().map(|a| substitute(a, subst)).collect(),
            }),
            expr.span,
        ),
        ExprKind::Member(m) => Expr::new(
            ExprKind::Member(MemberExpr::new(substitute(&m.object, subst), m.member.clone())),
            expr.span,
        ),
        ExprKind::New(n) => Expr::new(
            ExprKind::New(NewExpr::new(
                n.type_name.clone(),
                n.args.iter().map(|a| substitute(a, subst)).collect(),
            )),
            expr.span,
        ),
        ExprKind::Index(i) => Expr::new(
            ExprKind::Index(IndexExpr::new(substitute(&i.object, subst), substitute(&i.index, subst))),
            expr.span,
        ),
        ExprKind::TypeTest(t) => Expr::new(
            ExprKind::TypeTest(TypeTestExpr::new(substitute(&t.expr, subst), t.type_name.clone())),
            expr.span,
        ),
        ExprKind::Downcast(d) => Expr::new(
            ExprKind::Downcast(DowncastExpr::new(substitute(&d.expr, subst), d.type_name.clone())),
            expr.span,
        ),
        ExprKind::Vector(v) => match v {
            VectorExpr::Literal(items) => Expr::new(
                ExprKind::Vector(VectorExpr::Literal(
                    items.iter().map(|i| substitute(i, subst)).collect()
                )),
                expr.span,
            ),
            VectorExpr::Comprehension(c) => Expr::new(
                ExprKind::Vector(VectorExpr::Comprehension(VectorComprehension::new(
                    substitute(&c.expr, subst),
                    c.var.clone(),
                    substitute(&c.iterable, subst),
                ))),
                expr.span,
            ),
        },
        ExprKind::Match(m) => Expr::new(
            ExprKind::Match(hulk_ast::MatchExpr::new(
                substitute(&m.value, subst),
                m.cases.iter().map(|c| MatchCase::new(c.pattern.clone(), substitute(&c.body, subst))).collect(),
            )),
            expr.span,
        ),
        ExprKind::Lambda(l) => Expr::new(
            ExprKind::Lambda(hulk_ast::LambdaExpr::new(
                l.params.clone(),
                l.return_type.clone(),
                substitute(&l.body, subst),
            )),
            expr.span,
        ),
        // MacroCall nodes inside a macro body are left for the next expansion pass.
        ExprKind::MacroCall(mc) => Expr::new(
            ExprKind::MacroCall(hulk_ast::MacroCallExpr::new(
                mc.name.clone(),
                mc.args.iter().map(|a| match a {
                    MacroArg::Expr(e) => MacroArg::Expr(substitute(e, subst)),
                    other => other.clone(),
                }).collect(),
                mc.body.as_deref().map(|b| substitute(b, subst)),
            )),
            expr.span,
        ),
        ExprKind::MacroMatch(mm) => {
            let new_scrutinee = substitute(&mm.scrutinee, subst);
            let new_cases = mm.cases
                .iter()
                .map(|case| MacroCase {
                    pattern: case.pattern.clone(), // patterns are not substituted
                    body: substitute(&case.body, subst),
                })
                .collect();
            Expr::new(
                ExprKind::MacroMatch(MacroMatchExpr {
                    scrutinee: Box::new(new_scrutinee),
                    cases: new_cases,
                }),
                expr.span,
            )
        }
        // Leaf nodes are returned unchanged.
        _ => expr.clone(),
    }
}