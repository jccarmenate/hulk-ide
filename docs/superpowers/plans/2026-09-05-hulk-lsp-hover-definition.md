# hulk-lsp Hover and Go-to-Definition Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `textDocument/hover` and `textDocument/definition` to `hulk-lsp`, using the precise spans Plan 3 added to `Param`/`LetBinding`/`MemberExpr`/`ForExpr`. Hover shows the resolved type of the identifier under the cursor; go-to-definition jumps to where it was declared (a `let`/`for`/function-or-method parameter) or, for a `.member` access, to its declaration in the type (an attribute or method), via `hulk-semantic`'s `TypeRegistry`.

**Architecture:** A new pure module, `resolve.rs`, walks a document's last successfully analyzed typed AST (`hulk_semantic::VerifiedProgram`, cached per document after Plan 2's diagnostics pass) to find the `Variable`/`self`/`.member` reference under the cursor. It returns a `Hit` carrying the reference's name, its resolved type (read directly from the AST node's already-computed `anno` — no re-inference needed), and, for a plain name, where it was declared (via a small scope stack this module tracks itself, mirroring the scope-introduction rules `hulk_semantic::Environment` documents, but tracking only positions rather than duplicating type inference). For a `.member` access, `resolve_at` deliberately leaves the definition unresolved — that lookup needs `TypeRegistry`, not scope tracking, and is done separately in `backend.rs` by finding the member as a method or (walking up the inheritance chain) an attribute. Both `textDocument/hover` and `textDocument/definition` are implemented as thin trait methods delegating to pure, directly-testable functions (`hover_response`, `definition_response`), following the same split `diagnostics.rs` already uses.

**Tech Stack:** Rust, existing `hulk-lsp` crate and its dependencies. No new dependencies.

**Spec:** [docs/superpowers/specs/2026-09-01-hulk-ide-design.md](../specs/2026-09-01-hulk-ide-design.md) — "Hover / go-to-definition / completion" section. This plan implements hover and go-to-definition only; completion is a separate follow-up plan (it needs to reason about a partially-typed, likely-unparseable buffer — a different, more approximate problem than looking things up in a tree that already parsed and analyzed cleanly).

## Global Constraints

- `resolve.rs` takes only `&hulk_semantic::TypedProgram` / `Position` and returns plain data — no `tower_lsp` `Client`, no I/O. Every position-resolution test in this plan calls it directly against a real analyzed program (lex → parse → expand → analyze on a literal source string), the same testing style `diagnostics.rs` already uses.
- Hover never needs to re-derive a type: every node in the last-good typed tree already carries its correct, compiler-computed type in `anno`. Only go-to-definition needs the scope walk (and, for members, the type registry).
- Known limitations, by design (documented here rather than silently attempted): go-to-definition does not resolve vector-comprehension loop variables or `match`-case pattern bindings (`hulk_ast::VectorComprehension.var` and `Pattern` bindings carry no span — Plan 3 did not touch them, since they're rarer and would have expanded that plan's already-large scope). Hover still works on any *use* of such a variable inside its body, since that use's own `anno` is unaffected — only jumping to its declaration is unavailable.

---

### Task 1: Cache the last-good analyzed program per document

**Files:**
- Modify: `crates/hulk-lsp/src/diagnostics.rs`
- Modify: `crates/hulk-lsp/src/backend.rs`

**Interfaces:**
- Produces: `pub struct DiagnosticsOutcome { pub diagnostics: Vec<Diagnostic>, pub last_good: Option<hulk_semantic::VerifiedProgram> }`, replacing `compute_diagnostics`'s old bare `Vec<Diagnostic>` return type. `pub(crate) fn span_to_range(line: usize, col: usize) -> Range` (was private — Task 4 needs it too). `Backend`'s document map now stores text *and* the last successfully analyzed program, so hover/definition (Tasks 3–4) have something to query.

- [ ] **Step 1: Change `compute_diagnostics`'s return type and update its tests**

In `crates/hulk-lsp/src/diagnostics.rs`, add this struct right above `compute_diagnostics`:

```rust
/// Everything one call to [`compute_diagnostics`] produces: the
/// diagnostics to publish, and — when analysis succeeded — the fully
/// typed program, kept around so hover/completion/go-to-definition can
/// query it even while a *later* edit has a syntax error (see the
/// "last-good tree" idea in the design spec).
pub struct DiagnosticsOutcome {
    pub diagnostics: Vec<Diagnostic>,
    pub last_good: Option<hulk_semantic::VerifiedProgram>,
}
```

