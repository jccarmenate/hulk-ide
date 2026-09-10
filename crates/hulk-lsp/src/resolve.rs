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

/// Finds the type of the first `Variable`/`self` reference named `name`
/// encountered while walking `program` top-down (declarations in order,
/// then the entry expression). Used by completion, where the expression
/// right before a `.` may not itself have parsed successfully yet (the
/// user is mid-edit) — so unlike `resolve_at`, this doesn't match an
/// exact position, and deliberately searches the *whole* program instead
/// of tracking scope. This can pick an unrelated same-named variable from
/// a different scope; see the completion plan's "Global Constraints" for
/// why that trade-off is accepted here.
pub fn find_named_type(program: &TypedProgram, name: &str) -> Option<Type> {
    let mut found = None;
    for decl in &program.declarations {
        find_name_in_declaration(decl, name, &mut found);
        if found.is_some() {
            return found;
        }
    }
    find_name_in_expr(&program.entry, name, &mut found);
    found
}

fn find_name_in_declaration(decl: &Declaration<Type>, name: &str, found: &mut Option<Type>) {
    match &decl.kind {
        DeclarationKind::Function(func) => find_name_in_expr(&func.body, name, found),
        DeclarationKind::Type(type_decl) => {
            for member in &type_decl.members {
                if let TypeMemberKind::Method(method) = &member.kind {
                    find_name_in_expr(&method.body, name, found);
                    if found.is_some() {
                        return;
                    }
                }
            }
        }
        DeclarationKind::Protocol(_) | DeclarationKind::Macro(_) => {}
    }
}

fn find_name_in_expr(expr: &TypedExpr, name: &str, found: &mut Option<Type>) {
    if found.is_some() {
        return;
    }
    match &expr.kind {
        ExprKind::Variable(n) => {
            if n == name {
                *found = Some(expr.anno.clone());
            }
        }
        ExprKind::SelfRef => {
            if name == "self" {
                *found = Some(expr.anno.clone());
            }
        }
        ExprKind::BaseRef | ExprKind::Literal(_) => {}
        ExprKind::Unary(u) => find_name_in_expr(&u.expr, name, found),
        ExprKind::Binary(b) => {
            find_name_in_expr(&b.left, name, found);
            find_name_in_expr(&b.right, name, found);
        }
        ExprKind::Let(l) => {
            for binding in &l.bindings {
                find_name_in_expr(&binding.initializer, name, found);
            }
            find_name_in_expr(&l.body, name, found);
        }
        ExprKind::Assign(a) => {
            match &a.target {
                AssignTarget::Variable(_) => {}
                AssignTarget::Member { object, .. } => find_name_in_expr(object, name, found),
                AssignTarget::Index { object, index } => {
                    find_name_in_expr(object, name, found);
                    find_name_in_expr(index, name, found);
                }
            }
            find_name_in_expr(&a.value, name, found);
        }
        ExprKind::Block(b) => {
            for e in &b.expressions {
                find_name_in_expr(e, name, found);
            }
        }
        ExprKind::If(i) => {
            find_name_in_expr(&i.condition, name, found);
            find_name_in_expr(&i.then_branch, name, found);
            for elif in &i.elif_branches {
                find_name_in_expr(&elif.condition, name, found);
                find_name_in_expr(&elif.body, name, found);
            }
            find_name_in_expr(&i.else_branch, name, found);
        }
        ExprKind::While(w) => {
            find_name_in_expr(&w.condition, name, found);
            find_name_in_expr(&w.body, name, found);
        }
        ExprKind::For(f) => {
            find_name_in_expr(&f.iterable, name, found);
            find_name_in_expr(&f.body, name, found);
        }
        ExprKind::Call(c) => {
            find_name_in_expr(&c.callee, name, found);
            for a in &c.args {
                find_name_in_expr(a, name, found);
            }
        }
        ExprKind::Lambda(l) => find_name_in_expr(&l.body, name, found),
        ExprKind::Member(m) => find_name_in_expr(&m.object, name, found),
        ExprKind::New(n) => {
            for a in &n.args {
                find_name_in_expr(a, name, found);
            }
        }
        ExprKind::TypeTest(t) => find_name_in_expr(&t.expr, name, found),
        ExprKind::Downcast(d) => find_name_in_expr(&d.expr, name, found),
        ExprKind::Index(idx) => {
            find_name_in_expr(&idx.object, name, found);
            find_name_in_expr(&idx.index, name, found);
        }
        ExprKind::Vector(v) => match v {
            VectorExpr::Literal(items) => {
                for e in items {
                    find_name_in_expr(e, name, found);
                }
            }
            VectorExpr::Comprehension(c) => {
                find_name_in_expr(&c.iterable, name, found);
                find_name_in_expr(&c.expr, name, found);
            }
        },
        ExprKind::Match(m) => {
            find_name_in_expr(&m.value, name, found);
            for case in &m.cases {
                find_name_in_expr(&case.body, name, found);
            }
        }
        ExprKind::MacroCall(_) | ExprKind::MacroMatch(_) => {}
    }
}

