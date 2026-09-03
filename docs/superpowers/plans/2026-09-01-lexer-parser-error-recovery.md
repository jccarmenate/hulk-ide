# Lexer/Parser Error Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add multi-error-reporting entry points to `hulk-lexer` and `hulk-parser` (`tokenize_recovering` and `parse_recovering`) so a downstream tool (the future `hulk-lsp` crate) can surface every lexical error and every top-level-declaration parse error in one pass, without changing the existing single-error `tokenize()`/`parse()` behavior that `hulk-cli` depends on.

**Architecture:** Both crates keep their existing single-error entry points untouched from the outside. Internally, the lexer's scan loop is refactored into one shared implementation that always continues past an error (recording it) instead of returning early; `tokenize()` becomes a thin wrapper that takes the first recorded error, if any, reproducing the old behavior exactly. The parser gets a new sibling method, `parse_program_recovering`, that parses top-level declarations in a loop, and on a failed declaration records the error and skips tokens until the next declaration-start or expression-start token before continuing; the existing `parse_program` is left as-is.

**Tech Stack:** Rust, existing `hulk-lexer` and `hulk-parser` crates (no new dependencies).

**Spec:** [docs/superpowers/specs/2026-09-01-hulk-ide-design.md](../specs/2026-09-01-hulk-ide-design.md) — see "Lexer/parser error recovery" section.

## Global Constraints

- `hulk_lexer::Lexer::tokenize()` and `hulk_parser::parse()` / `Ll1Parser::parse_program()` must keep their exact current public signatures and behavior — every existing test in both crates must keep passing unmodified.
- New entry points are additive only: `Lexer::tokenize_recovering`, `hulk_parser::parse_recovering`, `Ll1Parser::parse_program_recovering`.
- No new crate dependencies.
- Parser recovery granularity is top-level declarations only (`function`, `def`, `type`, `protocol` — i.e. whatever `lookahead_starts_declaration()` already recognizes). Do not attempt recovery inside a declaration's body.

---

### Task 1: Lexer — `tokenize_recovering`

**Files:**
- Modify: `crates/hulk-lexer/src/lib.rs`

**Interfaces:**
- Produces: `pub fn tokenize_recovering(&mut self) -> (Vec<Token>, Vec<LexError>)` on `Lexer` — best-effort token stream plus every `LexError` encountered, in source order. `pub fn tokenize(&mut self) -> Result<Vec<Token>, LexError>` keeps its existing signature and behavior (returns `Err` with the *first* error that would have been recorded, `Ok(tokens)` otherwise).

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block at the bottom of `crates/hulk-lexer/src/lib.rs` (after the existing `test_full_expression` test):

```rust
    #[test]
    fn test_tokenize_recovering_reports_multiple_lexical_errors() {
        let (tokens, errors) = Lexer::new("# + ?").tokenize_recovering();
        assert_eq!(errors.len(), 2);
        assert!(matches!(errors[0], LexError::UnexpectedChar { ch: '#', .. }));
        assert!(matches!(errors[1], LexError::UnexpectedChar { ch: '?', .. }));
        assert!(tokens.iter().any(|t| matches!(t.kind, TokenKind::Plus)));
    }

    #[test]
    fn test_tokenize_recovering_continues_after_invalid_escape_in_string() {
        let (tokens, errors) = Lexer::new(r#""a\qb" + 1"#).tokenize_recovering();
        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0], LexError::InvalidEscape { ch: 'q', .. }));
        assert!(matches!(&tokens[0].kind, TokenKind::StringLit(s) if s == "ab"));
        assert!(tokens.iter().any(|t| matches!(t.kind, TokenKind::Plus)));
    }

    #[test]
    fn test_tokenize_invalid_escape_still_returns_single_error() {
        let err = lex_err(r#""a\qb" + 1"#);
        assert!(matches!(err, LexError::InvalidEscape { ch: 'q', .. }));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p hulk-lexer`
Expected: compile error — `tokenize_recovering` does not exist on `Lexer`.

- [ ] **Step 3: Refactor the scan loop to support recovery**

In `crates/hulk-lexer/src/lib.rs`, replace the body of `tokenize` (lines 287–414) with the new recovering implementation, and turn `tokenize` into a thin wrapper. Replace:

```rust
    pub fn tokenize(&mut self) -> Result<Vec<Token>, LexError> {
        let mut tokens = Vec::new();
        while !self.is_at_end() {
            let span: Span = self.current_span();
            let ch: char = self.advance();

            let kind = match ch {
                // ── Whitespace ────────────────────────────────────────────
                // Skip silently — whitespace carries no meaning in HULK.
                ' ' | '\t' | '\r' | '\n' => continue,

                // ── Single-character tokens ───────────────────────────────
                '+' => TokenKind::Plus,
                '-' => {
                    if self.peek() == '>' {
                        self.advance();
                        TokenKind::Arrow
                    } else {
                        TokenKind::Minus
                    }
                }
                '*' => TokenKind::Star,
                '/' => {
                    if self.peek() == '/' {
                        // This is a comment — skip everything until end of line.
                        // WHY: HULK uses // for single-line comments like most languages.
                        while self.peek() != '\n' && !self.is_at_end() {
                            self.advance();
                        }
                        continue;
                    } else {
                        TokenKind::Slash
                    }
                }
                '^' => TokenKind::Caret,
                '%' => TokenKind::Percent,
                '(' => TokenKind::LParen,
                ')' => TokenKind::RParen,
                '{' => TokenKind::LBrace,
                '}' => TokenKind::RBrace,
                '[' => TokenKind::LBracket,
                ']' => TokenKind::RBracket,
                ';' => TokenKind::Semicolon,
                ',' => TokenKind::Comma,
                '.' => TokenKind::Dot,
                '|' => TokenKind::Or,

                // ── One-or-two character tokens ───────────────────────────
                '=' => {
                    if self.peek() == '=' {
                        self.advance();
                        TokenKind::EqEq
                    } else if self.peek() == '>' {
                        self.advance();
                        TokenKind::FatArrow
                    } else {
                        TokenKind::Assign
                    }
                }
                '!' => {
                    if self.peek() == '=' {
                        self.advance();
                        TokenKind::Neq
                    } else {
                        TokenKind::Not
                    }
                }
                '<' => {
                    if self.peek() == '=' {
                        self.advance();
                        TokenKind::Leq
                    } else {
                        TokenKind::Lt
                    }
                }
                '>' => {
                    if self.peek() == '=' {
                        self.advance();
                        TokenKind::Geq
                    } else {
                        TokenKind::Gt
                    }
                }
                ':' => {
                    if self.peek() == '=' {
                        self.advance();
                        TokenKind::ColonEq
                    } else {
                        TokenKind::Colon
                    }
                }
                '&' => TokenKind::And,
                '@' => {
                    if self.peek() == '@' {
                        self.advance();
                        TokenKind::AtAt
                    } else {
                        TokenKind::At
                    }
                }
                '$' => TokenKind::Dollar,

                // ── String literals ───────────────────────────────────────
                '"' => self.lex_string(span)?,

                // ── Numbers ───────────────────────────────────────────────
                '0'..='9' => self.lex_number(ch),

                // ── Identifiers and keywords ──────────────────────────────
                'a'..='z' | 'A'..='Z' => self.lex_ident(ch),
                '_' => TokenKind::Underscore,

                // ── Unknown character ─────────────────────────────────────
                _ => {
                    return Err(LexError::UnexpectedChar { ch, span });
                }
            };

            tokens.push(Token { kind, span });
        }

        tokens.push(Token {
            kind: TokenKind::Eof,
            span: self.current_span(),
        });

        Ok(tokens)
    }
```

with:

```rust
    pub fn tokenize(&mut self) -> Result<Vec<Token>, LexError> {
        let (tokens, mut errors) = self.tokenize_recovering();
        if errors.is_empty() {
            Ok(tokens)
        } else {
            Err(errors.remove(0))
        }
    }

    /// Tokenizes the entire source string, continuing past errors instead of
    /// stopping at the first one.
    ///
    /// Returns every token that could be produced (best effort — a string
    /// literal with an invalid escape still yields a `StringLit` token, with
    /// the bad escape skipped) alongside every [`LexError`] encountered, in
    /// source order. `tokenize()` is a thin wrapper around this that
    /// preserves the original stop-at-first-error contract.
    pub fn tokenize_recovering(&mut self) -> (Vec<Token>, Vec<LexError>) {
        let mut tokens = Vec::new();
        let mut errors = Vec::new();

        while !self.is_at_end() {
            let span: Span = self.current_span();
            let ch: char = self.advance();

            let kind = match ch {
                // ── Whitespace ────────────────────────────────────────────
                // Skip silently — whitespace carries no meaning in HULK.
                ' ' | '\t' | '\r' | '\n' => continue,

                // ── Single-character tokens ───────────────────────────────
                '+' => TokenKind::Plus,
                '-' => {
                    if self.peek() == '>' {
                        self.advance();
                        TokenKind::Arrow
                    } else {
                        TokenKind::Minus
                    }
                }
                '*' => TokenKind::Star,
                '/' => {
                    if self.peek() == '/' {
                        // This is a comment — skip everything until end of line.
                        // WHY: HULK uses // for single-line comments like most languages.
                        while self.peek() != '\n' && !self.is_at_end() {
                            self.advance();
                        }
                        continue;
                    } else {
                        TokenKind::Slash
                    }
                }
                '^' => TokenKind::Caret,
                '%' => TokenKind::Percent,
                '(' => TokenKind::LParen,
                ')' => TokenKind::RParen,
                '{' => TokenKind::LBrace,
                '}' => TokenKind::RBrace,
                '[' => TokenKind::LBracket,
                ']' => TokenKind::RBracket,
                ';' => TokenKind::Semicolon,
                ',' => TokenKind::Comma,
                '.' => TokenKind::Dot,
                '|' => TokenKind::Or,

                // ── One-or-two character tokens ───────────────────────────
                '=' => {
                    if self.peek() == '=' {
                        self.advance();
                        TokenKind::EqEq
                    } else if self.peek() == '>' {
                        self.advance();
                        TokenKind::FatArrow
                    } else {
                        TokenKind::Assign
                    }
                }
                '!' => {
                    if self.peek() == '=' {
                        self.advance();
                        TokenKind::Neq
                    } else {
                        TokenKind::Not
                    }
                }
                '<' => {
                    if self.peek() == '=' {
                        self.advance();
                        TokenKind::Leq
                    } else {
                        TokenKind::Lt
                    }
                }
                '>' => {
                    if self.peek() == '=' {
                        self.advance();
                        TokenKind::Geq
                    } else {
                        TokenKind::Gt
                    }
                }
                ':' => {
                    if self.peek() == '=' {
                        self.advance();
                        TokenKind::ColonEq
                    } else {
                        TokenKind::Colon
                    }
                }
                '&' => TokenKind::And,
                '@' => {
                    if self.peek() == '@' {
                        self.advance();
                        TokenKind::AtAt
                    } else {
                        TokenKind::At
                    }
                }
                '$' => TokenKind::Dollar,

                // ── String literals ───────────────────────────────────────
                '"' => self.lex_string(span, &mut errors),

                // ── Numbers ───────────────────────────────────────────────
                '0'..='9' => self.lex_number(ch),

                // ── Identifiers and keywords ──────────────────────────────
                'a'..='z' | 'A'..='Z' => self.lex_ident(ch),
                '_' => TokenKind::Underscore,

                // ── Unknown character ─────────────────────────────────────
                // WHY: record and skip rather than abort, so scanning can
                // continue and find later errors in the same pass.
                _ => {
                    errors.push(LexError::UnexpectedChar { ch, span });
                    continue;
                }
            };

            tokens.push(Token { kind, span });
        }

        tokens.push(Token {
            kind: TokenKind::Eof,
            span: self.current_span(),
        });

        (tokens, errors)
    }
```

Then update `lex_string` (previously returning `Result<TokenKind, LexError>`) to always return a `TokenKind` and push errors into a caller-supplied vec instead of aborting. Replace:

```rust
    fn lex_string(&mut self, open_span: Span) -> Result<TokenKind, LexError> {
        let mut text = String::new();

        loop {
            // Check for EOF before advancing — an unclosed string is an error.
            // WHY: we report open_span (where the string started) not current
            // position, because that's where the programmer made the mistake.

            if self.is_at_end() {
                return Err(LexError::UnterminatedString { span: open_span });
            }

            let ch = self.advance();

            match ch {
                // Closing quote — string is complete, return what we collected.
                '"' => return Ok(TokenKind::StringLit(text)),

                // Escape sequence — the next character has special meaning.
                '\\' => {
                    // Capture the span of the character after the backslash before consuming it,
                    // so that InvalidEscape points at the unrecognised character, not past it.
                    let escape_span = self.current_span();
                    let escaped = self.advance();

                    match escaped {
                        '"' => text.push('"'),   // literal quote
                        '\\' => text.push('\\'), // literal backslash
                        'n' => text.push('\n'),  // new line
                        't' => text.push('\t'),  // tab

                        // HULK spec §A.2.2: only \", \\, \n, \t are valid escapes.
                        // Any other \X is a hard error — silent pass-through would hide bugs.
                        other => {
                            return Err(LexError::InvalidEscape {
                                ch: other,
                                span: escape_span,
                            })
                        }
                    }
                }

                _ => text.push(ch),
            }
        }
    }
```