Then change the signature and body of `compute_diagnostics` from:

```rust
pub fn compute_diagnostics(text: &str) -> Vec<Diagnostic> {
    let (tokens, lex_errors) = hulk_lexer::Lexer::new(text).tokenize_recovering();
    if !lex_errors.is_empty() {
        return lex_errors.iter().map(lex_diagnostic).collect();
    }

    let (mut program, parse_errors) = hulk_parser::parse_recovering(tokens);
    let mut diagnostics: Vec<Diagnostic> = parse_errors
        .iter()
        .map(|err| diagnostic(err.span.line, err.span.col, DiagnosticSeverity::ERROR, err.to_string()))
        .collect();

    let macro_errors = hulk_transpile::expand_program(&mut program);
    diagnostics.extend(macro_errors.iter().map(|err| {
        diagnostic(err.span.line, err.span.col, DiagnosticSeverity::ERROR, err.kind.to_string())
    }));

    match hulk_semantic::analyze(&program) {
        Err(sem_errors) => diagnostics.extend(
            sem_errors
                .iter()
                .map(|err| semantic_diagnostic(err, DiagnosticSeverity::ERROR)),
        ),
        Ok(verified) => diagnostics.extend(
            verified
                .warnings
                .iter()
                .map(|err| semantic_diagnostic(err, DiagnosticSeverity::WARNING)),
        ),
    }

    diagnostics
}
```

to:

```rust
pub fn compute_diagnostics(text: &str) -> DiagnosticsOutcome {
    let (tokens, lex_errors) = hulk_lexer::Lexer::new(text).tokenize_recovering();
    if !lex_errors.is_empty() {
        return DiagnosticsOutcome {
            diagnostics: lex_errors.iter().map(lex_diagnostic).collect(),
            last_good: None,
        };
    }

    let (mut program, parse_errors) = hulk_parser::parse_recovering(tokens);
    let mut diagnostics: Vec<Diagnostic> = parse_errors
        .iter()
        .map(|err| diagnostic(err.span.line, err.span.col, DiagnosticSeverity::ERROR, err.to_string()))
        .collect();

    let macro_errors = hulk_transpile::expand_program(&mut program);
    diagnostics.extend(macro_errors.iter().map(|err| {
        diagnostic(err.span.line, err.span.col, DiagnosticSeverity::ERROR, err.kind.to_string())
    }));

    let last_good = match hulk_semantic::analyze(&program) {
        Err(sem_errors) => {
            diagnostics.extend(
                sem_errors
                    .iter()
                    .map(|err| semantic_diagnostic(err, DiagnosticSeverity::ERROR)),
            );
            None
        }
        Ok(verified) => {
            diagnostics.extend(
                verified
                    .warnings
                    .iter()
                    .map(|err| semantic_diagnostic(err, DiagnosticSeverity::WARNING)),
            );
            Some(verified)
        }
    };

    DiagnosticsOutcome {
        diagnostics,
        last_good,
    }
}
```

Make `span_to_range` crate-visible — change:

```rust
fn span_to_range(line: usize, col: usize) -> Range {
```

to:

```rust
pub(crate) fn span_to_range(line: usize, col: usize) -> Range {
```

Now fix the five existing tests in this file's `mod tests` (each currently does `let diagnostics = compute_diagnostics(...)`) — change every one of those five call sites from:

```rust
        let diagnostics = compute_diagnostics(...);
```

to:

```rust
        let diagnostics = compute_diagnostics(...).diagnostics;
```

