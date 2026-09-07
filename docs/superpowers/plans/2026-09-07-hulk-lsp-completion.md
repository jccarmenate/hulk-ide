# hulk-lsp Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `textDocument/completion` to `hulk-lsp`: after `object.`, suggest that receiver's methods and attributes (walking inheritance); otherwise, suggest every variable/`self` name found in the last-good program plus global functions and types from the `TypeRegistry`.

**Architecture:** Completion is fundamentally less precise than hover/go-to-definition (Plan 4), and this plan is explicit about that rather than pretending otherwise: the moment a user asks for completion (typically right after typing `.`), that line has almost always just stopped parsing cleanly — there is no fresh position to resolve against, only the last-good tree from *before* this edit. So instead of `resolve_at`'s exact-position matching, a new `completion.rs` module works by **name**: it reads the identifier immediately before the cursor's `.` directly from the buffer text (a small string scan, no parsing involved), then looks for *any* occurrence of that name anywhere in the last-good typed tree to recover its type. Two new full-tree traversals in `resolve.rs` support this (`find_named_type`, `all_declared_names`); both are deliberately not scope-accurate, and say so in their doc comments, matching this plan's stated trade-off.

**Tech Stack:** Rust, existing `hulk-lsp` crate and dependencies. No new dependencies.

**Spec:** [docs/superpowers/specs/2026-09-01-hulk-ide-design.md](../specs/2026-09-01-hulk-ide-design.md) — "Hover / go-to-definition / completion" section, "Completion" bullet. This is the last `hulk-lsp` feature from that section; the VS Code extension (Plan 6) is next and separate.

## Global Constraints

- Completion never blocks on, or requires, the *current* line to parse. It only ever reads the buffer text (for finding the identifier before a `.`) and the cached last-good `VerifiedProgram` (for everything else).
- Every function in `completion.rs` is a plain, synchronous, `Client`-free function taking `&VerifiedProgram` / `&str` / `Position` and returning data — same testing style as `diagnostics.rs` and `resolve.rs`.
- Documented, accepted imprecision (not a bug to fix later, just how this feature works): `find_named_type` and `all_declared_names` search the *whole* program by name, not by scope. In a file with two unrelated variables that happen to share a name in different functions, completion can surface information from the wrong one. Hover and go-to-definition (Plan 4) are unaffected by this — they still resolve by exact position.

---

### Task 1: Name-based lookups in `resolve.rs`

**Files:**
- Modify: `crates/hulk-lsp/src/resolve.rs`

**Interfaces:**
- Produces: `pub fn find_named_type(program: &TypedProgram, name: &str) -> Option<Type>` (first occurrence of `name` as a `Variable`/`self` reference anywhere in the program, by simple top-down traversal order — declarations in order, then the entry expression) and `pub fn all_declared_names(program: &TypedProgram) -> Vec<String>` (every name introduced by a `let`, `for`, function/method parameter, `self`, or lambda parameter, anywhere in the program, in no particular order and with duplicates possible).

- [ ] **Step 1: Write the failing tests**

Add to `crates/hulk-lsp/src/resolve.rs`'s `mod tests` (after `returns_none_over_a_literal`):

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p hulk-lsp find_named_type`
Expected: compile error — `find_named_type` is not defined.

- [ ] **Step 3: Implement both functions**

Add this to `crates/hulk-lsp/src/resolve.rs`, after the existing `walk_expr` function and before `#[cfg(test)]`:

```rust
/// Finds the type of the first `Variable`/`self` reference named `name`
/// encountered while walking `program` top-down (declarations in order,
/// then the entry expression). Used by completion, where the expression
/// right before a `.` may not itself have parsed successfully yet (the
/// user is mid-edit) — so unlike `resolve_at`, this doesn't match an
/// exact position, and deliberately searches the *whole* program instead
/// of tracking scope. This can pick an unrelated same-named variable from
/// a different scope; see this plan's "Global Constraints" for why that
/// trade-off is accepted here.
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
/// — see this plan's "Global Constraints" for why that's an accepted
/// trade-off for completion specifically.
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
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hulk-lsp resolve::`
Expected: PASS — all previous `resolve::` tests plus the four new ones.

- [ ] **Step 5: Commit**

