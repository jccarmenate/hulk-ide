# hulk-semantic

Semantic analysis for the HULK programming language.

`hulk-semantic` takes the untyped AST produced by `hulk-parser` and turns it into a
fully type-checked, fully type-*annotated* AST, ready to be handed to a code generator.
It owns name resolution, inheritance and protocol resolution, type inference, and final
type checking — everything between parsing and codegen.

```text
source text → hulk-lexer → hulk-parser → hulk-semantic → (codegen)
                                              ▲
                                         you are here
```

The crate has exactly one dependency, `hulk-ast` — it never needs the lexer or parser
at runtime, only in its own tests (`hulk-lexer` / `hulk-parser` are dev-dependencies).

---

## Entry point

```rust
pub fn analyze(program: &hulk_ast::Program) -> Result<VerifiedProgram, Vec<SemanticError>>
```

```rust
use hulk_lexer::Lexer;
use hulk_parser::parse;
use hulk_semantic::analyze;

let src = "let x: Number = 42 in print(x);";
let tokens = Lexer::new(src).tokenize().expect("lex ok");
let program = parse(tokens).expect("parse ok");

match analyze(&program) {
    Ok(verified) => {
        // verified.typed_program  -> Program<Type>, ready for codegen
        // verified.registry       -> every resolved type/protocol/function signature
        for warning in &verified.warnings {
            eprintln!("{warning}");
        }
    }
    Err(errors) => {
        for error in &errors {
            eprintln!("{error}");
        }
    }
}
```

`VerifiedProgram` is the contract with everything downstream:

```rust
pub struct VerifiedProgram {
    pub registry: TypeRegistry,        // the global knowledge base
    pub typed_program: TypedProgram,   // Program<Type> — every Expr carries a resolved Type
    pub warnings: Vec<SemanticError>,  // non-fatal diagnostics (Severity::Warning)
}
```

`analyze` never panics on malformed-but-syntactically-valid input — every failure path
that can occur during a normal compilation is represented as a `SemanticError` and
collected rather than propagated as a Rust error, so a single `analyze` call surfaces
*every* problem in the program, not just the first one.

---

## The pipeline

`analyze` runs five passes in a fixed order, threading a single `TypeRegistry` through
all of them:

| # | Pass | Module | Responsibility |
|---|------|--------|-----------------|
| 0 | Collect | `passes::collect` | Walks declarations only (no bodies) and registers every function, type, and protocol *signature* in the registry, so forward references work regardless of declaration order. |
| 1 | Hierarchy | `passes::hierarchy` | Resolves `inherits`/`extends` links, rejects invalid inheritance, detects cycles, checks override and protocol-variance rules, and flattens attribute/method tables (parent → child). |
| 1.5 | Resolve constructor params | `passes::resolve_constructor_params` | Infers unannotated type-constructor parameters from `new T(...)` call sites and propagates resolved types up through `inherits Parent(args)` clauses. |
| 2 | Infer | `passes::infer` | Builds the fully typed tree (`Program<Type>`), assigning a `Type` to every expression and resolving every unannotated symbol it can. |
| 3 | Check | `passes::check` | Re-validates explicit annotations against what Pass 2 settled on, sweeps for leftover `Unknown` types, and enforces attribute privacy. |

`analyze` exits early after Pass 1 if any hard error was reported there: a broken
hierarchy makes `Type::conforms_to` itself ill-defined, so continuing into inference
would only produce a flood of misleading cascade errors.

### Pass 0 — Collect

Registers every global function, type, and protocol by name, recording parameter and
return types as written (unannotated slots become `Type::Unknown`, to be resolved
later). Reports `DuplicateFunction`, `DuplicateType` (types and protocols share one
namespace), `DuplicateAttribute`, `DuplicateMethod`, `DuplicateParameter`, and
`MissingTypeAnnotation` for protocol methods, which must be fully typed since they have
no body to infer from.

### Pass 1 — Hierarchy & protocol resolution

Runs in a deliberate order so that one bad inheritance link doesn't cascade into
unrelated diagnostics:

1. Resolve every `inherits` link and check the parent exists (`InheritFromUndefinedType`
   otherwise; the link is cleared so later steps don't trip over it again).
2. Reject inheritance from a builtin value type — `Number`, `String`, `Boolean`
   (`InheritFromBuiltinValueType`).
3. Detect cycles in the inheritance graph via DFS (`InheritanceCycle`, with the offending
   path and the span of the type that closes the cycle). A cycle is a hard error that
   disables override checking and table flattening for the rest of this pass, since
   neither is well-defined over a cyclic graph.
4. Check that overriding methods match their parent's signature **exactly** — class
   inheritance has no variance (`InvalidOverride`). Skipped if a cycle was found.
5. Flatten protocol method tables (so `extends` sees inherited methods), then check
   protocol-extension variance: parameters are contravariant, return types are
   covariant, and no inherited method may be silently dropped
   (`InvalidProtocolVariance`). This always runs, independent of the class hierarchy.
6. Flatten attribute and method tables top-down (parent → child) into
   `TypeInfo::flattened_methods`, so every later lookup is a single `HashMap` access
   instead of a walk up the chain. Skipped if a cycle was found.

### Pass 1.5 — Constructor parameter resolution

Bridges Pass 0 and Pass 2: an unannotated type-constructor parameter like `type T(x)`
can't be inferred from a body (types have no body), so this pass collects type
constraints from literal arguments at every `new T(...)` call site, resolves each
type's own parameters from those constraints (using the same unique-candidate logic as
function-parameter inference — zero candidates is `CannotInferType`, exactly one
resolves it, more than one is `AmbiguousInference`), and then propagates resolved
parameter types upward through `inherits Parent(args)` clauses whenever an argument is
a bare reference to one of the type's own (now-resolved) parameters. It processes types
leaves-first (the reverse of parent-before-child topological order) so that propagation
always flows from a concrete use site toward the root of the hierarchy.

### Pass 2 — Type inference

Builds the typed tree node by node in a single post-order traversal. This is a
**bounded** inferer, not a general fixed-point solver — by design, per the same
philosophy as the language spec: it tries hard, succeeds in all the common cases, and
fails predictably (`CannotInferType` / `AmbiguousInference`) rather than silently
guessing wrong. Two mechanisms make this work:

- **Constraint collection** for unannotated function/method parameters: every place a
  parameter is used (an arithmetic operator, a boolean operator, an argument position
  in another call) records a required type; once the body is fully walked, the
  candidates are deduplicated and resolved the same way as in Pass 1.5.
- **Placeholder-and-patch** for recursive functions: a self-recursive (or
  self-referential via `self.method()`) function's return type starts as
  `Type::Unknown` so its own body can be inferred without deadlocking; once the body's
  real type is known, a dedicated `patch_unknowns` tree rewrite backfills every
  parameter reference and recursive call site that was provisionally `Unknown`.

This pass also resolves `for`/comprehension element types uniformly across three
shapes — `Type::Vector(T)`, `Type::Iterable(T)`, and any `Type::Named` type that
structurally implements the builtin `Iterable` protocol (via the covariant return type
of its `current()` method) — and handles a `match` expression as a project extension
beyond the core spec, including a `NonExhaustiveMatch` warning when no pattern is a
wildcard or bare-variable catch-all.

### Pass 3 — Type checking

A read-only final sweep over the typed tree:

- Re-checks every explicit annotation (function/method parameters and return types,
  attributes, `let` bindings) against the type Pass 2 actually settled on.
- Enforces attribute privacy: a member access that resolves to an *attribute* (as
  opposed to a method) is only legal when the receiver is literally `self` **and** of
  the exact owning type — any other receiver shape, including a protocol- or
  subtype-narrowed view, is rejected as `UnknownMember`.
- Sweeps for any `Unknown` left in the tree, reported as `CannotInferType`. This is a
  safety net — Pass 2 already reports inference failures as they happen — rather than
  the primary detection point.

---

## Core types

### `Type` (`src/types/mod.rs`)

The resolved type of every expression and declared symbol:

```rust
enum Type {
    Number, String, Boolean,        // builtin value types
    Object,                         // root of the nominal hierarchy
    Named(String),                  // any user type or protocol — they share one namespace
    Vector(Box<Type>),              // T[] sugar / vector literals
    Iterable(Box<Type>),            // T* sugar / the builtin Iterable protocol, specialized
    Unknown,                        // inference placeholder — must never survive a successful analyze()
    Error,                          // poison value — suppresses cascading diagnostics
}
```

`Type::conforms_to(&self, other, registry) -> bool` implements the `<=` relation,
checked in priority order: reflexivity, everything conforms to `Object`, `Error` is
absorbing in either position (cascade suppression), `Unknown` conforms both ways
(inference placeholder), nominal ancestry for `Named` vs `Named`, and structural
protocol conformance (`registry.implements_protocol`) wherever the expected side names a
protocol. Anything else is `false` — HULK has no implicit numeric widening.

`lowest_common_ancestor(types, registry) -> Type` powers every multi-branch
construct (`if`/`elif`/`else`, vector literals, `match`): it propagates `Error`,
filters out `Unknown` (falling back to `Unknown` only if *everything* was `Unknown`),
and otherwise walks each type's ancestor chain up to `Object` to find the deepest
common node.

### `TypeRegistry` (`src/types/registry.rs`)

The global, read-mostly knowledge base — three maps (`types`, `protocols`,
`functions`), pre-populated by `seeded_registry()` with every HULK builtin before a
single user declaration is collected:

- **Types:** `Object` (root), `Number` / `String` / `Boolean` (flagged
  `is_builtin_value` so inheriting from them is rejected), `Range` (implements
  `Iterable` structurally via `current(): Number` / `next(): Boolean`).
