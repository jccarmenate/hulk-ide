# Precise Binding Spans Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `Param`, `LetBinding`, `MemberExpr`, and `ForExpr` a real source-position field for the identifier they introduce or reference (a parameter's name, a `let` binding's name, a `.member` access's member name, a `for` loop's variable name) — today none of these carry any span at all for that specific name (parameters use a hardcoded `(0,0)`, `let`/`for` reuse an unrelated span, and `.member` has no span whatsoever). This is a prerequisite for Plan 4 (hover/go-to-definition/completion in `hulk-lsp`), which needs accurate positions to work correctly instead of misleadingly.

**Architecture:** Each of the four `hulk-ast` struct definitions gets one new `SourceSpan` field, and every construction site across `hulk-parser` (where the real position is captured from the token stream), `hulk-semantic` (which threads the field through when building the typed tree, and — as a direct, welcome side effect — now declares parameters and `for` variables in its `Environment` at their real position instead of `(0,0)`/the iterable's span), and `hulk-transpile` (which threads the field through when expanding macros and substituting arguments) is updated to supply it. Two of `hulk-codegen`'s own test-only helper functions also construct these types directly and need the same one-field addition, using that file's existing `dummy_span()` convention.

**Tech Stack:** Rust. No new dependencies. Touches `hulk-ast`, `hulk-parser`, `hulk-semantic`, `hulk-transpile`, `hulk-codegen` (test code only).

**Spec:** [docs/superpowers/specs/2026-09-01-hulk-ide-design.md](../specs/2026-09-01-hulk-ide-design.md) — this plan implements a prerequisite the spec's "Hover / go-to-definition / completion" section assumed was available but wasn't; see that section for why precise positions matter for Plan 4.

## Global Constraints

- Every new field is named `<thing>_span: SourceSpan` (`name_span` for `Param`/`LetBinding`, `member_span` for `MemberExpr`, `var_span` for `ForExpr`) and is always the **last** parameter of that struct's `::new()` constructor, for a consistent, memorable convention across all four.
- `hulk-codegen`'s production (non-test) code does not construct any of these four types — confirmed by inspection — so this plan's changes to that crate are confined to two `#[cfg(test)]` helper functions in `crates/hulk-codegen/src/lower/mod.rs`. That crate cannot be built or tested in this environment (no LLVM 17 installed) either before or after this plan — this is a pre-existing gap, not a regression introduced here. The two edits are made carefully by hand, following the file's own `dummy_span()` convention exactly, and must be verified by the user (or in CI) on a machine with LLVM 17 before being trusted.
- Every existing test in every touched crate must keep passing. Where a test's expected span value changes because a genuinely more accurate span is now available (e.g. a semantic error position shifting from an initializer's span to the binding name's own span), update the expected value in that test rather than avoiding the improvement — but only after confirming via a real `cargo test` failure that this is what's actually happening, not by guessing ahead of time.

---

### Task 1: `Param.name_span`

**Files:**
- Modify: `crates/hulk-ast/src/lib.rs` (`Param` struct + `Param::new`)
- Modify: `crates/hulk-parser/src/lib.rs:481` (`parse_param_list_after_lparen`) + add a test
- Modify: `crates/hulk-semantic/src/passes/infer.rs:183` (`infer_function`, parameter declaration)

**Interfaces:**
- Produces: `Param::new(name: impl Into<String>, type_annotation: Option<TypeRef>, name_span: SourceSpan) -> Param`, and a new public field `Param.name_span: SourceSpan`.

- [ ] **Step 1: Write the failing test**

Add to `crates/hulk-parser/src/lib.rs`'s `mod tests` (after `parses_function_declaration_and_entry_expression`, around line 1995):

```rust
    #[test]
    fn param_name_span_points_at_the_parameter_name() {
        let program = parse_source("function f(x: Number): Number => x; f(1);");
        match &program.declarations[0].kind {
            DeclarationKind::Function(function) => {
                assert_eq!(function.params[0].name_span, SourceSpan::new(1, 12));
            }
            other => panic!("expected function declaration, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p hulk-parser param_name_span`
Expected: compile error — `Param` has no field `name_span`.

- [ ] **Step 3: Add the field**

In `crates/hulk-ast/src/lib.rs`, find:

```rust
pub struct Param {
    pub name: String,
    pub type_annotation: Option<TypeRef>,
}

impl Param {
    pub fn new(name: impl Into<String>, type_annotation: Option<TypeRef>) -> Self {
        Self {
            name: name.into(),
            type_annotation,
        }
    }
}
```

Replace with:

```rust
pub struct Param {
    pub name: String,
    pub type_annotation: Option<TypeRef>,
    /// The source location of the parameter name itself — not the whole
    /// parameter (e.g. not its type annotation).
    pub name_span: SourceSpan,
}

impl Param {
    pub fn new(
        name: impl Into<String>,
        type_annotation: Option<TypeRef>,
        name_span: SourceSpan,
    ) -> Self {
        Self {
            name: name.into(),
            type_annotation,
            name_span,
        }
    }
}
```

- [ ] **Step 4: Capture the span in the parser**

In `crates/hulk-parser/src/lib.rs`, in `parse_param_list_after_lparen` (around line 480), replace:

```rust
                let name = self.parse_name()?;
                let type_annotation = if self.match_kind(&TokenKind::Colon) {
                    Some(self.parse_type_ref()?)
                } else {
                    None
                };
                params.push(Param::new(name, type_annotation));
```

with:

```rust
                let name_span = self.peek_span();
                let name = self.parse_name()?;
                let type_annotation = if self.match_kind(&TokenKind::Colon) {
                    Some(self.parse_type_ref()?)
                } else {
                    None
                };
                params.push(Param::new(name, type_annotation, name_span));
```

- [ ] **Step 5: Use the real span in the semantic analyzer**

In `crates/hulk-semantic/src/passes/infer.rs`, in `infer_function` (around line 183), replace:

```rust
            env.declare(&p.name, ty.clone(), SourceSpan::new(0, 0));
```

with:

```rust
            env.declare(&p.name, ty.clone(), p.name_span);
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p hulk-parser -p hulk-ast -p hulk-semantic`
Expected: PASS. If a `hulk-semantic` test fails because it asserted an error's span was `(0, 0)` for a parameter-related diagnostic, read the failure, confirm the new span is the parameter's real position, and update that test's expected span — this is the intended improvement, not a regression.

- [ ] **Step 7: Commit**

```bash
git add crates/hulk-ast/src/lib.rs crates/hulk-parser/src/lib.rs crates/hulk-semantic/src/passes/infer.rs
git commit -m "feat(ast): add Param.name_span with a real position"
```

---

### Task 2: `LetBinding.name_span`

**Files:**
- Modify: `crates/hulk-ast/src/lib.rs` (`LetBinding<A>` struct + `LetBinding::new`, and the existing unit test using it)
- Modify: `crates/hulk-parser/src/lib.rs:1170` (`finish_let_expression`) + add a test
- Modify: `crates/hulk-semantic/src/passes/infer.rs` (around lines 956–967, `let` binding inference)
- Modify: `crates/hulk-transpile/src/expand.rs:347` and `crates/hulk-transpile/src/substitute.rs:143`
- Modify: `crates/hulk-codegen/src/lower/mod.rs:419` (test-only helper)

**Interfaces:**
- Produces: `LetBinding::new(name: impl Into<String>, type_annotation: Option<TypeRef>, initializer: Expr<A>, name_span: SourceSpan) -> LetBinding<A>`, and a new public field `LetBinding.name_span: SourceSpan`.

- [ ] **Step 1: Write the failing test**

Add to `crates/hulk-parser/src/lib.rs`'s `mod tests` (after `parses_let_expression_with_type_annotation`, around line 2016):

```rust
    #[test]
    fn let_binding_name_span_points_at_the_bound_name() {
        let program = parse_source("let x = 5 in x + 1;");
        match &program.entry.kind {
            ExprKind::Let(let_expr) => {
                assert_eq!(let_expr.bindings[0].name_span, SourceSpan::new(1, 5));
            }
            other => panic!("expected let entry, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p hulk-parser let_binding_name_span`
Expected: compile error — `LetBinding` has no field `name_span`.

- [ ] **Step 3: Add the field**

In `crates/hulk-ast/src/lib.rs`, find:

```rust
pub struct LetBinding<A = ()> {
    pub name: String,
    pub type_annotation: Option<TypeRef>,
    pub initializer: Expr<A>,
}

impl<A> LetBinding<A> {
    pub fn new(
        name: impl Into<String>,
        type_annotation: Option<TypeRef>,
        initializer: Expr<A>,
    ) -> Self {
```

Replace the struct and the start of `new` with:

```rust
pub struct LetBinding<A = ()> {
    pub name: String,
    pub type_annotation: Option<TypeRef>,
    pub initializer: Expr<A>,
    /// The source location of the bound name itself — not the whole
    /// binding (e.g. not its initializer expression).
    pub name_span: SourceSpan,
}

impl<A> LetBinding<A> {
    pub fn new(
        name: impl Into<String>,
        type_annotation: Option<TypeRef>,
        initializer: Expr<A>,
        name_span: SourceSpan,
    ) -> Self {
```

And its body — find (immediately following):

```rust
        Self {
            name: name.into(),
            type_annotation,
            initializer,
        }
    }
}
```

Replace with:

```rust
        Self {
            name: name.into(),
            type_annotation,
            initializer,
            name_span,
        }
    }
}
```

Then fix the crate's own unit test, in `crates/hulk-ast/src/lib.rs`'s `mod tests` (around line 1098): replace

```rust
                vec![LetBinding::new("x", None, Expr::number(5.0, s()))],
```

with:

```rust
                vec![LetBinding::new("x", None, Expr::number(5.0, s()), s())],
```

- [ ] **Step 4: Capture the span in the parser**

In `crates/hulk-parser/src/lib.rs`, in `finish_let_expression` (around line 1170), replace:

```rust
            let name = self.parse_name()?;
            let type_annotation = if self.match_kind(&TokenKind::Colon) {
                Some(self.parse_type_ref()?)
            } else {
                None
            };

            self.consume(&TokenKind::Assign, "`=` in let binding")?;
            let initializer = self.parse_expression()?;
            bindings.push(LetBinding::new(name, type_annotation, initializer));
```

with:

```rust
            let name_span = self.peek_span();
            let name = self.parse_name()?;
            let type_annotation = if self.match_kind(&TokenKind::Colon) {
                Some(self.parse_type_ref()?)
            } else {
                None
            };

            self.consume(&TokenKind::Assign, "`=` in let binding")?;
            let initializer = self.parse_expression()?;
            bindings.push(LetBinding::new(name, type_annotation, initializer, name_span));
```

- [ ] **Step 5: Thread the span through the semantic analyzer**

In `crates/hulk-semantic/src/passes/infer.rs` (around lines 956–967), replace:

```rust
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
```

with:

```rust
            env.declare(
                &binding.name,
                declared_type.clone(),
                binding.name_span,
            );

            // 2d. Store the typed binding.
            typed_bindings.push(LetBinding::new(
                &binding.name,
                binding.type_annotation.clone(),
                typed_init,
                binding.name_span,
            ));
```

- [ ] **Step 6: Thread the span through macro expansion and substitution**

In `crates/hulk-transpile/src/expand.rs` (around line 347), replace:

```rust
                l.bindings.into_iter().map(|b| hulk_ast::LetBinding::new(
                    b.name,
                    b.type_annotation,
                    ex!(b.initializer),
                )).collect(),
```

with:

```rust
                l.bindings.into_iter().map(|b| hulk_ast::LetBinding::new(
                    b.name,
                    b.type_annotation,
                    ex!(b.initializer),
                    b.name_span,
                )).collect(),
```

In `crates/hulk-transpile/src/substitute.rs` (around line 143), replace:

```rust
                LetBinding::new(
                    new_name,
                    b.type_annotation.clone(),
                    substitute(&b.initializer, subst),
                )
```

with:

```rust
                LetBinding::new(
                    new_name,
                    b.type_annotation.clone(),
                    substitute(&b.initializer, subst),
                    b.name_span,
                )
```

- [ ] **Step 7: Update the codegen test helper**

In `crates/hulk-codegen/src/lower/mod.rs` (around line 419), replace:

```rust
            .map(|(name, init)| LetBinding {
                name,
                type_annotation: None,
                initializer: *Box::new(init),
            })
```

with:

```rust
            .map(|(name, init)| LetBinding {
                name,
                type_annotation: None,
                initializer: *Box::new(init),
                name_span: dummy_span(),
            })
```

This crate cannot be built here (no LLVM 17) — this edit is made to keep the source textually correct for whenever it next builds; verify with `cargo build -p hulk-codegen` and `cargo test -p hulk-codegen` on a machine with LLVM 17.

- [ ] **Step 8: Run the tests**

Run: `cargo test -p hulk-ast -p hulk-parser -p hulk-semantic -p hulk-transpile`
Expected: PASS. As in Task 1, if a test's expected span shifts to a more accurate value, update it after confirming the failure shows the improvement.

- [ ] **Step 9: Commit**

```bash
git add crates/hulk-ast/src/lib.rs crates/hulk-parser/src/lib.rs crates/hulk-semantic/src/passes/infer.rs crates/hulk-transpile/src/expand.rs crates/hulk-transpile/src/substitute.rs crates/hulk-codegen/src/lower/mod.rs
git commit -m "feat(ast): add LetBinding.name_span with a real position"
```

---

### Task 3: `MemberExpr.member_span`

**Files:**
- Modify: `crates/hulk-ast/src/lib.rs` (`MemberExpr<A>` struct + `MemberExpr::new`)
- Modify: `crates/hulk-parser/src/lib.rs:913` (`parse_postfix`) + add a test
- Modify: `crates/hulk-semantic/src/passes/infer.rs` (around lines 1474–1505, `infer_member`)
- Modify: `crates/hulk-transpile/src/substitute.rs:237`

**Interfaces:**
- Produces: `MemberExpr::new(object: Expr<A>, member: impl Into<String>, member_span: SourceSpan) -> MemberExpr<A>`, and a new public field `MemberExpr.member_span: SourceSpan`. This is the field Plan 4 needs to resolve hover/completion/go-to-definition on `.member` accesses precisely instead of only approximately.

- [ ] **Step 1: Write the failing test**

Add to `crates/hulk-parser/src/lib.rs`'s `mod tests` (after `parses_arithmetic_precedence_inside_call`, around line 1977):

```rust
    #[test]
    fn member_span_points_at_the_member_name_not_the_receiver() {
        let program = parse_source("a.b;");
        match &program.entry.kind {
            ExprKind::Member(member) => {
                assert_eq!(member.member_span, SourceSpan::new(1, 3));
            }
            other => panic!("expected member entry, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p hulk-parser member_span_points_at`
Expected: compile error — `MemberExpr` has no field `member_span`.

- [ ] **Step 3: Add the field**

In `crates/hulk-ast/src/lib.rs`, find:

```rust
pub struct MemberExpr<A = ()> {
    pub object: Box<Expr<A>>,
    pub member: String,
}

impl<A> MemberExpr<A> {
    pub fn new(object: Expr<A>, member: impl Into<String>) -> Self {
        Self {
            object: Box::new(object),
            member: member.into(),
        }
    }
}
```

Replace with:

```rust
pub struct MemberExpr<A = ()> {
    pub object: Box<Expr<A>>,
    pub member: String,
    /// The source location of the member name itself (the identifier
    /// after the `.`) — not the receiver, and not the whole expression.
    pub member_span: SourceSpan,
}

impl<A> MemberExpr<A> {
    pub fn new(object: Expr<A>, member: impl Into<String>, member_span: SourceSpan) -> Self {
        Self {
            object: Box::new(object),
            member: member.into(),
            member_span,
        }
    }
}
```

- [ ] **Step 4: Capture the span in the parser**

In `crates/hulk-parser/src/lib.rs`, in `parse_postfix` (around line 912), replace:

```rust
            } else if self.match_kind(&TokenKind::Dot) {
                let member = self.parse_name()?;
                expr = Expr::new(ExprKind::Member(MemberExpr::new(expr, member)), span);
```

with:

```rust
            } else if self.match_kind(&TokenKind::Dot) {
                let member_span = self.peek_span();
                let member = self.parse_name()?;
                expr = Expr::new(
                    ExprKind::Member(MemberExpr::new(expr, member, member_span)),
                    span,
                );
```

- [ ] **Step 5: Thread the span through the semantic analyzer**

In `crates/hulk-semantic/src/passes/infer.rs`, in `infer_member` (around lines 1474–1505), replace:

```rust
            let typed_member = MemberExpr::new(typed_obj, &member.member);
```

with:

```rust
            let typed_member = MemberExpr::new(typed_obj, &member.member, member.member_span);
```

and replace:

```rust
            typed_expr(
                ExprKind::Member(MemberExpr {
                    object: Box::new(typed_obj),
                    member: member.member.clone(),
                }),
                Type::Error,
                member.object.span,
            )
```

with:

```rust
            typed_expr(
                ExprKind::Member(MemberExpr {
                    object: Box::new(typed_obj),
                    member: member.member.clone(),
                    member_span: member.member_span,
                }),
                Type::Error,
                member.object.span,
            )
```

- [ ] **Step 6: Thread the span through substitution**

In `crates/hulk-transpile/src/substitute.rs` (around line 237), replace:

```rust
        ExprKind::Member(m) => Expr::new(
            ExprKind::Member(MemberExpr::new(substitute(&m.object, subst), m.member.clone())),
            expr.span,
        ),
```

with:

```rust
        ExprKind::Member(m) => Expr::new(
            ExprKind::Member(MemberExpr::new(
                substitute(&m.object, subst),
                m.member.clone(),
                m.member_span,
            )),
            expr.span,
        ),
```

- [ ] **Step 7: Run the tests**

Run: `cargo test -p hulk-ast -p hulk-parser -p hulk-semantic -p hulk-transpile`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/hulk-ast/src/lib.rs crates/hulk-parser/src/lib.rs crates/hulk-semantic/src/passes/infer.rs crates/hulk-transpile/src/substitute.rs
git commit -m "feat(ast): add MemberExpr.member_span with a real position"
```

---

### Task 4: `ForExpr.var_span`

**Files:**
- Modify: `crates/hulk-ast/src/lib.rs` (`ForExpr<A>` struct + `ForExpr::new`)
- Modify: `crates/hulk-parser/src/lib.rs:1229` (`finish_for_expression`) + add a test
- Modify: `crates/hulk-semantic/src/passes/infer.rs` (around lines 1274–1280, `infer_for`)
- Modify: `crates/hulk-transpile/src/substitute.rs:182`
- Modify: `crates/hulk-codegen/src/lower/mod.rs:642` (test-only helper)

**Interfaces:**
- Produces: `ForExpr::new(var: impl Into<String>, iterable: Expr<A>, body: Expr<A>, var_span: SourceSpan) -> ForExpr<A>`, and a new public field `ForExpr.var_span: SourceSpan`.

- [ ] **Step 1: Write the failing test**

Add to `crates/hulk-parser/src/lib.rs`'s `mod tests` (after `parses_control_flow_expressions`, around line 2056):

```rust
    #[test]
    fn for_var_span_points_at_the_loop_variable() {
        let program = parse_source("for (x in range(1, 10)) print(x);");
        match &program.entry.kind {
            ExprKind::For(for_expr) => {
                assert_eq!(for_expr.var_span, SourceSpan::new(1, 6));
            }
            other => panic!("expected for entry, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p hulk-parser for_var_span_points_at`
Expected: compile error — `ForExpr` has no field `var_span`.

- [ ] **Step 3: Add the field**

In `crates/hulk-ast/src/lib.rs`, find:

```rust
pub struct ForExpr<A = ()> {
    pub var: String,
    pub iterable: Box<Expr<A>>,
    pub body: Box<Expr<A>>,
}

impl<A> ForExpr<A> {
    pub fn new(var: impl Into<String>, iterable: Expr<A>, body: Expr<A>) -> Self {
        Self {
            var: var.into(),
            iterable: Box::new(iterable),
            body: Box::new(body),
        }
```

Replace with:

```rust
pub struct ForExpr<A = ()> {
    pub var: String,
    pub iterable: Box<Expr<A>>,
    pub body: Box<Expr<A>>,
    /// The source location of the loop variable name itself — not the
    /// whole `for` expression.
    pub var_span: SourceSpan,
}

impl<A> ForExpr<A> {
    pub fn new(var: impl Into<String>, iterable: Expr<A>, body: Expr<A>, var_span: SourceSpan) -> Self {
        Self {
            var: var.into(),
            iterable: Box::new(iterable),
            body: Box::new(body),
            var_span,
        }
```

- [ ] **Step 4: Capture the span in the parser**

In `crates/hulk-parser/src/lib.rs`, in `finish_for_expression` (around line 1227), replace:

```rust
    fn finish_for_expression(&mut self, span: SourceSpan) -> Result<Expr, ParseError> {
        self.consume(&TokenKind::LParen, "`(` before for binding")?;
        let var = self.parse_name()?;
        self.consume(&TokenKind::In, "`in` inside for binding")?;
        let iterable = self.parse_expression()?;
        self.consume(&TokenKind::RParen, "`)` after for binding")?;
        let body = self.parse_expression()?;

        Ok(Expr::new(
            ExprKind::For(ForExpr::new(var, iterable, body)),
            span,
        ))
    }
```

with:

```rust
    fn finish_for_expression(&mut self, span: SourceSpan) -> Result<Expr, ParseError> {
        self.consume(&TokenKind::LParen, "`(` before for binding")?;
        let var_span = self.peek_span();
        let var = self.parse_name()?;
        self.consume(&TokenKind::In, "`in` inside for binding")?;
        let iterable = self.parse_expression()?;
        self.consume(&TokenKind::RParen, "`)` after for binding")?;
        let body = self.parse_expression()?;

        Ok(Expr::new(
            ExprKind::For(ForExpr::new(var, iterable, body, var_span)),
            span,
        ))
    }
```

- [ ] **Step 5: Thread the span through the semantic analyzer**

In `crates/hulk-semantic/src/passes/infer.rs`, in `infer_for` (around lines 1274–1280), replace:

```rust
        env.push_scope();
        env.declare(&for_expr.var, element_type.clone(), for_expr.iterable.span);
        let body = self.infer_expr(&for_expr.body, env);
        env.pop_scope();

        let result_type = body.anno.clone();
        let for_typed = ForExpr::new(&for_expr.var, iterable, body);
```

with:

```rust
        env.push_scope();
        env.declare(&for_expr.var, element_type.clone(), for_expr.var_span);
        let body = self.infer_expr(&for_expr.body, env);
        env.pop_scope();

        let result_type = body.anno.clone();
        let for_typed = ForExpr::new(&for_expr.var, iterable, body, for_expr.var_span);
```

- [ ] **Step 6: Thread the span through substitution**

In `crates/hulk-transpile/src/substitute.rs` (around line 181), replace:

```rust
        ExprKind::For(f) => Expr::new(
            ExprKind::For(ForExpr::new(
                f.var.clone(),
                substitute(&f.iterable, subst),
                substitute(&f.body, subst),
            )),
            expr.span,
        ),
```

with:

```rust
        ExprKind::For(f) => Expr::new(
            ExprKind::For(ForExpr::new(
                f.var.clone(),
                substitute(&f.iterable, subst),
                substitute(&f.body, subst),
                f.var_span,
            )),
            expr.span,
        ),
```

- [ ] **Step 7: Update the codegen test helper**

In `crates/hulk-codegen/src/lower/mod.rs` (around line 640), replace:

```rust
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
```

with:

```rust
    fn for_expr(var: &str, iterable: Expr<Type>, body: Expr<Type>, ty: Type) -> Expr<Type> {
        Expr {
            kind: ExprKind::For(ForExpr {
                var: var.to_string(),
                iterable: Box::new(iterable),
                body: Box::new(body),
                var_span: dummy_span(),
            }),
            anno: ty,
            span: dummy_span(),
        }
    }
```

As in Task 2 Step 7, this can't be built or tested here — verify on a machine with LLVM 17.

- [ ] **Step 8: Run the tests**

Run: `cargo test -p hulk-ast -p hulk-parser -p hulk-semantic -p hulk-transpile`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/hulk-ast/src/lib.rs crates/hulk-parser/src/lib.rs crates/hulk-semantic/src/passes/infer.rs crates/hulk-transpile/src/substitute.rs crates/hulk-codegen/src/lower/mod.rs
git commit -m "feat(ast): add ForExpr.var_span with a real position"
```

---

## Post-plan check

- [ ] Run `cargo test -p hulk-ast -p hulk-lexer -p hulk-parser -p hulk-transpile -p hulk-semantic -p hulk-lsp` (everything buildable in this environment) and confirm all green.
- [ ] Run `cargo build -p hulk-lsp` and confirm it still builds — `hulk-lsp`'s `diagnostics.rs` calls `hulk_transpile::expand_program` and `hulk_semantic::analyze` directly, so it must keep compiling against the updated signatures even though Plan 4 (which will actually use the new spans) hasn't started yet.
- [ ] Grep the four changed files' diffs one more time for any remaining `SourceSpan::new(0, 0)` or similarly-dummy span tied to a parameter/binding/member/loop-variable that this plan was supposed to fix, to make sure nothing was missed.
