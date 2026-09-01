# HULK IDE — Design Spec

Date: 2026-09-01
Status: Approved for planning

## Background

`hulk-compiler` is a Rust workspace implementing a complete compiler for the
HULK language (lexer → macro expansion → parser → semantic analyzer →
LLVM/inkwell codegen → native binary). It was built as a university
compilers course project, co-authored by the repo owner and a teammate; the
owner has the teammate's permission to reuse the codebase for this new,
separate personal project.

Today the compiler is CLI-only (`hulk-cli`): it reads a file, runs the
pipeline, and on the first error in a phase prints `(line,col) TYPE: message`
to stderr and calls `process::exit`. There is no LSP, no editor tooling, and
no library-friendly "collect all errors" entry point for the lexer or
parser (semantic analysis already collects multiple errors via
`Vec<SemanticError>`; lexer and parser return `Result<_, SingleError>` and
stop at the first failure).

This project builds a VS Code extension backed by a Language Server Protocol
(LSP) server, reusing the existing compiler crates as libraries rather than
reimplementing language analysis.

## Goals (v1)

1. Syntax highlighting for `.hulk` files (static TextMate grammar).
2. Live diagnostics: lexical, syntactic, and semantic errors/warnings shown
   as editor squiggles as the user types.
3. Autocompletion: variables/parameters in scope, and members after `.` on a
   typed receiver.
4. Hover (type/signature of the identifier under the cursor) and go-to-definition.

## Non-goals (v1)

- Running or debugging HULK programs from the editor (no LLVM/codegen
  dependency in the LSP).
- Parser error recovery / multi-error reporting from the lexer and parser
  (see "Known limitations" below — v1 accepts single-error-per-phase for
  those two stages).
- Anything beyond VS Code (no web playground, no standalone desktop app).

## Repository layout

New repo `hulk-ide`, seeded as a full copy of `hulk-compiler`'s source
(crates, tests, docs) with a fresh git history — the thesis repo's git
history is not carried over, since it records joint authorship not relevant
to this personal project. This has already been done: the repo exists at
`C:\Users\Juank\Proyectos\hulk-ide` with one initial commit containing the
copied snapshot.

Target layout after this project's changes:

```
hulk-ide/
  crates/
    hulk-ast, hulk-lexer, hulk-parser, hulk-transpile,
    hulk-semantic, hulk-codegen, hulk-rt, hulk-cli   # unchanged, inherited
    hulk-lsp/                                        # new
  editors/
    vscode/                                          # new: extension (TypeScript)
```

`hulk-codegen`, `hulk-rt`, and `hulk-cli` are kept in the workspace
unchanged (for potential future use — e.g. a "run" feature — but out of
scope now) and are not depended on by `hulk-lsp`.

## `hulk-lsp` crate

A binary crate implementing an LSP server using `tower-lsp` (async, Tokio
runtime — chosen for its ecosystem maturity and the number of reference
implementations to draw from).

Dependencies: `hulk-ast`, `hulk-lexer`, `hulk-parser`, `hulk-transpile`,
`hulk-semantic` (path dependencies within the workspace). No dependency on
`hulk-codegen` or `hulk-rt` (avoids pulling in `inkwell`/LLVM, keeping the
server lightweight and fast to start).

### Per-document state

For each open document the server keeps:

- The current text content (server is the source of truth via
  `textDocument/didOpen` + `didChange`, full-document sync for v1 — no
  incremental sync, since HULK source files are small).
- The **last-good typed tree**: the most recent `VerifiedProgram` (from
  `hulk_semantic::analyze`) that was produced without a fatal error,
  together with the `TypeRegistry` from that same run. This is what
  hover/completion/go-to-definition query, so those features keep working
  off stale-but-valid data while the user is mid-edit with a syntax error
  elsewhere in the file.
- The current set of published diagnostics.

### Diagnostics pipeline (on `didOpen` / `didChange`)

Runs synchronously in memory against the buffer text (never touches disk,
never shells out to `hulk-cli`):

1. `hulk_lexer::Lexer::new(text).tokenize()`. On `Err(LexError)` → publish
   one diagnostic (severity Error) at its span, clear this document's
   diagnostics from later phases, stop. Do **not** touch the last-good tree.
2. On success: `hulk_transpile::expand_program` (macro expansion) then
   `hulk_parser::parse`. On `Err` from either → publish one diagnostic,
   stop, last-good tree untouched.
3. On success: `hulk_semantic::analyze(&program)`.
   - `Err(Vec<SemanticError>)` → publish **all** of them as diagnostics.
     Last-good tree untouched (still reflects the previous valid state).
   - `Ok(VerifiedProgram)` → publish `verified.warnings` as diagnostics
     (severity Warning), and replace the last-good tree with this result.