(the `...` is each test's existing source-string argument, unchanged — only add `.diagnostics` after the call).

Then add one new test after the existing five, verifying the new field:

```rust
    #[test]
    fn last_good_is_populated_on_success_and_absent_on_semantic_error() {
        let ok = compute_diagnostics("print(1 + 2);");
        assert!(ok.last_good.is_some());

        let broken = compute_diagnostics("{ print(a); }");
        assert!(broken.last_good.is_none());
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p hulk-lsp`
Expected: compile errors at first (`Backend::publish_for` still expects a bare `Vec<Diagnostic>`) — fix that next, then re-run.

- [ ] **Step 3: Cache `last_good` in `Backend`**

In `crates/hulk-lsp/src/backend.rs`, replace the `Backend` struct and `Backend::new`/`publish_for`:

```rust
/// The HULK language server.
pub struct Backend {
    client: Client,
    documents: RwLock<HashMap<Url, DocumentState>>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: RwLock::new(HashMap::new()),
        }
    }

    /// Recomputes and publishes diagnostics for `uri` from its currently
    /// stored text. No-op if the document isn't open (e.g. it was already
    /// closed by the time this runs). Also refreshes the cached last-good
    /// analyzed program, when analysis succeeded.
    async fn publish_for(&self, uri: Url) {
        let text = {
            let docs = self.documents.read().unwrap();
            docs.get(&uri).map(|state| state.text.clone())
        };
        let Some(text) = text else { return };

        let outcome = compute_diagnostics(&text);
        if outcome.last_good.is_some() {
            let mut docs = self.documents.write().unwrap();
            if let Some(state) = docs.get_mut(&uri) {
                state.last_good = outcome.last_good;
            }
        }

        self.client.publish_diagnostics(uri, outcome.diagnostics, None).await;
    }
}

/// Per-document state: the current text, and the most recently
/// successfully analyzed program (if any is available yet).
struct DocumentState {
    text: String,
    last_good: Option<hulk_semantic::VerifiedProgram>,
}
```

Then update `did_open`/`did_change`/`did_close` — replace:

```rust
    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        self.documents
            .write()
            .unwrap()
            .insert(uri.clone(), params.text_document.text);
        self.publish_for(uri).await;
    }

    async fn did_change(&self, mut params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        // Full-document sync (see `server_capabilities`): the client always
        // sends exactly one change event containing the whole new text.
        let Some(change) = params.content_changes.pop() else {
            return;
        };
        self.documents.write().unwrap().insert(uri.clone(), change.text);
        self.publish_for(uri).await;
    }
```

with:

```rust
    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        self.documents.write().unwrap().insert(
            uri.clone(),
            DocumentState {
                text: params.text_document.text,
                last_good: None,
            },
        );
        self.publish_for(uri).await;
    }

    async fn did_change(&self, mut params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        // Full-document sync (see `server_capabilities`): the client always
        // sends exactly one change event containing the whole new text.
        let Some(change) = params.content_changes.pop() else {
            return;
        };
        self.documents.write().unwrap().insert(
            uri.clone(),
            DocumentState {
                text: change.text,
                last_good: None,
            },
        );
        self.publish_for(uri).await;
    }
```

`did_close` is unchanged (it already just removes the whole map entry).

- [ ] **Step 4: Run the tests and build**

Run: `cargo test -p hulk-lsp`
Expected: PASS — all previous tests plus the new `last_good_is_populated_...` test.

Run: `cargo build -p hulk-lsp`
Expected: builds cleanly.

- [ ] **Step 5: Commit**

```bash
git add crates/hulk-lsp/src/diagnostics.rs crates/hulk-lsp/src/backend.rs
git commit -m "feat(lsp): cache the last successfully analyzed program per document"
```

---

### Task 2: Position resolution (`resolve.rs`)

**Files:**
- Create: `crates/hulk-lsp/src/resolve.rs`
- Modify: `crates/hulk-lsp/src/main.rs` (add `mod resolve;`)

**Interfaces:**
- Produces: `pub struct Hit { pub name: String, pub ty: Type, pub definition: Option<SourceSpan>, pub receiver_type: Option<Type> }` and `pub fn resolve_at(program: &TypedProgram, position: Position) -> Option<Hit>`. `receiver_type` is `Some` only for a `.member` hit (needed by Task 4 to look the member up in the `TypeRegistry`); `definition` is filled in directly here for variables/`self`/parameters/`for`-loop variables, and left `None` for members (resolved separately, from the registry, in Task 4).

- [ ] **Step 1: Write the failing tests**

Create `crates/hulk-lsp/src/resolve.rs`:

```rust
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
    todo!()
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
```

- [ ] **Step 2: Register the module and run the tests to verify they fail**

Add `mod resolve;` to `crates/hulk-lsp/src/main.rs`, after `mod diagnostics;`:

```rust
mod backend;
mod diagnostics;
mod resolve;
```

Run: `cargo test -p hulk-lsp resolve::`
Expected: compiles, then panics with `not yet implemented`.

- [ ] **Step 3: Implement `resolve_at`**

Replace the `todo!()` body of `resolve_at` and add its helper functions (everything below goes between the `Hit` struct and the `#[cfg(test)]` block):

```rust
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
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hulk-lsp resolve::`
Expected: PASS — all 6 tests. If a position/column assertion is off by one, re-count the source string's characters carefully (1-based `SourceSpan` vs. 0-based `Position`) rather than adjusting `span_contains`'s logic to match a wrong expectation.

- [ ] **Step 5: Commit**

```bash
git add crates/hulk-lsp/src/resolve.rs crates/hulk-lsp/src/main.rs
git commit -m "feat(lsp): add resolve_at for position-to-declaration lookup"
```

---

### Task 3: Hover

**Files:**
- Modify: `crates/hulk-lsp/src/backend.rs`

**Interfaces:**
- Consumes: `resolve::resolve_at` (Task 2).
- Produces: `textDocument/hover` support, advertised via `server_capabilities`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/hulk-lsp/src/backend.rs`'s `mod tests` (after the existing `server_capabilities_use_full_text_document_sync` test):

```rust
    fn test_analyze(source: &str) -> hulk_semantic::VerifiedProgram {
        let tokens = hulk_lexer::Lexer::new(source).tokenize().expect("valid tokens");
        let mut program = hulk_parser::parse(tokens).expect("valid parse");
        hulk_transpile::expand_program(&mut program);
        hulk_semantic::analyze(&program).expect("valid program")
    }

    #[test]
    fn hover_response_shows_the_resolved_type() {
        let verified = test_analyze("let x = 5 in\nx + 1;");
        let hover = hover_response(&verified, Position { line: 1, character: 0 }).expect("hover");
        match hover.contents {
            HoverContents::Markup(markup) => assert!(markup.value.contains("x: Number")),
            other => panic!("expected markup hover, got {other:?}"),
        }
    }

    #[test]
    fn hover_response_is_none_over_a_literal() {
        let verified = test_analyze("print(1);");
        assert!(hover_response(&verified, Position { line: 0, character: 6 }).is_none());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p hulk-lsp hover_response`
Expected: compile error — `hover_response` is not defined.

- [ ] **Step 3: Implement `hover_response` and wire it into the trait**

Add these imports to `crates/hulk-lsp/src/backend.rs`'s existing `use tower_lsp::lsp_types::{...}` line — extend it to also bring in `Hover, HoverContents, HoverParams, HoverProviderCapability, MarkupContent, MarkupKind, OneOf`:

```rust
use tower_lsp::lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams, Hover,
    HoverContents, HoverParams, HoverProviderCapability, InitializeParams, InitializeResult,
    InitializedParams, MarkupContent, MarkupKind, OneOf, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};
```

Update `server_capabilities` to advertise hover support — replace:

```rust
pub fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        ..ServerCapabilities::default()
    }
}
```

with:

```rust
pub fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        ..ServerCapabilities::default()
    }
}
```

Add the pure response-building function (place it right after `server_capabilities`):

```rust
/// Builds a hover response for `position`, or `None` if nothing resolves
/// there (e.g. the cursor is over a literal, keyword, or punctuation).
fn hover_response(verified: &hulk_semantic::VerifiedProgram, position: Position) -> Option<Hover> {
    let hit = crate::resolve::resolve_at(&verified.typed_program, position)?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: format!("```hulk\n{}: {}\n```", hit.name, hit.ty),
        }),
        range: None,
    })
}
```

Add the trait method inside the existing `impl LanguageServer for Backend` block, after `did_close`:

```rust
    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let response = self
            .documents
            .read()
            .unwrap()
            .get(&uri)
            .and_then(|state| state.last_good.as_ref())
            .and_then(|verified| hover_response(verified, position));
        Ok(response)
    }
