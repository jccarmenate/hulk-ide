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
5. Run a `.hulk` file from the editor (compile + execute, output shown in a
   terminal).
6. Lexer and parser error recovery: report multiple lexical/syntactic errors
   per pass instead of stopping at the first one, at least at top-level
   declaration granularity (see "Lexer/parser error recovery" below).

## Non-goals (v1)

- Step-through debugging (breakpoints, DAP integration) — "run" (goal 5)
  means compile-and-execute-with-output only.
- Fine-grained (sub-declaration / inside-a-block) parser error recovery —
  goal 6 recovers at top-level declaration boundaries; recovering inside a
  single function/block body is a documented future improvement (HULK is
  expression-based with no statement boundaries, which makes intra-block
  recovery considerably harder — not worth the risk for v1).
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

`hulk-codegen` and `hulk-rt` are kept unchanged and are still not depended
on by `hulk-lsp` — the "run" feature (goal 5) is implemented entirely in
the VS Code extension, which shells out to the existing `hulk-cli` binary
rather than the LSP driving codegen itself (see "Running code from the
editor" below). `hulk-lexer` and `hulk-parser` gain new additive entry
points (see "Lexer/parser error recovery" below); `hulk-cli`'s own
behavior is unchanged.

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

1. `hulk_lexer::Lexer::new(text).tokenize_recovering()` (new recovering
   entry point, see "Lexer/parser error recovery" below) → best-effort
   token stream plus `Vec<LexError>`. Publish all lex errors as
   diagnostics. If there are any, stop here for this revision (parsing a
   token stream with recovery gaps is not attempted in v1) — last-good tree
   untouched.
2. On success (no lex errors): `hulk_transpile::expand_program` (macro
   expansion) then the new recovering parse entry point (see below) →
   best-effort `Program` plus `Vec<ParseError>`, recovered at top-level
   declaration boundaries. Publish all parse errors as diagnostics. If any
   top-level declaration failed to parse, semantic analysis still runs
   against the declarations that *did* parse successfully (partial
   program), so the rest of the file keeps getting semantic diagnostics
   and last-good-tree updates even while one declaration has a syntax
   error.
3. `hulk_semantic::analyze(&program)` on whatever declarations parsed.
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

### Lexer/parser error recovery

Both crates get a new, additive "recovering" entry point; the existing
single-error entry points used by `hulk-cli` are left in place and
unchanged (its stderr output and exit-code contract do not change).

- **Lexer** (`crates/hulk-lexer`): the internal scan loop already
  identifies the same failure cases (`unexpected char`, `unterminated
  string`, `invalid escape`) that `tokenize()` returns on. Refactor that
  loop into a shared internal function that, instead of returning on the
  first failure, records a `LexError`, skips the offending character(s)
  (or advances to end-of-line for an unterminated string) and keeps
  scanning. `tokenize()` becomes a thin wrapper: run the shared loop, and
  if its error vec is non-empty return `Err(errors[0])` (bit-for-bit the
  old behavior) else `Ok(tokens)`. The new `tokenize_recovering(&mut self)
  -> (Vec<Token>, Vec<LexError>)` exposes the full result. `hulk-cli` keeps
  calling `tokenize()`; `hulk-lsp` calls `tokenize_recovering()`.
- **Parser** (`crates/hulk-parser`): recovery granularity is one top-level
  declaration (`function`, `type`, `protocol`, or the trailing global
  expression — see `GRAMMAR_LL1.md` for the top-level production). Add
  `parse_recovering(tokens: Vec<Token>) -> (Program, Vec<ParseError>)`: it
  parses top-level items in a loop; when parsing one item fails, it records
  the `ParseError`, then skips tokens until it finds one that starts a new
  top-level item (`function`, `type`, `protocol` keyword) or reaches EOF,
  and resumes the loop from there. The existing `parse()` /
  `parse_program()` (single error, stops immediately) are unchanged and
  keep serving `hulk-cli`.
- Because `hulk_transpile::expand_program` runs on the `Program` produced
  by parsing, it operates on whatever top-level declarations survived
  recovery — a macro inside a declaration that failed to parse is simply
  absent from that `Program`, which is correct (nothing to expand there).

### Running code from the editor

A VS Code command (`hulk.runFile`, bound to an editor title button and the
command palette) that does not involve `hulk-lsp` or the JSON-RPC
connection at all:

1. Save the active document if dirty.
2. Reuse (or create) a dedicated VS Code integrated terminal named "HULK".
3. Send it a shell command that runs the already-built `hulk-cli` binary
   against the file's path, and on success runs the produced `./output`
   binary — e.g. `hulk-cli path/to/file.hulk && ./output` (exact
   invocation/working-directory handling to be finalized in the
   implementation plan, including the Windows executable extension).
   `hulk-cli` already writes compiler errors to stderr in the
   `(line,col) TYPE: message` format and sets a non-zero exit code on
   failure, so `&&` naturally skips execution when compilation fails and
   the user sees the compiler's own error output in the terminal.

**Prerequisite / risk**: `hulk-cli`'s codegen path links a native
executable via `cc` and needs LLVM 17 (`inkwell`'s `llvm17-0` feature) and
a C linker available on the machine. The original project's dev/CI
environment was not confirmed to be Windows. Verifying that `cargo build
-p hulk-cli` succeeds on this Windows machine (and what toolchain it needs
— e.g. MSVC, MinGW, or WSL) is the first task of the implementation plan
for goal 5, before any extension-side "run" code is written.

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
  `hulk-lsp` binary over stdio, wires it to `.hulk` documents; also
  registers the `hulk.runFile` command described in "Running code from the
  editor" above.

The extension ships the `hulk-lsp` binary path via configuration during
development (points at `target/debug/hulk-lsp` or a workspace setting);
packaging/distribution (e.g. bundling a prebuilt binary) is out of scope for
v1 — this is a personal-use extension run from source.

## Known limitations (v1, by design)

- Parser recovery is at top-level-declaration granularity only: multiple
  syntax errors within the *same* function/type body still surface as one
  error for that declaration (the rest of the declaration's body is
  skipped until the next top-level item). Other declarations in the file
  are unaffected.
- No formatting, no rename-symbol, no find-all-references, no code actions.
- No step-through debugging — "run" is compile-and-execute only, output
  goes to a terminal, not an inline debugger.

## Testing

- `hulk-lexer` / `hulk-parser`: unit tests for the new recovering entry
  points — a source with two separate lexical errors both get reported by
  `tokenize_recovering`; a source with a broken `function` followed by a
  valid `type` gets one `ParseError` plus a `Program` that still contains
  the valid `type` declaration. Existing tests for `tokenize()`/`parse()`
  must keep passing unchanged (proves the single-error entry points are
  untouched).
- `hulk-lsp`: Rust integration tests that feed HULK source snippets
  directly into the pipeline functions used by the diagnostics handler
  (`tokenize_recovering` → `expand_program` → `parse_recovering` →
  `analyze`) and assert the resulting diagnostic list (message, severity,
  position) — no actual LSP transport/JSON-RPC involved, these test the
  analysis-to-diagnostics mapping logic directly. Separately, a handful of
  tests drive the position-index/scope-helper functions used by
  hover/completion/goto-def against known snippets and assert the expected
  span/type is found.
- Extension: manual smoke test (open a `.hulk` file, confirm highlighting,
  introduce an error, confirm the squiggle appears and disappears on fix,
  check hover and completion on a small sample file, run a working file via
  `hulk.runFile` and confirm its output appears in the terminal, run a file
  with a compile error and confirm the compiler error appears instead of a
  stale/no binary being executed). Not worth automating VS Code UI for a
  personal project.

## Open items deferred to the implementation plan

- Exact `tower-lsp` handler wiring and crate/module breakdown within
  `hulk-lsp`.
- Exact shape of the position-index data structure.
- Whether the scope-helper extraction from `hulk-semantic`'s inference pass
  is a new public function or an internal one re-exported for `hulk-lsp`.
- Verifying the local Windows toolchain can actually build `hulk-cli`
  (LLVM 17 + C linker) — first task for the "run" feature; may require
  installing LLVM/MinGW or documenting a WSL-based workaround.
- The exact set of "top-level item start" tokens the parser's recovery
  resynchronizes on, and how it handles a syntax error inside the trailing
  global expression (which has no following top-level keyword to
  resynchronize to — likely just skips to EOF for that case).
- Exact terminal invocation for `hulk.runFile` (working directory, how the
  extension locates the `hulk-cli`/`output` paths, Windows `.exe` handling).