Diagnostic spans: `hulk_ast::SourceSpan { line, col }` is a point, not a
range (confirmed in `hulk-ast`). v1 maps each diagnostic to a
single-character LSP `Range` at that point (`{line, col}` →
`{line, col}..{line, col+1}`, converting from HULK's 1-based line/col to
LSP's 0-based). Widening this to cover a full token/node is a possible
future improvement, not required for v1.

### Hover / go-to-definition / completion

None of these are exposed as ready-made queries by `hulk-semantic` today —
`Environment` (the scope stack) is transient, rebuilt per analysis pass and
discarded after `analyze()` returns; `VerifiedProgram` only exposes the
final `TypedProgram` (AST with a resolved `Type` per node) and the global
`TypeRegistry` (types/protocols/functions). So `hulk-lsp` builds its own
position index:

- After each successful `analyze()`, walk `verified.typed_program` once and
  build a flat list of `(SourceSpan, node info)` entries — enough to, given
  a cursor position, find the innermost enclosing expression/identifier and
  its resolved `Type`.
- **Hover**: find the node at the cursor position in the last-good tree,
  render its `Type` (and, for a function/method call, its signature from
  `TypeRegistry`).
- **Go-to-definition**: for a variable reference, the binding's declaration
  span is recoverable by re-walking the enclosing scope in the last-good
  tree (`let`, function parameters, `for`, catch-all `self`) — this reuses
  the same scope-construction logic `hulk-semantic`'s inference pass already
  has, factored into a small shared helper rather than duplicated. For a
  type member (`.field`, `.method`), the definition span comes from the
  type's declaration in `TypeRegistry`.
- **Completion**: triggered on `.` (member access) or on identifier
  characters (general scope completion).
  - After `.`: infer the receiver's type from the last-good tree, list its
    fields/methods from `TypeRegistry` (including inherited members).
  - General: walk the last-good tree's scopes at the cursor position (same
    scope helper as go-to-definition) to list variables/parameters in
    scope, plus global function/type names from `TypeRegistry`.

This scope-helper reuse is the one piece of new shared code expected inside
`hulk-semantic` itself (extracting scope-at-position logic that today only
exists inline inside the inference pass); everything else in `hulk-lsp` is
additive and doesn't modify the existing crates.

## VS Code extension (`editors/vscode`)

Standard TypeScript npm project:

- `package.json` — extension manifest (language contribution for `hulk`,
  activation on `.hulk` files, points at the grammar and language config).
- `syntaxes/hulk.tmLanguage.json` — TextMate grammar covering keywords
  (`let`, `in`, `if`, `while`, `for`, `function`, `type`, `inherits`,
  `protocol`, `new`, `is`, `as`, `match`, `case`), primitive types, string/
  number literals, comments, operators. Purely static, no server involved.
- `language-configuration.json` — bracket pairs, comment tokens, auto-closing.
- `src/extension.ts` — activates `vscode-languageclient`, spawns the
  `hulk-lsp` binary over stdio, wires it to `.hulk` documents.

The extension ships the `hulk-lsp` binary path via configuration during
development (points at `target/debug/hulk-lsp` or a workspace setting);
packaging/distribution (e.g. bundling a prebuilt binary) is out of scope for
v1 — this is a personal-use extension run from source.

## Known limitations (v1, by design)

- Only one lexical or syntactic error is ever shown at a time (matches the
  underlying compiler's current error model) — if there's a syntax error,
  semantic diagnostics for the rest of the file aren't recomputed until it's
  fixed, though hover/completion still work off the last-good tree.
- No formatting, no rename-symbol, no find-all-references, no code actions.
- No execution/debugging.

## Testing

- `hulk-lsp`: Rust integration tests that feed HULK source snippets
  directly into the pipeline functions used by the diagnostics handler
  (`tokenize` → `expand_program` → `parse` → `analyze`) and assert the
  resulting diagnostic list (message, severity, position) — no actual LSP
  transport/JSON-RPC involved, these test the analysis-to-diagnostics
  mapping logic directly. Separately, a handful of tests drive the
  position-index/scope-helper functions used by hover/completion/goto-def
  against known snippets and assert the expected span/type is found.
- Extension: manual smoke test (open a `.hulk` file, confirm highlighting,
  introduce an error, confirm the squiggle appears and disappears on fix,
  check hover and completion on a small sample file). Not worth automating
  VS Code UI for a personal project.

## Open items deferred to the implementation plan

- Exact `tower-lsp` handler wiring and crate/module breakdown within
  `hulk-lsp`.
- Exact shape of the position-index data structure.
- Whether the scope-helper extraction from `hulk-semantic`'s inference pass
  is a new public function or an internal one re-exported for `hulk-lsp`.