```

Note: this needs `Position` in scope for the `hover_response` signature and the tests — it's already reachable via the `tower_lsp::lsp_types::*`-style imports used elsewhere in this file; if the compiler reports it's missing, add `Position` to the same `use tower_lsp::lsp_types::{...}` block above.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hulk-lsp`
Expected: PASS — all previous tests plus the two new hover tests.

Run: `cargo build -p hulk-lsp`
Expected: builds cleanly.

- [ ] **Step 5: Commit**

```bash
git add crates/hulk-lsp/src/backend.rs
git commit -m "feat(lsp): add hover support"
```

---

### Task 4: Go-to-definition

**Files:**
- Modify: `crates/hulk-lsp/src/backend.rs`

**Interfaces:**
- Consumes: `resolve::resolve_at` / `resolve::Hit` (Task 2), `hulk_semantic::TypeRegistry::{lookup_method, lookup_type, parent_of}`, `crate::diagnostics::span_to_range` (made crate-visible in Task 1).
- Produces: `textDocument/definition` support, advertised via `server_capabilities`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/hulk-lsp/src/backend.rs`'s `mod tests` (after the hover tests):

```rust
    #[test]
    fn definition_response_finds_a_let_binding() {
        let verified = test_analyze("let x = 5 in\nx + 1;");
        let uri = Url::parse("file:///t.hulk").unwrap();
        let response = definition_response(&verified, &uri, Position { line: 1, character: 0 })
            .expect("response");
        match response {
            GotoDefinitionResponse::Scalar(location) => {
                assert_eq!(location.range.start, Position { line: 0, character: 4 });
            }
            other => panic!("expected scalar response, got {other:?}"),
        }
    }

    #[test]
    fn definition_response_finds_a_method_via_the_type_registry() {
        let verified = test_analyze("type A {\n    b(): Number => 1;\n}\nnew A().b();");
        let uri = Url::parse("file:///t.hulk").unwrap();
        let response = definition_response(&verified, &uri, Position { line: 3, character: 8 })
            .expect("response");
        match response {
            GotoDefinitionResponse::Scalar(location) => {
                assert_eq!(location.range.start.line, 1);
            }
            other => panic!("expected scalar response, got {other:?}"),
        }
    }

    #[test]
    fn definition_response_is_none_over_a_literal() {
        let verified = test_analyze("print(1);");
        let uri = Url::parse("file:///t.hulk").unwrap();
        assert!(
            definition_response(&verified, &uri, Position { line: 0, character: 6 }).is_none()
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p hulk-lsp definition_response`
Expected: compile error — `definition_response` is not defined.

- [ ] **Step 3: Implement `definition_response` and wire it into the trait**

Extend the same `use tower_lsp::lsp_types::{...}` block from Task 3 to also bring in `GotoDefinitionParams, GotoDefinitionResponse, Location`:

```rust
use tower_lsp::lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents, HoverParams,
    HoverProviderCapability, InitializeParams, InitializeResult, InitializedParams, Location,
    MarkupContent, MarkupKind, OneOf, Position, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind, Url,
};
```

Also add this import for the registry type:

```rust
use hulk_semantic::TypeRegistry;
```

Update `server_capabilities` again — replace:

```rust
pub fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        ..ServerCapabilities::default()
    }
}
```

with:

```rust
pub fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        ..ServerCapabilities::default()
    }
}
```

Add the pure response-building functions (after `hover_response`):

```rust
/// Builds a go-to-definition response for `position`, or `None` if
/// nothing resolves there, or what resolves there has no known
/// definition location (see this plan's "Known limitations").
fn definition_response(
    verified: &hulk_semantic::VerifiedProgram,
    uri: &Url,
    position: Position,
) -> Option<GotoDefinitionResponse> {
    let hit = crate::resolve::resolve_at(&verified.typed_program, position)?;
    let span = definition_span_for(&hit, &verified.registry)?;
    Some(GotoDefinitionResponse::Scalar(Location {
        uri: uri.clone(),
        range: crate::diagnostics::span_to_range(span.line, span.col),
    }))
}