```bash
git add crates/hulk-lsp/src/resolve.rs
git commit -m "feat(lsp): add name-based lookups for completion"
```

---

### Task 2: `completion.rs`

**Files:**
- Create: `crates/hulk-lsp/src/completion.rs`
- Modify: `crates/hulk-lsp/src/main.rs` (add `mod completion;`)

**Interfaces:**
- Consumes: `resolve::find_named_type`, `resolve::all_declared_names` (Task 1); `hulk_semantic::TypeRegistry::{method_table_for, lookup_type, parent_of}`.
- Produces: `pub fn completion_items(verified: &VerifiedProgram, text: &str, position: Position) -> Vec<CompletionItem>`.

- [ ] **Step 1: Write the failing tests**

Create `crates/hulk-lsp/src/completion.rs`:

```rust
//! Builds completion suggestions for a cursor position.
//!
//! Two modes: right after `object.`, suggest that receiver's members
//! (methods and attributes, walking the inheritance chain); otherwise,
//! suggest every variable/`self` name found anywhere in the last-good
//! program, plus every global function and type name from the type
//! registry.
//!
//! This is deliberately more approximate than hover/go-to-definition (see
//! `resolve.rs`'s module doc comment and this project's Plan 5): the
//! buffer at the moment completion is requested (typically right after
//! typing `.`) has almost always just stopped parsing cleanly, so there
//! is no fresh, exact position to resolve against — only the last-good
//! tree from before this edit. Suggestions are found by name, not by
//! scope-accurate position.

use std::collections::HashSet;

use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind, Position};

use hulk_semantic::{Type, VerifiedProgram};

pub fn completion_items(
    verified: &VerifiedProgram,
    text: &str,
    position: Position,
) -> Vec<CompletionItem> {
    todo!()
}

fn receiver_before_dot(text: &str, position: Position) -> Option<String> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_analyze(source: &str) -> VerifiedProgram {
        let tokens = hulk_lexer::Lexer::new(source).tokenize().expect("valid tokens");
        let mut program = hulk_parser::parse(tokens).expect("valid parse");
        hulk_transpile::expand_program(&mut program);
        hulk_semantic::analyze(&program).expect("valid program")
    }

    #[test]
    fn receiver_before_dot_finds_the_identifier_right_before_the_cursor() {
        assert_eq!(
            receiver_before_dot("obj.", Position { line: 0, character: 4 }),
            Some("obj".to_string())
        );
        assert_eq!(
            receiver_before_dot("let x = obj.", Position { line: 0, character: 12 }),
            Some("obj".to_string())
        );
    }

    #[test]
    fn receiver_before_dot_is_none_without_a_trailing_dot() {
        assert_eq!(receiver_before_dot("obj", Position { line: 0, character: 3 }), None);
    }

    #[test]
    fn completion_after_dot_suggests_methods_and_attributes() {
        let source = "type A {\n    value: Number = 1;\n    f(): Number => 1;\n}\nlet a = new A() in\na;";
        let verified = test_analyze(source);
        let items = completion_items(&verified, "a.", Position { line: 0, character: 2 });
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"f"), "expected method `f` in {labels:?}");
        assert!(labels.contains(&"value"), "expected attribute `value` in {labels:?}");
    }

    #[test]
    fn completion_without_a_dot_suggests_bound_names_and_globals() {
        let verified = test_analyze("let x = 5 in\nx + 1;");
        let items = completion_items(&verified, "x", Position { line: 1, character: 0 });
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"x"));
        assert!(labels.contains(&"print"));
    }
}
```

- [ ] **Step 2: Register the module and run the tests to verify they fail**

Add `mod completion;` to `crates/hulk-lsp/src/main.rs`, after `mod backend;`:

```rust
mod backend;
mod completion;
mod diagnostics;
mod resolve;
```

Run: `cargo test -p hulk-lsp completion::`
Expected: compiles, then panics with `not yet implemented` (from `todo!()`).

- [ ] **Step 3: Implement `receiver_before_dot`, `completion_items`, and their helpers**

Replace the two `todo!()` bodies and add the two private helper functions (everything below goes between the imports and `#[cfg(test)]`):

