# hulk-lsp Diagnostics Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the `hulk-lsp` crate as a working Language Server Protocol server that provides live diagnostics (lexical, syntactic, macro-expansion, and semantic errors/warnings) for `.hulk` files over stdio, using the compiler crates directly as libraries — no LLVM, no shelling out to `hulk-cli`.

**Architecture:** `hulk-lsp` is a new binary crate with two files: `diagnostics.rs` holds a pure, fully unit-tested function `compute_diagnostics(text: &str) -> Vec<Diagnostic>` that runs the compiler pipeline (`tokenize_recovering` → `parse_recovering` → `expand_program` → `analyze`, mirroring `hulk-cli`'s phase order) and maps every error/warning to an LSP `Diagnostic`; `backend.rs` holds the `tower-lsp` `LanguageServer` trait implementation, which is thin glue that stores each open document's text and calls `compute_diagnostics` on `didOpen`/`didChange`, publishing the result via `Client::publish_diagnostics`. This is Plan 2 of the `hulk-ide` roadmap — hover/completion/go-to-definition (which need a position index built from the typed AST) are Plan 3, and the VS Code extension is Plan 4.

**Tech Stack:** Rust, `tower-lsp` 0.20.0 (LSP server framework, re-exports `lsp_types`), `tokio` (async runtime), the existing `hulk-ast`/`hulk-lexer`/`hulk-parser`/`hulk-transpile`/`hulk-semantic` crates as path dependencies. No LLVM/`hulk-codegen` dependency.

**Spec:** [docs/superpowers/specs/2026-09-01-hulk-ide-design.md](../specs/2026-09-01-hulk-ide-design.md) — see "`hulk-lsp` crate", "Diagnostics pipeline", and "Per-document state" sections. See also [docs/superpowers/plans/2026-09-01-lexer-parser-error-recovery.md](2026-09-01-lexer-parser-error-recovery.md) (Plan 1, already merged) for `tokenize_recovering`/`parse_recovering`, which this plan consumes.

## Global Constraints

- `hulk-lsp` depends only on `hulk-ast`, `hulk-lexer`, `hulk-parser`, `hulk-transpile`, `hulk-semantic`, `tower-lsp`, and `tokio` — never `hulk-codegen` or `hulk-rt` (keeps the server LLVM-free).
- The diagnostics pipeline never touches disk and never shells out to `hulk-cli`; it operates purely on the in-memory buffer text the editor sends.
- `compute_diagnostics` must be a plain, synchronous, dependency-free-of-`Client` function so it can be unit-tested without any LSP transport — this is where all the pipeline-mapping logic and its tests live.
- HULK source spans are 1-based (`line`, `col` both starting at 1); LSP `Position` is 0-based. Every conversion goes through one shared helper (`span_to_range`) to avoid an off-by-one mismatch creeping into only some diagnostics.

---

### Task 1: Crate scaffold and server capabilities

**Files:**
- Modify: `Cargo.toml` (root workspace — add `crates/hulk-lsp` to `members`)
- Create: `crates/hulk-lsp/Cargo.toml`
- Create: `crates/hulk-lsp/src/main.rs`
- Create: `crates/hulk-lsp/src/backend.rs`

**Interfaces:**
- Produces: `hulk_lsp::backend::Backend` (via `mod backend;` in `main.rs` — this is a binary crate, so `backend` is a private module reached through `main.rs`, not a library API) with `Backend::new(client: Client) -> Backend`, implementing `tower_lsp::LanguageServer`. `pub fn server_capabilities() -> ServerCapabilities` is the piece of this task that has a real unit test.
- Consumes: nothing from other tasks yet (Task 2's `diagnostics` module is wired in by Task 3).

- [ ] **Step 1: Register the new crate in the workspace**

In the root `Cargo.toml`, add `"crates/hulk-lsp"` to `members`:

```toml
[workspace]
resolver = "2"
members = [
    "crates/hulk-ast",
    "crates/hulk-lexer",
    "crates/hulk-parser",
    "crates/hulk-semantic",
    "crates/hulk-codegen",
    "crates/hulk-rt",
    "crates/hulk-transpile",
    "crates/hulk-cli",
    "crates/hulk-lsp",
]
```

- [ ] **Step 2: Create the crate manifest**

Create `crates/hulk-lsp/Cargo.toml`:

```toml
[package]
name = "hulk-lsp"
version.workspace = true
edition.workspace = true
license.workspace = true

[[bin]]
name = "hulk-lsp"
path = "src/main.rs"

[dependencies]
hulk-ast = { path = "../hulk-ast" }
hulk-lexer = { path = "../hulk-lexer" }
hulk-parser = { path = "../hulk-parser" }
hulk-transpile = { path = "../hulk-transpile" }
hulk-semantic = { path = "../hulk-semantic" }
tower-lsp = "0.20.0"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "io-std"] }
```

- [ ] **Step 3: Write the failing test for server capabilities**

Create `crates/hulk-lsp/src/backend.rs` with just the test and the function signature it needs (no body yet other than `todo!()`), so the test fails to compile against a real implementation first:

```rust
//! The `tower-lsp` `LanguageServer` implementation for HULK.
//!
//! This module is intentionally thin: it stores each open document's text
//! and delegates all compiler-pipeline work to [`crate::diagnostics`].

use tower_lsp::lsp_types::{ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind};

/// The capabilities this server advertises to the client during
/// `initialize`. Pulled out as its own function so it can be unit-tested
/// without spinning up a real `tower-lsp` `Client`/`Server` pair.
pub fn server_capabilities() -> ServerCapabilities {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_capabilities_use_full_text_document_sync() {
        let caps = server_capabilities();
        assert_eq!(
            caps.text_document_sync,
            Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL))
        );
    }
}
```

Create a minimal `crates/hulk-lsp/src/main.rs` so the crate builds as a binary (this will be filled in for real in Step 5):

```rust
mod backend;

fn main() {}
```

- [ ] **Step 4: Run the test to verify it fails**

Run: `cargo test -p hulk-lsp`
Expected: compiles, then panics at runtime with `not yet implemented` (from `todo!()`) — confirms the test actually exercises `server_capabilities`.

- [ ] **Step 5: Implement `server_capabilities` and the `Backend` struct**

Replace the `todo!()` body in `crates/hulk-lsp/src/backend.rs`'s `server_capabilities` with:

```rust
pub fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        ..ServerCapabilities::default()
    }
}
```

Then add the `Backend` struct and its minimal `LanguageServer` implementation below the `server_capabilities` function (document storage and diagnostics wiring come in Task 3 — for now `did_open`/`did_change`/`did_close` don't exist yet, since `LanguageServer` provides default no-op implementations for everything except `initialize` and `shutdown`):

```rust
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{InitializeParams, InitializeResult, InitializedParams};
use tower_lsp::{Client, LanguageServer};

/// The HULK language server.
pub struct Backend {
    #[allow(dead_code)] // used starting in Task 3
    client: Client,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: server_capabilities(),
            server_info: None,
        })
    }

    async fn initialized(&self, _: InitializedParams) {}

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
```

- [ ] **Step 6: Wire up `main.rs` to actually run the server**

Replace `crates/hulk-lsp/src/main.rs` with:

```rust
//! Entry point for the HULK language server.
//!
//! Speaks LSP over stdio, as every editor integration expects.

mod backend;

use tower_lsp::{LspService, Server};

use backend::Backend;

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
```

- [ ] **Step 7: Run the test to verify it passes, and that the binary builds**

Run: `cargo test -p hulk-lsp`
Expected: PASS (`server_capabilities_use_full_text_document_sync`).

Run: `cargo build -p hulk-lsp`
Expected: builds successfully, producing `target/debug/hulk-lsp(.exe)`.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml crates/hulk-lsp
git commit -m "feat(lsp): scaffold hulk-lsp crate with minimal LanguageServer"
```

---

### Task 2: Diagnostics pipeline (`compute_diagnostics`)

**Files:**
- Create: `crates/hulk-lsp/src/diagnostics.rs`
- Modify: `crates/hulk-lsp/src/main.rs` (add `mod diagnostics;`)

**Interfaces:**
- Consumes: `hulk_lexer::Lexer::tokenize_recovering` (Plan 1), `hulk_parser::parse_recovering` (Plan 1), `hulk_transpile::expand_program`, `hulk_semantic::analyze`.
- Produces: `pub fn compute_diagnostics(text: &str) -> Vec<Diagnostic>` (`Diagnostic` = `tower_lsp::lsp_types::Diagnostic`). This is what Task 3's `did_open`/`did_change` handlers call.

- [ ] **Step 1: Write the failing tests**

Create `crates/hulk-lsp/src/diagnostics.rs`:

```rust
//! Turns HULK source text into LSP diagnostics.
//!
//! Runs the same phases as `hulk-cli` in the same order — lex, parse,
//! expand macros, analyze — but uses the recovering lexer/parser entry
//! points (see `hulk-lexer`/`hulk-parser`) so every error in a phase is
//! reported at once instead of stopping at the first one.

use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

use hulk_lexer::LexError;
use hulk_semantic::SemanticError;

/// Runs the full compiler pipeline over `text` and returns every diagnostic
/// found. Never panics on malformed input — every phase either recovers or
/// short-circuits cleanly.
pub fn compute_diagnostics(text: &str) -> Vec<Diagnostic> {
    todo!()
}

fn lex_diagnostic(err: &LexError) -> Diagnostic {
    todo!()
}

fn semantic_diagnostic(err: &SemanticError, severity: DiagnosticSeverity) -> Diagnostic {
    todo!()
}

fn diagnostic(line: usize, col: usize, severity: DiagnosticSeverity, message: String) -> Diagnostic {
    todo!()
}

/// Converts a 1-based HULK source position to a one-character 0-based LSP
/// range. HULK spans are single points, not ranges, so a one-character
/// range is the most precise conversion available without deeper
/// AST/token-length lookup.
fn span_to_range(line: usize, col: usize) -> Range {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_program_has_no_diagnostics() {
        let diagnostics = compute_diagnostics("print(1 + 2);");
        assert!(diagnostics.is_empty(), "unexpected diagnostics: {diagnostics:?}");
    }

    #[test]
    fn lexical_error_short_circuits_to_a_single_diagnostic_with_correct_range() {
        let diagnostics = compute_diagnostics("#");
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("unexpected character"));
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(
            diagnostics[0].range,
            Range {
                start: Position { line: 0, character: 0 },
                end: Position { line: 0, character: 1 },
            }
        );
    }

    #[test]
    fn parse_error_is_reported_and_semantic_analysis_still_runs_on_the_rest() {
        let source = "function broken(x: Number\nfunction tan(x: Number): Number => sin(x) / cos(x);\nprint(tan(PI));";
        let diagnostics = compute_diagnostics(source);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.severity == Some(DiagnosticSeverity::ERROR) && d.range.start.line == 1),
            "expected a parse error diagnostic on 0-based line 1, got: {diagnostics:?}"
        );
    }

    #[test]
    fn semantic_analysis_reports_every_error_not_just_the_first() {
        let diagnostics = compute_diagnostics("{ print(a); print(b); }");
        assert_eq!(
            diagnostics.len(),
            2,
            "expected both undefined variables to be reported: {diagnostics:?}"
        );
        assert!(diagnostics.iter().all(|d| d.severity == Some(DiagnosticSeverity::ERROR)));
        assert!(diagnostics.iter().any(|d| d.message.contains("undefined variable `a`")));
        assert!(diagnostics.iter().any(|d| d.message.contains("undefined variable `b`")));
    }

    #[test]
    fn semantic_warning_is_reported_with_warning_severity() {
        let diagnostics = compute_diagnostics("match 1 { case n: Number => \"number\"; }");
        assert_eq!(diagnostics.len(), 1, "expected exactly one warning: {diagnostics:?}");
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::WARNING));
        assert!(diagnostics[0].message.contains("non-exhaustive match"));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p hulk-lsp diagnostics::`
Expected: compiles, then the first test to run panics with `not yet implemented` (from `todo!()`).

- [ ] **Step 3: Implement the pipeline**

Replace the five `todo!()` function bodies in `crates/hulk-lsp/src/diagnostics.rs`:

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

fn lex_diagnostic(err: &LexError) -> Diagnostic {
    match err {
        LexError::UnexpectedChar { ch, span } => diagnostic(
            span.line,
            span.col,
            DiagnosticSeverity::ERROR,
            format!("unexpected character '{}'", ch),
        ),
        LexError::UnterminatedString { span } => diagnostic(
            span.line,
            span.col,
            DiagnosticSeverity::ERROR,
            "unterminated string literal".to_string(),
        ),
        LexError::InvalidEscape { ch, span } => diagnostic(
            span.line,
            span.col,
            DiagnosticSeverity::ERROR,
            format!("invalid escape sequence '\\{}'", ch),
        ),
    }
}

fn semantic_diagnostic(err: &SemanticError, severity: DiagnosticSeverity) -> Diagnostic {
    diagnostic(err.span.line, err.span.col, severity, err.kind.to_string())
}

fn diagnostic(line: usize, col: usize, severity: DiagnosticSeverity, message: String) -> Diagnostic {
    Diagnostic {
        range: span_to_range(line, col),
        severity: Some(severity),
        source: Some("hulk".to_string()),
        message,
        ..Diagnostic::default()
    }
}

fn span_to_range(line: usize, col: usize) -> Range {
    let line = line.saturating_sub(1) as u32;
    let character = col.saturating_sub(1) as u32;
    Range {
        start: Position { line, character },
        end: Position {
            line,
            character: character + 1,
        },
    }
}
```