/// Resolves a `Hit`'s definition location. For a variable/`self`/parameter
/// reference this is already known (`resolve_at` found it via scope). For
/// a `.member` reference, `resolve_at` deliberately leaves it unresolved —
/// that needs the type registry: look the member up as a method first
/// (methods have a pre-flattened, inheritance-aware table via
/// `lookup_method`), then as an attribute, walking up the inheritance
/// chain by hand since `TypeInfo.attributes` only holds a type's *own*
/// attributes, not inherited ones.
fn definition_span_for(
    hit: &crate::resolve::Hit,
    registry: &TypeRegistry,
) -> Option<hulk_ast::SourceSpan> {
    if hit.definition.is_some() {
        return hit.definition;
    }

    let receiver_type = hit.receiver_type.as_ref()?;
    if let Some(method) = registry.lookup_method(receiver_type, &hit.name) {
        return Some(method.span);
    }

    let mut current = match receiver_type {
        hulk_semantic::Type::Named(name) => Some(name.clone()),
        _ => None,
    };
    while let Some(type_name) = current {
        if let Some(info) = registry.lookup_type(&type_name) {
            if let Some(attr) = info.attributes.get(&hit.name) {
                return Some(attr.span);
            }
        }
        current = registry.parent_of(&type_name);
    }
    None
}
```

Add the trait method inside `impl LanguageServer for Backend`, after `hover`:

```rust
    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let response = self
            .documents
            .read()
            .unwrap()
            .get(&uri)
            .and_then(|state| state.last_good.as_ref())
            .and_then(|verified| definition_response(verified, &uri, position));
        Ok(response)
    }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hulk-lsp`
Expected: PASS — all previous tests plus the three new go-to-definition tests. If `definition_response_finds_a_method_via_the_type_registry` fails because `MethodSignature.span` doesn't point at the line you expected, read the actual value from the failure and fix the assertion (or, if the span is clearly wrong — e.g. `(0, 0)` for a user-defined method — stop and report it, since that would mean `hulk-semantic`'s own method-span tracking has a bug worth fixing separately, not something to paper over here).

- [ ] **Step 5: Commit**

```bash
git add crates/hulk-lsp/src/backend.rs
git commit -m "feat(lsp): add go-to-definition for variables and type members"
```

---

### Task 5: End-to-end smoke test and full verification

**Files:** none (verification only).

- [ ] **Step 1: Manual protocol smoke test**

Run this PowerShell script from the repo root — it extends Plan 2's smoke test with a `hover` and a `definition` request against a small valid program:

```powershell
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = "target\debug\hulk-lsp.exe"
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.UseShellExecute = $false
$proc = [System.Diagnostics.Process]::Start($psi)