```rust
pub fn completion_items(
    verified: &VerifiedProgram,
    text: &str,
    position: Position,
) -> Vec<CompletionItem> {
    match receiver_before_dot(text, position) {
        Some(receiver) => member_completions(verified, &receiver),
        None => general_completions(verified),
    }
}

/// If the cursor immediately follows `<ident>.`, returns `<ident>`.
fn receiver_before_dot(text: &str, position: Position) -> Option<String> {
    let line = text.lines().nth(position.line as usize)?;
    let col = (position.character as usize).min(line.len());
    let before_cursor = &line[..col];
    let before_dot = before_cursor.strip_suffix('.')?;
    let ident_start = before_dot
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(|i| i + 1)
        .unwrap_or(0);
    let ident = &before_dot[ident_start..];
    let first = ident.chars().next()?;
    if !(first.is_alphabetic() || first == '_') {
        return None;
    }
    Some(ident.to_string())
}

fn member_completions(verified: &VerifiedProgram, receiver: &str) -> Vec<CompletionItem> {
    let Some(ty) = crate::resolve::find_named_type(&verified.typed_program, receiver) else {
        return Vec::new();
    };

    let mut items = Vec::new();
    if let Some(methods) = verified.registry.method_table_for(&ty) {
        for (name, sig) in methods {
            let params: Vec<String> =
                sig.params.iter().map(|(n, t)| format!("{n}: {t}")).collect();
            items.push(CompletionItem {
                label: name,
                kind: Some(CompletionItemKind::METHOD),
                detail: Some(format!("({}) -> {}", params.join(", "), sig.return_type)),
                ..Default::default()
            });
        }
    }

    if let Type::Named(type_name) = &ty {
        let mut current = Some(type_name.clone());
        let mut seen = HashSet::new();
        while let Some(name) = current {
            if let Some(info) = verified.registry.lookup_type(&name) {
                for (attr_name, attr) in &info.attributes {
                    if seen.insert(attr_name.clone()) {
                        items.push(CompletionItem {
                            label: attr_name.clone(),
                            kind: Some(CompletionItemKind::FIELD),
                            detail: attr.declared_type.as_ref().map(|t| t.to_string()),
                            ..Default::default()
                        });
                    }
                }
            }
            current = verified.registry.parent_of(&name);
        }
    }

    items
}

fn general_completions(verified: &VerifiedProgram) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();

    for name in crate::resolve::all_declared_names(&verified.typed_program) {
        if seen.insert(name.clone()) {
            items.push(CompletionItem {
                label: name,
                kind: Some(CompletionItemKind::VARIABLE),
                ..Default::default()
            });
        }
    }

    for (name, sig) in &verified.registry.functions {
        if seen.insert(name.clone()) {
            let params: Vec<String> =
                sig.params.iter().map(|(n, t)| format!("{n}: {t}")).collect();
            items.push(CompletionItem {
                label: name.clone(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some(format!("({}) -> {}", params.join(", "), sig.return_type)),
                ..Default::default()
            });
        }
    }

    for name in verified.registry.types.keys() {
        if seen.insert(name.clone()) {
            items.push(CompletionItem {
                label: name.clone(),
                kind: Some(CompletionItemKind::CLASS),
                ..Default::default()
            });
        }
    }

    items
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hulk-lsp completion::`
Expected: PASS — all 4 tests. If `receiver_before_dot`'s column-slicing panics on a non-ASCII line, that's a real bug to fix (slice on a char boundary using `line.char_indices()` instead of a byte index) — the test sources here are all ASCII, so this won't surface here, but keep it in mind if you extend the tests later.

- [ ] **Step 5: Commit**

```bash
git add crates/hulk-lsp/src/completion.rs crates/hulk-lsp/src/main.rs
git commit -m "feat(lsp): add completion.rs for member and general completions"
```

---

### Task 3: Wire `textDocument/completion` into the server

**Files:**
- Modify: `crates/hulk-lsp/src/backend.rs`

**Interfaces:**
- Consumes: `completion::completion_items` (Task 2).
- Produces: `textDocument/completion` support, advertised via `server_capabilities` (triggered on `.`).

- [ ] **Step 1: Extend `server_capabilities`**

In `crates/hulk-lsp/src/backend.rs`, add `CompletionOptions` to the existing `tower_lsp::lsp_types::{...}` import list, then replace:

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

with:

```rust
pub fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".to_string()]),
            ..CompletionOptions::default()
        }),
        ..ServerCapabilities::default()
    }
}
```

