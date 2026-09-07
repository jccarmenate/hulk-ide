//! Resolves an editor cursor position against a document's last
//! successfully analyzed typed AST: what's there, what type is it, and
//! where was it declared (when the AST tracks a precise enough position).

use hulk_ast::{
    AssignTarget, Declaration, DeclarationKind, ExprKind, FunctionDecl, SourceSpan, TypeMemberKind,
    VectorExpr,
};
use hulk_semantic::{Type, TypedExpr, TypedProgram};
use tower_lsp::lsp_types::Position;

/// What a resolved identifier reference is: its name, resolved type, and
/// (when known) where it was declared.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub name: String,
    pub ty: Type,
    /// Where this name was declared — known for variables, `self`, and
    /// parameters (found by walking scope); left `None` for `.member`
    /// accesses, which need the type registry instead (see `backend.rs`).
    pub definition: Option<SourceSpan>,
    /// Set only for a `.member` access: the type of the receiver
    /// (`object` in `object.member`), needed to look the member up in the
    /// `TypeRegistry`.
    pub receiver_type: Option<Type>,
}

pub fn resolve_at(program: &TypedProgram, position: Position) -> Option<Hit> {
    let mut hit = None;
    for decl in &program.declarations {
        walk_declaration(decl, position, &mut hit);
        if hit.is_some() {
            return hit;
        }
    }
    let mut scopes = Scopes::new();
    walk_expr(&program.entry, &mut scopes, position, &mut hit);
    hit
}

/// A simple lexical scope stack mapping names to their declaration span.
/// Mirrors the scope-introduction rules `hulk_semantic::Environment`
/// documents (`let` body, function/method body, and `for` loop body each
/// push a scope; plain `{ }` blocks don't) — but unlike `Environment`,
/// this only tracks positions, not types. Hover reads the resolved type
/// directly off the matched AST node's `anno`, which the compiler already
/// computed correctly, so there's nothing to re-infer here.
struct Scopes {
    frames: Vec<Vec<(String, SourceSpan)>>,
}

impl Scopes {
    fn new() -> Self {
        Self {
            frames: vec![Vec::new()],
        }
    }

    fn push(&mut self) {
        self.frames.push(Vec::new());
    }

    fn pop(&mut self) {
        self.frames.pop();
    }

    fn declare(&mut self, name: &str, span: SourceSpan) {
        self.frames
            .last_mut()
            .expect("at least one scope exists")
            .push((name.to_string(), span));
    }

    /// Where `name` was declared: innermost scope first, and within a
    /// scope, most recently declared first (shadowing).
    fn lookup(&self, name: &str) -> Option<SourceSpan> {
        for frame in self.frames.iter().rev() {
            for (n, span) in frame.iter().rev() {
                if n == name {
                    return Some(*span);
                }
            }
        }
        None
    }
}

fn walk_declaration(decl: &Declaration<Type>, position: Position, hit: &mut Option<Hit>) {
    match &decl.kind {
        DeclarationKind::Function(func) => {
            let mut scopes = Scopes::new();
            walk_function(func, false, &mut scopes, position, hit);
        }
        DeclarationKind::Type(type_decl) => {
            for member in &type_decl.members {
                if let TypeMemberKind::Method(method) = &member.kind {
                    let mut scopes = Scopes::new();
                    walk_function(method, true, &mut scopes, position, hit);
                    if hit.is_some() {
                        return;
                    }
                }
            }
        }
        DeclarationKind::Protocol(_) | DeclarationKind::Macro(_) => {}
    }
}

fn walk_function(
    func: &FunctionDecl<Type>,
    is_method: bool,
    scopes: &mut Scopes,
    position: Position,
    hit: &mut Option<Hit>,
) {
    if is_method {
        // `self` has no dedicated span in the AST; the function body's
        // own span is the closest available anchor (this is also what
        // `hulk-semantic`'s own inference pass uses internally).
        scopes.declare("self", func.body.span);
    }
    for param in &func.params {
        scopes.declare(&param.name, param.name_span);
    }
    walk_expr(&func.body, scopes, position, hit);
}