Note: `hulk_semantic::SemanticError` does **not** carry a usable public `Severity` type outside the crate (only `SemanticError`/`SemanticErrorKind` are re-exported from `hulk-semantic`'s crate root) — this is why `semantic_diagnostic` takes the severity as a parameter from the call site (which already knows it from which container the error came out of: the `Err` branch is always hard errors, `verified.warnings` is always warnings) rather than reading an `err.severity` field.

Add `mod diagnostics;` to `crates/hulk-lsp/src/main.rs`, right after `mod backend;`:

```rust
mod backend;
mod diagnostics;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p hulk-lsp`
Expected: PASS — all 6 tests (1 from Task 1, 5 new ones here).

If `semantic_warning_is_reported_with_warning_severity` fails because the match-exhaustiveness check behaves differently than expected (e.g. a different message, or zero/multiple diagnostics), read the actual failure output, find the real message in `crates/hulk-semantic/src/error.rs`'s `SemanticErrorKind::NonExhaustiveMatch` Display arm, and adjust the assertion to match reality rather than changing the pipeline code — this test's job is to confirm severity plumbing works, not to pin down exact exhaustiveness semantics.

- [ ] **Step 5: Commit**

```bash
git add crates/hulk-lsp/src/diagnostics.rs crates/hulk-lsp/src/main.rs
git commit -m "feat(lsp): add compute_diagnostics pipeline with full test coverage"
```

---

### Task 3: Wire diagnostics into the server (`didOpen`/`didChange`/`didClose`)

**Files:**
- Modify: `crates/hulk-lsp/src/backend.rs`

**Interfaces:**
- Consumes: `crate::diagnostics::compute_diagnostics` (Task 2).
- Produces: `Backend` now tracks open documents and publishes diagnostics on every open/change, clearing them on close. No new public interface — this is the last piece of Plan 2's deliverable.

Per the spec's testing section, this glue is verified by a manual protocol-level smoke test rather than an automated `cargo test` — `hulk-lsp`'s automated tests are scoped to the pipeline-mapping logic in `diagnostics.rs` (Task 2), which is where the actual risk of bugs lives. Full transport-level automation is deferred to real usage in Plan 4 (the VS Code extension).

- [ ] **Step 1: Add document storage and the publish helper**

In `crates/hulk-lsp/src/backend.rs`, add these imports (extend the existing `tower_lsp::lsp_types::{...}` import rather than duplicating it) and change the `Backend` struct and `Backend::new`:

```rust
use std::collections::HashMap;
use std::sync::RwLock;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    InitializeParams, InitializeResult, InitializedParams, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};
use tower_lsp::{Client, LanguageServer};

use crate::diagnostics::compute_diagnostics;

/// The HULK language server.
pub struct Backend {
    client: Client,
    documents: RwLock<HashMap<Url, String>>,
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
    /// closed by the time this runs).
    async fn publish_for(&self, uri: Url) {
        let text = self.documents.read().unwrap().get(&uri).cloned();
        let Some(text) = text else { return };
        let diagnostics = compute_diagnostics(&text);
        self.client.publish_diagnostics(uri, diagnostics, None).await;
    }
}
```

(The `#[allow(dead_code)]` on the old `client` field from Task 1 is removed here since `client` is now used directly in `publish_for`.)

- [ ] **Step 2: Implement `did_open`, `did_change`, `did_close`**

Add these methods inside the existing `#[tower_lsp::async_trait] impl LanguageServer for Backend` block, after `shutdown`:

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

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.documents.write().unwrap().remove(&uri);
        // Clear diagnostics so the editor doesn't keep showing stale
        // squiggles for a file that's no longer open.
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }
```

- [ ] **Step 3: Build and run the existing tests**

Run: `cargo build -p hulk-lsp`
Expected: builds successfully with no warnings about unused `client` field (it's used now) or unused imports.

Run: `cargo test -p hulk-lsp`
Expected: PASS — same 6 tests as after Task 2 (this task adds no new automated tests, per the note above).

- [ ] **Step 4: Manual smoke test**

Run this PowerShell script from the repo root to start the compiled server, send it a minimal `initialize` → `initialized` → `textDocument/didOpen` sequence over stdio with a deliberately broken HULK file, and confirm a `textDocument/publishDiagnostics` notification comes back containing the expected error message:

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

Send-LspMessage '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}'
Send-LspMessage '{"jsonrpc":"2.0","method":"initialized","params":{}}'
Send-LspMessage '{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///test.hulk","languageId":"hulk","version":1,"text":"#"}}}'

Start-Sleep -Milliseconds 500
$proc.StandardInput.Close()
$output = $proc.StandardOutput.ReadToEnd()
$proc.WaitForExit(2000) | Out-Null

if ($output -match "publishDiagnostics" -and $output -match "unexpected character") {
    Write-Output "SMOKE TEST PASSED"
} else {
    Write-Output "SMOKE TEST FAILED"
    Write-Output $output
}
```

Expected: prints `SMOKE TEST PASSED`.

- [ ] **Step 5: Commit**

```bash
git add crates/hulk-lsp/src/backend.rs
git commit -m "feat(lsp): publish live diagnostics on didOpen/didChange/didClose"
```

---

## Post-plan check

- [ ] Run `cargo test --workspace` (or, if `hulk-codegen` still can't build in this environment for lack of LLVM 17, `cargo test -p hulk-ast -p hulk-lexer -p hulk-parser -p hulk-transpile -p hulk-semantic -p hulk-lsp`) and confirm everything is green.
- [ ] Open `crates/hulk-lsp/src/backend.rs` and `diagnostics.rs` once more and confirm neither has a leftover `todo!()`.