with:

```rust
    fn lex_string(&mut self, open_span: Span, errors: &mut Vec<LexError>) -> TokenKind {
        let mut text = String::new();

        loop {
            // Check for EOF before advancing — an unclosed string is an error.
            // WHY: we report open_span (where the string started) not current
            // position, because that's where the programmer made the mistake.

            if self.is_at_end() {
                errors.push(LexError::UnterminatedString { span: open_span });
                return TokenKind::StringLit(text);
            }

            let ch = self.advance();

            match ch {
                // Closing quote — string is complete, return what we collected.
                '"' => return TokenKind::StringLit(text),

                // Escape sequence — the next character has special meaning.
                '\\' => {
                    // Capture the span of the character after the backslash before consuming it,
                    // so that InvalidEscape points at the unrecognised character, not past it.
                    let escape_span = self.current_span();
                    let escaped = self.advance();

                    match escaped {
                        '"' => text.push('"'),   // literal quote
                        '\\' => text.push('\\'), // literal backslash
                        'n' => text.push('\n'),  // new line
                        't' => text.push('\t'),  // tab

                        // HULK spec §A.2.2: only \", \\, \n, \t are valid escapes.
                        // Any other \X is a hard error — record and skip rather
                        // than abort, so the rest of the string (and file) can
                        // still be scanned.
                        other => {
                            errors.push(LexError::InvalidEscape {
                                ch: other,
                                span: escape_span,
                            });
                        }
                    }
                }

                _ => text.push(ch),
            }
        }
    }
```

Update the doc comment above `pub fn tokenize` (now the wrapper) to reflect the new delegation — replace its doc comment:

```rust
    /// Tokenizes the entire source string into a flat list of tokens.
    ///
    /// Skips whitespace and comments. Returns [`LexError`] on the first
    /// unrecognised character or unterminated string literal.
    ///
    /// # Errors
    /// - [`LexError::UnexpectedChar`] — character belongs to no HULK token.
    /// - [`LexError::UnterminatedString`] — string literal never closed.
```

with:

```rust
    /// Tokenizes the entire source string into a flat list of tokens.
    ///
    /// Stops at the first error, matching the compiler's CLI contract. See
    /// [`Lexer::tokenize_recovering`] to collect every error in one pass.
    ///
    /// # Errors
    /// - [`LexError::UnexpectedChar`] — character belongs to no HULK token.
    /// - [`LexError::UnterminatedString`] — string literal never closed.
    /// - [`LexError::InvalidEscape`] — unrecognised escape sequence in a string.
```

- [ ] **Step 4: Run the full lexer test suite**

Run: `cargo test -p hulk-lexer`
Expected: PASS — all existing tests plus the three new ones from Step 1.

- [ ] **Step 5: Commit**

```bash
git add crates/hulk-lexer/src/lib.rs
git commit -m "feat(lexer): add tokenize_recovering for multi-error reporting"
```

---

### Task 2: Parser — `parse_recovering`

**Files:**
- Modify: `crates/hulk-parser/src/lib.rs`

**Interfaces:**
- Consumes: nothing new from Task 1 (this crate depends on `hulk_lexer::Token`/`TokenKind`, unchanged).
- Produces: `pub fn parse_recovering(tokens: Vec<Token>) -> (Program, Vec<ParseError>)` (module-level, alongside the existing `pub fn parse`), and `pub fn parse_program_recovering(&mut self) -> (Program, Vec<ParseError>)` on `Ll1Parser`. Recovers at top-level-declaration granularity: on a failed declaration, skips tokens until one that starts a new declaration or the entry expression. If the entry expression itself fails to parse or is missing, the error is recorded and `program.entry` is set to a placeholder `Expr::number(0.0, span)` (never leaves `Program` in a partially-constructed/invalid state).

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block at the bottom of `crates/hulk-parser/src/lib.rs` (after `parses_vector_comprehension`, i.e. after line 2077 in the current file):