fn span_contains(span: SourceSpan, len: usize, position: Position) -> bool {
    if span.line == 0 || len == 0 {
        return false;
    }
    let line = (span.line - 1) as u32;
    let start = (span.col - 1) as u32;
    let end = start + len as u32;
    position.line == line && position.character >= start && position.character < end
}

fn walk_expr(expr: &TypedExpr, scopes: &mut Scopes, position: Position, hit: &mut Option<Hit>) {
    if hit.is_some() {
        return;
    }

    match &expr.kind {
        ExprKind::Variable(name) => {
            if span_contains(expr.span, name.len(), position) {
                *hit = Some(Hit {
                    name: name.clone(),
                    ty: expr.anno.clone(),
                    definition: scopes.lookup(name),
                    receiver_type: None,
                });
            }
        }
        ExprKind::SelfRef => {
            if span_contains(expr.span, "self".len(), position) {
                *hit = Some(Hit {
                    name: "self".to_string(),
                    ty: expr.anno.clone(),
                    definition: scopes.lookup("self"),
                    receiver_type: None,
                });
            }
        }
        ExprKind::BaseRef => {
            if span_contains(expr.span, "base".len(), position) {
                *hit = Some(Hit {
                    name: "base".to_string(),
                    ty: expr.anno.clone(),
                    definition: None,
                    receiver_type: None,
                });
            }
        }
        ExprKind::Literal(_) => {}
        ExprKind::Unary(u) => walk_expr(&u.expr, scopes, position, hit),
        ExprKind::Binary(b) => {
            walk_expr(&b.left, scopes, position, hit);
            walk_expr(&b.right, scopes, position, hit);
        }
        ExprKind::Let(let_expr) => {
            for binding in &let_expr.bindings {
                walk_expr(&binding.initializer, scopes, position, hit);
            }
            scopes.push();
            for binding in &let_expr.bindings {
                scopes.declare(&binding.name, binding.name_span);
            }
            walk_expr(&let_expr.body, scopes, position, hit);
            scopes.pop();
        }
        ExprKind::Assign(a) => {
            match &a.target {
                AssignTarget::Variable(_) => {}
                AssignTarget::Member { object, .. } => walk_expr(object, scopes, position, hit),
                AssignTarget::Index { object, index } => {
                    walk_expr(object, scopes, position, hit);
                    walk_expr(index, scopes, position, hit);
                }
            }
            walk_expr(&a.value, scopes, position, hit);
        }
        ExprKind::Block(b) => {
            for e in &b.expressions {
                walk_expr(e, scopes, position, hit);
            }
        }
        ExprKind::If(i) => {
            walk_expr(&i.condition, scopes, position, hit);
            walk_expr(&i.then_branch, scopes, position, hit);
            for elif in &i.elif_branches {
                walk_expr(&elif.condition, scopes, position, hit);
                walk_expr(&elif.body, scopes, position, hit);
            }
            walk_expr(&i.else_branch, scopes, position, hit);
        }
        ExprKind::While(w) => {
            walk_expr(&w.condition, scopes, position, hit);
            walk_expr(&w.body, scopes, position, hit);
        }
        ExprKind::For(f) => {
            walk_expr(&f.iterable, scopes, position, hit);
            scopes.push();
            scopes.declare(&f.var, f.var_span);
            walk_expr(&f.body, scopes, position, hit);
            scopes.pop();
        }
        ExprKind::Call(c) => {
            walk_expr(&c.callee, scopes, position, hit);
            for a in &c.args {
                walk_expr(a, scopes, position, hit);
            }
        }
        ExprKind::Lambda(l) => {
            scopes.push();
            for p in &l.params {
                scopes.declare(&p.name, p.name_span);
            }
            walk_expr(&l.body, scopes, position, hit);
            scopes.pop();
        }
        ExprKind::Member(m) => {
            walk_expr(&m.object, scopes, position, hit);
            if hit.is_none() && span_contains(m.member_span, m.member.len(), position) {
                *hit = Some(Hit {
                    name: m.member.clone(),
                    ty: expr.anno.clone(),
                    definition: None,
                    receiver_type: Some(m.object.anno.clone()),
                });
            }
        }
        ExprKind::New(n) => {
            for a in &n.args {
                walk_expr(a, scopes, position, hit);
            }
        }
        ExprKind::TypeTest(t) => walk_expr(&t.expr, scopes, position, hit),
        ExprKind::Downcast(d) => walk_expr(&d.expr, scopes, position, hit),
        ExprKind::Index(idx) => {
            walk_expr(&idx.object, scopes, position, hit);
            walk_expr(&idx.index, scopes, position, hit);
        }
        ExprKind::Vector(v) => match v {
            VectorExpr::Literal(items) => {
                for e in items {
                    walk_expr(e, scopes, position, hit);
                }
            }
            VectorExpr::Comprehension(c) => {
                walk_expr(&c.iterable, scopes, position, hit);
                // `VectorComprehension.var` has no span of its own (a
                // known limitation — see this plan's header), so the
                // comprehension variable isn't added to scope: hover
                // still works on any *use* of it inside `expr` (its
                // `anno` is still correct), but go-to-definition won't
                // find where it was introduced.
                walk_expr(&c.expr, scopes, position, hit);
            }
        },
        ExprKind::Match(m) => {
            walk_expr(&m.value, scopes, position, hit);
            for case in &m.cases {
                // Pattern bindings (`case n: Number`) aren't declarations
                // with a span either — same limitation as above.
                walk_expr(&case.body, scopes, position, hit);
            }
        }
        ExprKind::MacroCall(_) | ExprKind::MacroMatch(_) => {
            // Never present in a typed tree — expand_program removes
            // these before semantic analysis runs.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze_source(source: &str) -> hulk_semantic::VerifiedProgram {
        let tokens = hulk_lexer::Lexer::new(source).tokenize().expect("valid tokens");
        let mut program = hulk_parser::parse(tokens).expect("valid parse");
        hulk_transpile::expand_program(&mut program);
        hulk_semantic::analyze(&program).expect("valid program")
    }

    #[test]
    fn resolves_a_let_bound_variable_to_its_declaration_and_type() {
        let verified = analyze_source("let x = 5 in\nx + 1;");
        let hit = resolve_at(&verified.typed_program, Position { line: 1, character: 0 })
            .expect("hit");
        assert_eq!(hit.name, "x");
        assert_eq!(hit.ty, Type::Number);
        assert_eq!(hit.definition, Some(SourceSpan::new(1, 5)));
    }

    #[test]
    fn resolves_a_function_parameter_to_its_declaration_and_type() {
        let verified =
            analyze_source("function f(x: Number): Number =>\n    x;\nprint(f(1));");
        let hit = resolve_at(&verified.typed_program, Position { line: 1, character: 4 })
            .expect("hit");
        assert_eq!(hit.name, "x");
        assert_eq!(hit.ty, Type::Number);
        assert_eq!(hit.definition, Some(SourceSpan::new(1, 12)));
    }

    #[test]
    fn resolves_a_for_loop_variable_to_its_declaration() {
        let verified = analyze_source("for (x in range(1, 10))\n    print(x);");
        let hit = resolve_at(&verified.typed_program, Position { line: 1, character: 10 })
            .expect("hit");
        assert_eq!(hit.name, "x");
        assert_eq!(hit.definition, Some(SourceSpan::new(1, 6)));
    }

    #[test]
    fn resolves_self_inside_a_method_to_the_enclosing_type() {
        let verified =
            analyze_source("type A {\n    f(): A => self;\n}\nprint(new A().f());");
        let hit = resolve_at(&verified.typed_program, Position { line: 1, character: 14 })
            .expect("hit");
        assert_eq!(hit.name, "self");
        assert_eq!(hit.ty, Type::Named("A".to_string()));
    }

    #[test]
    fn resolves_a_member_access_by_its_member_span() {
        let verified = analyze_source("type A {\n    b(): Number => 1;\n}\nnew A().b();");
        let hit = resolve_at(&verified.typed_program, Position { line: 3, character: 8 })
            .expect("hit");
        assert_eq!(hit.name, "b");
        assert!(hit.receiver_type.is_some());
    }

    #[test]
    fn returns_none_over_a_literal() {
        let verified = analyze_source("print(1);");
        assert!(resolve_at(&verified.typed_program, Position { line: 0, character: 6 })
            .is_none());
    }
}