- [ ] **Step 2: Add a test for the capability, then the trait method**

Add to `mod tests`, after `server_capabilities_use_full_text_document_sync`:

```rust
    #[test]
    fn server_capabilities_trigger_completion_on_dot() {
        let caps = server_capabilities();
        let completion = caps.completion_provider.expect("completion provider");
        assert_eq!(completion.trigger_characters, Some(vec![".".to_string()]));
    }
```

Run: `cargo test -p hulk-lsp server_capabilities_trigger_completion_on_dot`
Expected: PASS immediately (Step 1 already implemented the capability) — this test exists to lock the behavior in, not to drive it.

Now add `CompletionParams, CompletionResponse` to the `tower_lsp::lsp_types::{...}` import list, and add the trait method inside `impl LanguageServer for Backend`, after `goto_definition`:

```rust
    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let items = {
            let docs = self.documents.read().unwrap();
            let Some(state) = docs.get(&uri) else {
                return Ok(None);
            };
            let Some(verified) = state.last_good.as_ref() else {
                return Ok(None);
            };
            crate::completion::completion_items(verified, &state.text, position)
        };
        Ok(Some(CompletionResponse::Array(items)))
    }
```

Note: `CompletionParams`'s position field is named `text_document_position` (not `text_document_position_params`, unlike `HoverParams`/`GotoDefinitionParams`) — if the compiler reports a different field name here, use whatever it actually is; this is a real inconsistency in `lsp_types` itself, not something to work around.

- [ ] **Step 3: Run the tests and build**

Run: `cargo test -p hulk-lsp`
Expected: PASS — every test so far, including the two new ones from this task.

Run: `cargo build -p hulk-lsp`
Expected: builds cleanly.

- [ ] **Step 4: Commit**

```bash
git add crates/hulk-lsp/src/backend.rs
git commit -m "feat(lsp): wire textDocument/completion"
```

---

### Task 4: End-to-end smoke test and full verification

**Files:** none (verification only).

- [ ] **Step 1: Manual protocol smoke test**

Run this PowerShell script from the repo root:

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

$src = "type A {\n    f(): Number => 1;\n}\nlet a = new A() in\na."
$openParams = '{"textDocument":{"uri":"file:///t.hulk","languageId":"hulk","version":1,"text":"' + $src + '"}}'

Send-LspMessage '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}'
Send-LspMessage '{"jsonrpc":"2.0","method":"initialized","params":{}}'
Send-LspMessage ('{"jsonrpc":"2.0","method":"textDocument/didOpen","params":' + $openParams + '}')
Send-LspMessage '{"jsonrpc":"2.0","id":2,"method":"textDocument/completion","params":{"textDocument":{"uri":"file:///t.hulk"},"position":{"line":4,"character":2}}}'

Start-Sleep -Milliseconds 500
$proc.StandardInput.Close()
$output = $proc.StandardOutput.ReadToEnd()
$proc.WaitForExit(2000) | Out-Null

if ($output -match '"label":"f"') {
    Write-Output "SMOKE TEST PASSED"
} else {
    Write-Output "SMOKE TEST FAILED"
    Write-Output $output
}
```

Expected: prints `SMOKE TEST PASSED`. Note the source has a syntax error at the very end (`a.` with nothing after the dot) — this is deliberate: it's the realistic mid-typing state completion has to work under, and the point of this test is confirming completion still works even though `didOpen`'s own diagnostics pass just failed to parse this exact line.

- [ ] **Step 2: Full workspace verification**

Run: `cargo test -p hulk-ast -p hulk-lexer -p hulk-parser -p hulk-transpile -p hulk-semantic -p hulk-lsp`
Expected: everything green.

Run: `grep -rn "todo!" crates/hulk-lsp/src/`
Expected: no output.

---

## Post-plan check

- [ ] Confirm `server_capabilities()` now advertises all four: `text_document_sync`, `hover_provider`, `definition_provider`, `completion_provider`.
- [ ] Re-read `completion.rs`'s module doc comment and confirm it still accurately describes the approximation this plan settled on (name-based, not scope-based) — this is the one piece of `hulk-lsp` where "it usually works, and here's exactly when it might not" is the honest, intended behavior, not a bug.