```rust
    #[test]
    fn parse_recovering_reports_error_per_broken_declaration_and_keeps_valid_ones() {
        let source = "function broken(x: Number\nfunction tan(x: Number): Number => sin(x) / cos(x);\nprint(tan(PI));";
        let tokens = Lexer::new(source).tokenize().expect("valid tokens");
        let (program, errors) = parse_recovering(tokens);

        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0].kind,
            ParseErrorKind::UnexpectedToken { .. }
        ));

        assert_eq!(program.declarations.len(), 1);
        match &program.declarations[0].kind {
            DeclarationKind::Function(function) => assert_eq!(function.name, "tan"),
            other => panic!("expected function declaration, got {other:?}"),
        }
        assert!(matches!(program.entry.kind, ExprKind::Call(_)));
    }

    #[test]
    fn parse_recovering_falls_back_to_placeholder_entry_when_entry_expression_missing() {
        let source = "function tan(x: Number): Number => sin(x) / cos(x);\n)";
        let tokens = Lexer::new(source).tokenize().expect("valid tokens");
        let (program, errors) = parse_recovering(tokens);

        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0].kind,
            ParseErrorKind::ExpectedExpression { .. }
        ));
        assert_eq!(program.declarations.len(), 1);
        assert!(matches!(
            program.entry.kind,
            ExprKind::Literal(Literal::Number(n)) if n == 0.0
        ));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p hulk-parser`
Expected: compile error — `parse_recovering` is not defined.

- [ ] **Step 3: Implement `parse_recovering`**

In `crates/hulk-parser/src/lib.rs`, add a module-level function right after the existing `pub fn parse` (after line 32):

```rust
/// Parses a complete HULK program from a token stream, recovering from
/// errors at top-level-declaration granularity instead of stopping at the
/// first one. See [`Ll1Parser::parse_program_recovering`].
pub fn parse_recovering(tokens: Vec<Token>) -> (Program, Vec<ParseError>) {
    Ll1Parser::new(tokens).parse_program_recovering()
}
```

Then add the following methods on `impl Ll1Parser`, placed directly after `parse_program` (after line 152, before `fn parse_declaration`):

```rust
    /// Parses a full HULK program like [`Ll1Parser::parse_program`], but
    /// never stops at the first error.
    ///
    /// Recovery granularity is one top-level declaration: if a `function`,
    /// `def`, `type`, or `protocol` declaration fails to parse, the error is
    /// recorded and the parser skips tokens until it finds one that starts
    /// another declaration or the entry expression, then continues. If the
    /// trailing entry expression itself fails to parse (or is missing), the
    /// error is recorded and a placeholder `0` literal is used as the entry
    /// so the returned `Program` is always well-formed.
    pub fn parse_program_recovering(&mut self) -> (Program, Vec<ParseError>) {
        let mut declarations = Vec::new();
        let mut errors = Vec::new();

        while self.lookahead_starts_declaration() {
            match self.parse_declaration() {
                Ok(decl) => declarations.push(decl),
                Err(err) => {
                    errors.push(err);
                    self.synchronize_to_next_declaration();
                }
            }
        }

        let entry = if self.lookahead_starts_expression() {
            match self.parse_expression() {
                Ok(expr) => {
                    self.consume_optional_semicolons();
                    expr
                }
                Err(err) => {
                    let span = err.span;
                    errors.push(err);
                    Expr::number(0.0, span)
                }
            }
        } else {
            let span = self.peek_span();
            errors.push(ParseError::new(
                ParseErrorKind::ExpectedExpression {
                    found: token_kind_name(&self.peek().kind),
                },
                span,
            ));
            Expr::number(0.0, span)
        };

        (Program::new(declarations, entry), errors)
    }

    /// Skips tokens until one that starts a new top-level declaration or the
    /// entry expression, or end of file — whichever comes first.
    fn synchronize_to_next_declaration(&mut self) {
        while !self.is_at_end()
            && !self.lookahead_starts_declaration()
            && !self.lookahead_starts_expression()
        {
            self.advance();
        }
    }
```

- [ ] **Step 4: Run the full parser test suite**

Run: `cargo test -p hulk-parser`
Expected: PASS — all existing tests plus the two new ones from Step 1.

- [ ] **Step 5: Commit**

```bash
git add crates/hulk-parser/src/lib.rs
git commit -m "feat(parser): add parse_recovering for multi-error reporting"
```

---

## Post-plan check

- [ ] Run `cargo test --workspace` from the repo root and confirm the whole workspace still builds and every test passes (not just the two touched crates) — `hulk-transpile`, `hulk-semantic`, `hulk-codegen`, and `hulk-cli` all depend on `hulk-ast`/`hulk-lexer`/`hulk-parser` and must be unaffected since only new, additive functions were introduced.