- **Protocols:** `Iterable` (`next(): Boolean`, `current(): Object`), `Enumerable`
  (`iter(): Iterable<Object>`).
- **Functions:** `print`, `sqrt`, `sin`, `cos`, `exp`, `log`, `rand`, `range`, and the
  constants `PI` / `E` (modeled as zero-argument functions).

Key queries: `lookup_type` / `lookup_protocol` / `lookup_function`, `is_ancestor`,
`is_protocol`, `parent_of`, `implements_protocol` (the cheap boolean structural check),
and `protocol_conformance_details` (the diagnostic-grade version — returns
`Err(missing_methods)` so the checker can report exactly which methods are missing or
have an incompatible signature, instead of a generic mismatch).

### `Environment` (`src/environment.rs`)

A stack of scopes (`Vec<HashMap<String, Binding>>`) threaded through Pass 2 and Pass 3.
`push_scope`/`pop_scope` bracket every construct that introduces real lexical scope —
`let` bodies, function/method bodies, `for` bodies, `match` case bodies — while plain
`{ ... }` blocks deliberately do **not** push a scope, since they're pure sequencing.
Re-declaring a name in the *same* scope intentionally overwrites the previous binding
(`let a = 7, a = 7 * 6 in ...` is valid per the spec), and a `Binding` carries an
`is_self` flag so `self` can be looked up like any other name while still being
rejected as an assignment target.

### `SemanticError` / `SemanticErrorKind` / `Severity` (`src/error.rs`)

Every diagnostic is a `{ kind, span, severity }` triple, mirroring `hulk_parser::ParseError`
so the two phases render uniformly. `Severity::Error` blocks compilation;
`Severity::Warning` does not — `analyze` partitions the two automatically, returning
warnings inside `VerifiedProgram::warnings` on success and only hard errors in the `Err`
case. `SemanticErrorKind` groups into: name resolution, redeclaration, inheritance,
protocols & annotations, typing, inference, and non-blocking quality-of-life warnings
(`UnreachableDowncast`, `NonExhaustiveMatch`). Every variant has a `Display` impl
producing a one-line, human-readable message, e.g.:

```text
semantic error at line 4, col 17: type `T` does not implement protocol `P`; missing methods: f
```

### `TypedExpr` / `TypedProgram` (`src/typed.rs`)

Thin aliases — `hulk_ast::Expr<Type>` and `hulk_ast::Program<Type>` — over the same
generic AST the parser builds, just instantiated with `Type` as the annotation. This is
the shape codegen consumes: no separate typed-tree type, no duplicated node
definitions.

---

## Crate layout

```text
src/
├── lib.rs                            – public API, the analyze() pipeline
├── error.rs                          – SemanticError / SemanticErrorKind / Severity
├── environment.rs                    – lexical scoping (Environment, Binding)
├── typed.rs                          – TypedExpr / TypedProgram aliases
├── types/
│   ├── mod.rs                        – Type enum, conforms_to, lowest_common_ancestor
│   └── registry.rs                   – TypeRegistry, builtin seeding, protocol conformance
└── passes/
    ├── mod.rs                        – pass orchestration / re-exports
    ├── collect.rs                    – Pass 0: declaration collection
    ├── hierarchy.rs                  – Pass 1: inheritance & protocol resolution
    ├── resolve_constructor_params.rs – Pass 1.5: constructor parameter inference
    ├── infer.rs                      – Pass 2: type inference
    ├── check.rs                      – Pass 3: type checking
    └── utils.rs                      – shared helpers (topological sort, test asserts)
```

---

## Testing

Every module carries its own `#[cfg(test)]` suite, from low-level unit tests (e.g.
`Environment` shadowing rules, `TypeRegistry::implements_protocol` variance edge cases)
up to whole-program integration tests that lex, parse, and `analyze` real HULK source.
`hulk-lexer` and `hulk-parser` are dev-dependencies for exactly this reason — they're
never needed outside of tests.

```sh
cargo test -p hulk-semantic
```

The crate also enables `#![deny(missing_docs)]`: every public item must carry a doc
comment, which `cargo doc -p hulk-semantic --open` will render directly.

---

## Known limitations

- **No functors or lambda expressions.** `hulk-ast`'s `ExprKind` has no `Lambda`
  variant, and there's no `(T) -> R` arrow-type syntax in `parse_type_ref` — this is a
  cross-crate gap (the lexer already has the `Arrow`/`FatArrow` tokens; parser and AST
  changes are what's missing).
- **Mutual recursion across functions that both lack explicit return-type
  annotations is unresolved.** Each function's `Unknown` placeholder return type can't
  be observed by the other while both are mid-inference, so this currently produces a
  deterministic `CannotInferType` rather than a correct inference. This is documented,
  intentional behavior (see the `mutual_recursion_two_functions` test) rather than a
  silent bug, but it's a real expressiveness gap relative to the spec's general
  inference intent.