function Send-LspMessage($body) {
    $bytes = [System.Text.Encoding]::UTF8.GetByteCount($body)
    $proc.StandardInput.Write("Content-Length: $bytes`r`n`r`n")
    $proc.StandardInput.Write($body)
    $proc.StandardInput.Flush()
}

$source = "let x = 5 in\nx + 1;"
$openParams = '{"textDocument":{"uri":"file:///test.hulk","languageId":"hulk","version":1,"text":"' + $source + '"}}'

Send-LspMessage '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}'
Send-LspMessage '{"jsonrpc":"2.0","method":"initialized","params":{}}'
Send-LspMessage ('{"jsonrpc":"2.0","method":"textDocument/didOpen","params":' + $openParams + '}')
Send-LspMessage '{"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{"textDocument":{"uri":"file:///test.hulk"},"position":{"line":1,"character":0}}}'
Send-LspMessage '{"jsonrpc":"2.0","id":3,"method":"textDocument/definition","params":{"textDocument":{"uri":"file:///test.hulk"},"position":{"line":1,"character":0}}}'

Start-Sleep -Milliseconds 500
$proc.StandardInput.Close()
$output = $proc.StandardOutput.ReadToEnd()
$proc.WaitForExit(2000) | Out-Null

if ($output -match "x: Number" -and $output -match '"line":0' -and $output -match '"character":4') {
    Write-Output "SMOKE TEST PASSED"
} else {
    Write-Output "SMOKE TEST FAILED"
    Write-Output $output
}
```

Expected: prints `SMOKE TEST PASSED` — the hover response contains `x: Number`, and the definition response's range starts at line 0, character 4 (the `x` in `let x = 5`).

- [ ] **Step 2: Full workspace verification**

Run: `cargo test -p hulk-ast -p hulk-lexer -p hulk-parser -p hulk-transpile -p hulk-semantic -p hulk-lsp`
Expected: everything green — no regressions in any crate this plan didn't mean to touch.

Grep for leftover placeholders:

Run: `grep -rn "todo!" crates/hulk-lsp/src/`
Expected: no output.

---

## Post-plan check

- [ ] Confirm `server_capabilities()` now advertises `text_document_sync`, `hover_provider`, and `definition_provider` — read the function back and check all three fields are set.
- [ ] Confirm the plan's stated "Known limitations" (vector-comprehension variables, match-case pattern bindings) are the *only* gaps — re-read `resolve.rs`'s `walk_expr` match arms once more and confirm every other `ExprKind` variant that can introduce a name (`Let`, `For`, `Lambda`, function/method params, `self`) does call `scopes.declare`.