/// Every name introduced by a `let`, `for`, function/method parameter,
/// `self`, or lambda parameter, anywhere in `program`. Not scope-filtered
/// — see the completion plan's "Global Constraints" for why that's an
/// accepted trade-off for completion specifically.
pub fn all_declared_names(program: &TypedProgram) -> Vec<String> {
    let mut names = Vec::new();
    for decl in &program.declarations {
        match &decl.kind {
            DeclarationKind::Function(func) => collect_names_in_function(func, true, &mut names),
            DeclarationKind::Type(type_decl) => {
                for member in &type_decl.members {
                    if let TypeMemberKind::Method(method) = &member.kind {
                        collect_names_in_function(method, true, &mut names);
                    }
                }
            }
            DeclarationKind::Protocol(_) | DeclarationKind::Macro(_) => {}
        }
    }
    collect_names_in_expr(&program.entry, &mut names);
    names
}

fn collect_names_in_function(func: &FunctionDecl<Type>, is_method: bool, names: &mut Vec<String>) {
    if is_method {
        names.push("self".to_string());
    }
    for p in &func.params {
        names.push(p.name.clone());
    }
    collect_names_in_expr(&func.body, names);
}

fn collect_names_in_expr(expr: &TypedExpr, names: &mut Vec<String>) {
    match &expr.kind {
        ExprKind::Let(l) => {
            for b in &l.bindings {
                names.push(b.name.clone());
                collect_names_in_expr(&b.initializer, names);
            }
            collect_names_in_expr(&l.body, names);
        }
        ExprKind::For(f) => {
            names.push(f.var.clone());
            collect_names_in_expr(&f.iterable, names);
            collect_names_in_expr(&f.body, names);
        }
        ExprKind::Lambda(l) => {
            for p in &l.params {
                names.push(p.name.clone());
            }
            collect_names_in_expr(&l.body, names);
        }
        ExprKind::Unary(u) => collect_names_in_expr(&u.expr, names),
        ExprKind::Binary(b) => {
            collect_names_in_expr(&b.left, names);
            collect_names_in_expr(&b.right, names);
        }
        ExprKind::Assign(a) => {
            match &a.target {
                AssignTarget::Member { object, .. } => collect_names_in_expr(object, names),
                AssignTarget::Index { object, index } => {
                    collect_names_in_expr(object, names);
                    collect_names_in_expr(index, names);
                }
                AssignTarget::Variable(_) => {}
            }
            collect_names_in_expr(&a.value, names);
        }
        ExprKind::Block(b) => {
            for e in &b.expressions {
                collect_names_in_expr(e, names);
            }
        }
        ExprKind::If(i) => {
            collect_names_in_expr(&i.condition, names);
            collect_names_in_expr(&i.then_branch, names);
            for elif in &i.elif_branches {
                collect_names_in_expr(&elif.condition, names);
                collect_names_in_expr(&elif.body, names);
            }
            collect_names_in_expr(&i.else_branch, names);
        }
        ExprKind::While(w) => {
            collect_names_in_expr(&w.condition, names);
            collect_names_in_expr(&w.body, names);
        }
        ExprKind::Call(c) => {
            collect_names_in_expr(&c.callee, names);
            for a in &c.args {
                collect_names_in_expr(a, names);
            }
        }
        ExprKind::Member(m) => collect_names_in_expr(&m.object, names),
        ExprKind::New(n) => {
            for a in &n.args {
                collect_names_in_expr(a, names);
            }
        }
        ExprKind::TypeTest(t) => collect_names_in_expr(&t.expr, names),
        ExprKind::Downcast(d) => collect_names_in_expr(&d.expr, names),
        ExprKind::Index(idx) => {
            collect_names_in_expr(&idx.object, names);
            collect_names_in_expr(&idx.index, names);
        }
        ExprKind::Vector(v) => match v {
            VectorExpr::Literal(items) => {
                for e in items {
                    collect_names_in_expr(e, names);
                }
            }
            VectorExpr::Comprehension(c) => {
                collect_names_in_expr(&c.iterable, names);
                collect_names_in_expr(&c.expr, names);
            }
        },
        ExprKind::Match(m) => {
            collect_names_in_expr(&m.value, names);
            for case in &m.cases {
                collect_names_in_expr(&case.body, names);
            }
        }
        ExprKind::Variable(_)
        | ExprKind::SelfRef
        | ExprKind::BaseRef
        | ExprKind::Literal(_)
        | ExprKind::MacroCall(_)
        | ExprKind::MacroMatch(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze_source(source: &str) -> hulk_semantic::VerifiedProgram {
        let tokens = hulk_lexer::Lexer::new(source)
            .tokenize()
            .expect("valid tokens");
        let mut program = hulk_parser::parse(tokens).expect("valid parse");
        hulk_transpile::expand_program(&mut program);
        hulk_semantic::analyze(&program).expect("valid program")
    }

    #[test]
    fn resolves_a_let_bound_variable_to_its_declaration_and_type() {
        let verified = analyze_source("let x = 5 in\nx + 1;");
        let hit = resolve_at(
            &verified.typed_program,
            Position {
                line: 1,
                character: 0,
            },
        )
        .expect("hit");
        assert_eq!(hit.name, "x");
        assert_eq!(hit.ty, Type::Number);
        assert_eq!(hit.definition, Some(SourceSpan::new(1, 5)));
    }

    #[test]
    fn resolves_a_function_parameter_to_its_declaration_and_type() {
        let verified = analyze_source("function f(x: Number): Number =>\n    x;\nprint(f(1));");
        let hit = resolve_at(
            &verified.typed_program,
            Position {
                line: 1,
                character: 4,
            },
        )
        .expect("hit");
        assert_eq!(hit.name, "x");
        assert_eq!(hit.ty, Type::Number);
        assert_eq!(hit.definition, Some(SourceSpan::new(1, 12)));
    }

    #[test]
    fn resolves_a_for_loop_variable_to_its_declaration() {
        let verified = analyze_source("for (x in range(1, 10))\n    print(x);");
        let hit = resolve_at(
            &verified.typed_program,
            Position {
                line: 1,
                character: 10,
            },
        )
        .expect("hit");
        assert_eq!(hit.name, "x");
        assert_eq!(hit.definition, Some(SourceSpan::new(1, 6)));
    }

    #[test]
    fn resolves_self_inside_a_method_to_the_enclosing_type() {
        let verified = analyze_source("type A {\n    f(): A => self;\n}\nprint(new A().f());");
        let hit = resolve_at(
            &verified.typed_program,
            Position {
                line: 1,
                character: 14,
            },
        )
        .expect("hit");
        assert_eq!(hit.name, "self");
        assert_eq!(hit.ty, Type::Named("A".to_string()));
    }

    #[test]
    fn resolves_a_member_access_by_its_member_span() {
        let verified = analyze_source("type A {\n    b(): Number => 1;\n}\nnew A().b();");
        let hit = resolve_at(
            &verified.typed_program,
            Position {
                line: 3,
                character: 8,
            },
        )
        .expect("hit");
        assert_eq!(hit.name, "b");
        assert!(hit.receiver_type.is_some());
    }

    #[test]
    fn returns_none_over_a_literal() {
        let verified = analyze_source("print(1);");
        assert!(resolve_at(
            &verified.typed_program,
            Position {
                line: 0,
                character: 6
            }
        )
        .is_none());
    }

    #[test]
    fn find_named_type_finds_a_let_bound_variable() {
        let verified = analyze_source("let x = 5 in\nx + 1;");
        let ty = find_named_type(&verified.typed_program, "x").expect("type");
        assert_eq!(ty, Type::Number);
    }

    #[test]
    fn find_named_type_finds_self_inside_a_method() {
        let verified = analyze_source("type A {\n    f(): A => self;\n}\nprint(new A().f());");
        let ty = find_named_type(&verified.typed_program, "self").expect("type");
        assert_eq!(ty, Type::Named("A".to_string()));
    }

    #[test]
    fn find_named_type_returns_none_for_an_unknown_name() {
        let verified = analyze_source("print(1);");
        assert!(find_named_type(&verified.typed_program, "nope").is_none());
    }

    #[test]
    fn all_declared_names_lists_every_binding() {
        let verified = analyze_source(
            "function f(x: Number): Number =>\n    let y = x in y;\nfor (z in range(1, 3))\n    print(z);",
        );
        let names = all_declared_names(&verified.typed_program);
        assert!(names.contains(&"x".to_string()));
        assert!(names.contains(&"y".to_string()));
        assert!(names.contains(&"z".to_string()));
    }
}
