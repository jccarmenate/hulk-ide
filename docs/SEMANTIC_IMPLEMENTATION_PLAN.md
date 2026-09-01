# Semantic Analysis Implementation Plan — `hulk-semantic`

**Status:** Planning document.

**Target crate:** `crates/hulk-semantic`

**Depends on:** `hulk-ast` (AST types produced by `hulk-parser`)

**Also touches:** `hulk-ast` — a small, mechanical, backward-compatible
refactor (Section 5.1) that turns the existing tree into a tree
*parametrized* over an annotation type, so that the typed output of this
plan can live in the same tree shape instead of a duplicated one.

**Consumed by:** `hulk-codegen` (future), `hulk-cli` (diagnostics)

**Companion reference:** `hulk-docs.pdf`, Appendix A ("The HULK Programming Language")

---

## 0. Purpose of this Document

This document is the implementation plan for the **semantic analysis** (a pass that 
decides whether a syntactically valid program is also meaningful). It:

1. Explains, from first principles, *why* a dedicated semantic phase is
   needed and how it should be structured, grounding every design decision
   in the theory presented in the supplied lecture material
   (`10-semantic.pdf`, `11-attr-gram.pdf`, `12-types.pdf`).
2. Cross-references every semantic rule against the formal HULK language
   definition in `hulk-docs.pdf`, Appendix A, so that the rules implemented
   are traceable to a specific section of the specification rather than to
   assumption.
3. Lays out the concrete infrastructure to be built inside `hulk-semantic`
   — modules, data structures, and the public API — and describes the
   responsibility of every component.
4. Defines a phased delivery roadmap consistent with the recommendations
   already present in `PRE-SEMANTIC_STATUS.md`.
5. Specifies the one small upstream change this plan requires of
   `hulk-ast` itself (Section 5.1) — a mechanical, backward-compatible
   refactor that lets the analyzer's typed output reuse the existing tree
   shape instead of duplicating it, so that `hulk-codegen` receives a
   single, compile-time-verified tree rather than a second parallel one.

This is a planning artifact, not a final design. Rust signatures shown below
are illustrative of intended shape and responsibilities; they are expected
to be refined during implementation.

---

## 1. Position in the Compiler Pipeline

```
Source Code
    │
    ▼
┌─────────────┐
│ hulk-lexer  │  Vec<Token>                              (Complete)
└──────┬──────┘
       ▼
┌─────────────┐
│ hulk-parser │  hulk_ast::Program                       (Complete)
└──────┬──────┘
       ▼
┌───────────────────────────────────────────────────────────────────────┐
│                         hulk-semantic  (THIS PLAN)                    │
│                                                                       │
│   Program ──▶ [Pass 0: Collect] ──▶ [Pass 1: Hierarchy] ──▶          │
│            [Pass 2: Inference] ──▶ [Pass 3: Checking] ──▶            │
│            VerifiedProgram  |  Vec<SemanticError>                    │
└──────────────────────────────────┬────────────────────────────────────┘
                                    ▼
                          ┌──────────────────┐
                          │   hulk-codegen   │   (Not implemented)
                          └──────────────────┘
```

Semantic analysis sits strictly between parsing and code generation. It
consumes the *syntax* (the `Program` AST, which is purely structural and
already free of grammar-only nodes — parentheses, separators, etc., per the
design rule stated at the top of `hulk-ast/lib.rs`) and produces a
*verified, typed* program that code generation can trust unconditionally:
no undefined symbol, no type mismatch, and no broken inheritance link will
ever reach `hulk-codegen`.

`Program` in this diagram, and everywhere else in this document unless an
annotation is written explicitly, means `Program<()>` — the untyped tree
`hulk-parser` already produces today. Section 5.1 explains why `Program`
remains a valid, unchanged way to spell that type even after `hulk-ast` is
refactored to be generic, and how `VerifiedProgram` carries `Program<Type>`,
the *same* tree shape with a different annotation, rather than a second,
hand-duplicated tree.

---

## 2. Theoretical Foundations

This plan is deliberately built on the conceptual framework provided by the
Compilation course. This section makes that mapping explicit, because every
later design decision is a direct consequence of one of these ideas.

### 2.1 Why a separate phase is required (`10-semantic.pdf`)

The lecture's opening argument is the foundation of this entire plan: a
programming language is *a set of predicates over strings*, and not all of
those predicates are context-free. Rules such as "a variable must be
declared before use" or "a function call must supply the right number of
arguments" are **context-dependent**: they cannot be checked by a
context-free grammar, no matter how the grammar is engineered (the deck's
own example, $L = a^n b^n$, shows that even simple counting constraints
already escape CFG expressiveness). HULK's analogous rules — "a variable
must be defined before use", "no two functions may share a name", "operator
operands must have compatible types" — are exactly this class of rule.

This is precisely why `hulk-parser` cannot and should not try to enforce
them: the grammar in `GRAMMAR_LL1.md` defines the **syntactic** rules of
HULK only. `hulk-semantic` is the component responsible for the
**semantic** rules: the dependent-on-context part of the language.

### 2.2 The AST as the substrate, and the analyzer as an attribute evaluator (`10-semantic.pdf`, `11-attr-gram.pdf`)

The first lecture distinguishes a *concrete* syntax tree (the parse tree of
non-terminals, including parentheses and single-child chains) from an
*abstract* syntax tree, which keeps a node type per semantic function
(literals, expressions, declarations, invocations, …) and discards
everything else. `hulk-ast` already *is* this AST — the crate's own
documentation states the design rule explicitly ("the AST keeps semantic
information; grammar-only helper productions are intentionally omitted").
This means `hulk-semantic` does not need to build any new tree from
scratch: it operates directly on `hulk_ast::Program`.

The second lecture (`11-attr-gram.pdf`) gives the formal vocabulary for
*how* to compute semantic information over a tree: **attribute grammars**.
Every node carries attributes; **synthesized** attributes are computed
bottom-up from children, **inherited** attributes are computed top-down
from the parent and siblings. A grammar is **evaluable** if its attribute
dependency graph is a DAG, and the lecture singles out two practically
important sub-classes:

* **S-attributed** grammars (synthesized attributes only) can be evaluated
  in a single bottom-up pass — exactly the shape of an LR/bottom-up parser
  action.
* **L-attributed** grammars (inherited attributes only depend on the
  parent and *already-evaluated* left siblings) can be evaluated in a
  single left-to-right, top-down pass — exactly the shape of our
  hand-written LL(1) recursive-descent parser, and, crucially, of a
  recursive-descent **AST visitor**.

`hulk-semantic` is designed as **an L-attributed evaluator over the
already-built AST**, executed as a small number of explicit visitor passes
(Section 6) rather than as one pass tangled into parsing — the slides
explicitly warn that evaluating semantic rules *during* LL(1) parsing is
"horribly tortuous" (see the conclusion of `11-attr-gram.pdf`, slide 19/19)
and recommend keeping it for when an LR/bottom-up strategy is used. Since
the rest of the pipeline is already split into independent crates, the
correct design is a dedicated post-parsing pass:

* **Inherited attributes** become the *environment* threaded top-down
  through the visitor: the current scope's symbol table, the expected
  type at a position (e.g. the annotated type of a `let` binding, used to
  check the initializer), and the current type's `self` type inside a
  method body.
* **Synthesized attributes** become the *inferred type* returned bottom-up
  from each expression visit — directly mirroring the example evaluator in
  `11-attr-gram.pdf` (`E -> T X { X.val = T.exp, E.exp = X.exp }`), except
  that here the synthesized value is a `Type` instead of a `f64`.

This single design choice — environment passed down, type returned up —
is the structural backbone of every pass described in Section 6.

### 2.3 Context objects as inherited attributes (`10-semantic.pdf`)

`10-semantic.pdf`'s worked example (the `IContext` interface with
`IsDefined`, `Define`, and `CreateChildContext`) is the direct ancestor of
the `Environment`/`Scope` design in Section 5.4. The "reversible"
modification of context required for `DefFunc` — create a child context,
populate it with parameters, validate the body against it, then discard it
— is exactly the push/pop scope discipline this plan adopts for `let`,
function/method bodies, `for` bindings, and (as an extension) `match`
case bindings.

### 2.4 The HULK type system (`12-types.pdf`, cross-referenced with `hulk-docs.pdf` §A.7–A.9)

`12-types.pdf` gives the minimal vocabulary this plan needs: a type
describes possible values, valid operations, and memory representation; a
type *system* is the set of rules for compatibility, implicit conversion,
and operator typing; and the basic arithmetic/comparison/boolean operator
table is exactly the one HULK uses (see Section 7.7). `hulk-docs.pdf`
extends this with HULK's specific rules — nominal typing with single
inheritance rooted at `Object`, the **conforming** relation `<=`
(§A.8.4), structural typing via **protocols** (§A.10), and an explicit,
intentionally under-specified **type inference** contract (§A.9) that this
plan implements as a deliberately bounded, *sound* strategy (Section 8.4).

---

## 3. Scope: What Semantic Analysis Covers *Today*

`hulk-semantic` must analyze exactly what `hulk-ast` and `hulk-parser` can
produce — no more, no less. The table below cross-references the language
feature matrix in `PRE-SEMANTIC_STATUS.md` against the actual AST/parser
code, because two corrections are needed relative to that table:

| HULK Feature (hulk-docs §) | Lexer/Parser/AST reality | Semantic plan phase |
|---|---|---|
| Arithmetic / string / boolean expr (A.2) | ✅ | Phase 1 |
| `let` / scoping / `:=` (A.4) | ✅ | Phase 1 |
| `if`/`elif`/`else`, `while`, `for` (A.5, A.6) | ✅ | Phase 1 |
| Functions, inline & full-form (A.3) | ✅ | Phase 1 |
| Types, attributes, methods, `self` (A.7) | ✅ | Phase 2 |
| Inheritance, `base`, polymorphism (A.7.3–4) | ✅ | Phase 2 |
| `is` / `as` (A.8.5–6) | ✅ (`TypeTestExpr`, `DowncastExpr`) | Phase 2 |
| Protocols (A.10) | ✅ AST (`ProtocolDecl`) | Phase 3 |
| Vector literals & comprehension, indexing (A.12) | ✅ | Phase 3 |
| **`T*` / `T[]` annotation sugar (A.11.2, A.12.3)** | ✅ **already supported** — `hulk-parser::parse_type_ref` rewrites `T*` → `TypeRef::with_args("Iterable", [T])` and `T[]` → `TypeRef::with_args("Vector", [T])`. | Phase 3 |
| `match` expression (project extension, not in hulk-docs) | ✅ AST (`MatchExpr`, `Pattern`) | Phase 4 (extension) |
| Functor sugar `(T) -> R` (A.13.3) | ❌ no `Arrow` consumption in parser | Out of scope — requires parser work first |
| Macros: `def`, `@symbol`, `$symbol`, structural pattern matching (A.14) | ❌ `Def`/`Match`/`Case` tokens exist, but macro *bodies*, symbolic/placeholder arguments, and compile-time AST rewriting are unimplemented | Out of scope — requires parser + an expansion pass before semantic analysis can even run on macro-using code |
| Generic-looking type arguments `Type<Arg>` (A.10/A.9.5 protocol synthesis) | ⚠️ Parsed structurally (`TypeRef.args`), but HULK itself has no user-facing generics outside `Iterable<T>`/`Vector<T>` sugar | Phase 3, restricted to the two builtin parametric types |

This plan therefore targets **Phases 1–3 as the concrete deliverable**, with
Phase 4 (the `match` extension) as an explicitly scoped add-on, and the
general protocol-synthesis inference strategy of §A.9.5 documented as
**future work** (Section 8.4.3) rather than committed work, exactly as
`PRE-SEMANTIC_STATUS.md` already does for code generation.

---

## 4. Crate Layout

```
crates/hulk-semantic/
├── Cargo.toml
└── src/
    ├── lib.rs              Public API: `analyze`, `VerifiedProgram`, re-exports
    ├── error.rs            SemanticError, SemanticErrorKind, Display
    ├── environment.rs      Environment (scope stack) and Binding
    ├── typed.rs            Type aliases over hulk-ast's parametrized tree:
    │                       `TypedExpr = hulk_ast::Expr<Type>`,
    │                       `TypedProgram = hulk_ast::Program<Type>`, etc.
    ├── types/
    │   ├── mod.rs          `Type`, `conforms_to`, `lowest_common_ancestor`
    │   └── registry.rs     TypeRegistry, TypeInfo, MethodSignature, builtins
    └── passes/
        ├── mod.rs          Pass orchestration
        ├── collect.rs      Pass 0 — declaration collection
        ├── hierarchy.rs    Pass 1 — inheritance & protocol resolution
        ├── infer.rs        Pass 2 — type inference; builds `TypedProgram`
        └── check.rs        Pass 3 — type checking over `TypedProgram`
```

There is no second tree definition because `hulk-ast`'s
node types are generic over an annotation parameter (Section 5.1),
`hulk-semantic` only ever needs to *name* an instantiation of that tree
(`Expr<Type>`), never re-declare its shape.

`Cargo.toml` for the crate needs exactly one path dependency, matching the
pattern already used by `hulk-parser`:

```toml
[package]
name = "hulk-semantic"
version.workspace = true
edition.workspace = true

[dependencies]
hulk-ast = { path = "../hulk-ast" }
```

No dependency on `hulk-lexer` is required: `SourceSpan` (used for
diagnostics) is already re-exported from `hulk-ast`, and `hulk-semantic`
never sees a token stream.

---

## 5. Core Data Structures

### 5.1 Parametrized AST in `hulk-ast` — the prerequisite refactor

Section 5.6 below needs a place to put the analyzer's output: a tree that
looks exactly like `Program`, except that every node also carries its
resolved `Type`. There are three ways a compiler can do this, and the
choice matters enough to justify against how mature compilers actually
make it, rather than by convention:

| Strategy | What it means here | Used by |
|---|---|---|
| **Separate parallel tree** | Hand-write a second tree (`tast.rs`) mirroring every `hulk_ast::ExprKind` variant, plus a `ty: Type` field | Rejected — see below |
| **In-place mutation** | Add a mutable `Option<Type>` field to the existing nodes, filled in during analysis | `javac` — pragmatic, but reintroduces `.unwrap()`/"hope the analyzer ran first" |
| **Parametrized ("Trees That Grow") tree** | Make the existing tree generic over an annotation type `A`; the parser instantiates `A = ()`, the analyzer instantiates `A = Type` | rustc (`HIR<'tcx>`/`TyCtxt` side tables), GHC (`HsSyn` → typed `Tree id`), Scala (`Tree[Type]`), Go (immutable `ast` + side `Info` table) |

The first option — a hand-duplicated `tast.rs` — is the *least* standard
of the three in production compilers, and it has a concrete, recurring
cost: every time a node kind is added to `hulk-ast` (and the language is
still being implemented against `hulk-docs.pdf`, so this will happen),
the duplicate tree must be updated in lockstep by hand, with no compiler
help if someone forgets. The second option avoids duplication but
reintroduces exactly the class of bug semantic analysis exists to rule
out: a node whose `Option<Type>` is still `None` when `hulk-codegen` reads
it, discoverable only at runtime. This plan adopts the third option,
because it gives the same zero-duplication property as in-place mutation
while keeping the compile-time guarantee that a "verified" tree cannot
contain an unresolved type — and because it is the pattern the surveyed
production compilers actually converge on.

#### **The change to `hulk-ast`.** 
Every node type that is, or transitively
contains, an `Expr` gains a generic annotation parameter `A`, defaulted to
`()` so that every existing call site — the parser, `hulk-ast`'s own
tests, and this document's own earlier references to plain `Program` and
`Expr` — keeps compiling unchanged:

```rust
// hulk-ast: illustrative shape of the change, not a verbatim diff —
// the exact current field layout of each struct is the implementer's
// source of truth in hulk-ast/src/*.rs.

pub struct Expr<A = ()> {
    pub kind: ExprKind<A>,
    pub anno: A,        // (): untyped syntax; Type: verified, fully typed
    pub span: SourceSpan,
}

pub enum ExprKind<A = ()> {
    Literal(Literal),
    Variable(String),
    SelfRef,
    BaseRef,
    Unary(Box<UnaryExpr<A>>),
    Binary(Box<BinaryExpr<A>>),
    Let(Box<LetExpr<A>>),
    Assign(Box<AssignExpr<A>>),
    If(Box<IfExpr<A>>),
    While(Box<WhileExpr<A>>),
    For(Box<ForExpr<A>>),
    New(Box<NewExpr<A>>),
    Member(Box<MemberExpr<A>>),
    Vector(Box<VectorExpr<A>>),
    Index(Box<IndexExpr<A>>),
    TypeTest(Box<TypeTestExpr<A>>),
    Downcast(Box<DowncastExpr<A>>),
    Match(Box<MatchExpr<A>>),
    // every other variant follows the same shape
}

// Every struct nested under Expr — UnaryExpr, BinaryExpr, LetExpr,
// LetBinding, AssignExpr, IfExpr, ElifBranch, WhileExpr, ForExpr,
// NewExpr, MemberExpr, VectorExpr, IndexExpr, TypeTestExpr,
// DowncastExpr, MatchExpr, MatchCase, Pattern::Type — becomes generic
// over A the same way, recursively, down to the leaves.

// Declaration-level nodes that contain a body or initializer follow too,
// since they transitively contain Expr<A>:
pub enum Declaration<A = ()> {
    Function(FunctionDecl<A>),
    Type(TypeDecl<A>),
    Protocol(ProtocolDecl),   // no body/initializer: not generic
}

pub struct Program<A = ()> {
    pub declarations: Vec<Declaration<A>>,
    pub entry: Expr<A>,
}

// Existing constructors keep their call sites unchanged: the `A`
// parameter defaults to `()`, so `Expr::number(value, span)` still
// produces an `Expr<()>` without the caller writing anything new.
impl Expr {
    pub fn number(value: f64, span: SourceSpan) -> Self {
        Self { kind: ExprKind::Literal(Literal::Number(value)), anno: (), span }
    }
    // ... every other existing constructor, unchanged at the call site
}
```

**Why this is safe to do without disturbing the certified frontend.**

1. **No call site changes.** Because `A` defaults to `()`, `hulk_ast::Program`
   and `hulk_ast::Expr` continue to mean exactly what they meant in
   `PRE-SEMANTIC_STATUS.md`: the untyped tree the parser produces. Every
   existing reference to those names elsewhere in *this* document — the
   pipeline diagram (Section 1), `ParentLink.args: Vec<hulk_ast::Expr>`
   (Section 5.3) — is unaffected and still denotes `Expr<()>`.
2. **`hulk-parser`'s tests are unaffected.** They construct and inspect
   `Program<()>` exactly as before; nothing about parsing or the existing
   parser test suite needs to change.
3. **Zero duplication, forever.** `ExprKind<A>` is defined once. Adding a
   future node kind (e.g. for functor sugar or macros, Section 3) means
   updating one enum, and the compiler forces every match on it — in
   `hulk-parser`, `hulk-semantic`, and eventually `hulk-codegen` — to
   handle the new case. A hand-duplicated `tast.rs` could silently drift
   out of sync with no such guarantee.
4. **The invariant is type-checked, not asserted.** A function that takes
   `&Expr<Type>` is *proven*, at compile time, to be working with a fully
   typed node — no `Option<Type>`, no `.unwrap()`, no "trust that Pass 2
   already ran." A function that takes `&Expr<()>` (or is generic over
   `A`, for code that genuinely doesn't care, such as a span-only
   traversal) is equally explicit about what it does and does not assume.
5. **Extensible to future passes.** If a later phase needs to attach more
   than a `Type` — e.g. a name-resolution pass run before type checking —
   it can target `Expr<(Resolved, Type)>` or a small dedicated struct,
   without touching the tree's structure again.

**The one-time cost.** Every struct under `Expr`/`Declaration`/`Program`
becomes generic (mechanical, no logic change); existing constructors gain
no new required argument because of the `= ()` default. For an AST of
this size, this is on the order of a couple of hours of mechanical,
low-risk editing in `hulk-ast` — paid once, with no further duplication
cost for the rest of the project's life. The authoritative list of which
`hulk-ast` structs need the parameter is the crate's own source tree at
implementation time.

---

### 5.2 `Type` — the synthesized attribute

```rust
/// A fully-resolved HULK type, as computed by the semantic analyzer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    /// The three builtin value types.
    Number,
    String,
    Boolean,
    /// Root of the nominal hierarchy; every type conforms to `Object`.
    Object,
    /// A user-defined `type` or `protocol`, resolved by name in the
    /// `TypeRegistry`. Both classes and protocols share one namespace
    /// (hulk-docs §A.10.2: "anywhere you can annotate ... you can also
    /// use a protocol").
    Named(String),
    /// `Vector<T>` — the `T[]` annotation sugar (§A.12.3) and the type of
    /// vector literals / comprehensions (§A.12).
    Vector(Box<Type>),
    /// `Iterable<T>` — the `T*` annotation sugar (§A.11.2) and the
    /// builtin `Iterable` protocol specialized to element type `T`.
    Iterable(Box<Type>),
    /// Placeholder used internally while a symbol's type is still being
    /// inferred (e.g. a self-recursive function). Never appears in a
    /// successfully verified program.
    Unknown,
    /// Poison value produced after a type error has already been
    /// reported for this expression, so that the error does not cascade
    /// into a flood of unrelated follow-up errors.
    Error,
}
```

`Type` intentionally has **no lifetime and no reference into the AST**: it
is a self-contained value so that it can be returned up the tree as a
synthesized attribute, stored in maps, and compared cheaply.

### 5.3 `TypeRegistry` — global, context-independent knowledge

The registry is built once (Pass 0/1) and is read-only afterwards. It
plays the role of the *global scope* in the `IContext` design from
`10-semantic.pdf`, but specialized for types rather than generic
"defined/undefined" checks, since HULK separates the function namespace
from the type/protocol namespace (hulk-docs never allows a function and a
type to share a name-resolution context: functions are invoked as
`f(...)`, types via `new T(...)`, and protocol/type names only ever appear
in annotation position).

```rust
pub struct TypeRegistry {
    types: HashMap<String, TypeInfo>,
    protocols: HashMap<String, ProtocolInfo>,
    functions: HashMap<String, FunctionSignature>,
}

pub struct TypeInfo {
    pub name: String,
    pub params: Vec<(String, Type)>,        // type constructor parameters
    pub parent: Option<ParentLink>,          // None => implicit Object
    pub attributes: HashMap<String, AttributeInfo>,
    pub methods: HashMap<String, MethodSignature>,
    pub span: SourceSpan,
}

pub struct ParentLink {
    pub name: String,
    pub args: Vec<hulk_ast::Expr>,           // untyped (Expr<()>) constructor-argument
                                              // expressions, as collected in Pass 0,
                                              // before Pass 2 builds the typed tree
}

pub struct AttributeInfo {
    pub declared_type: Option<Type>,         // None until inferred (Pass 2)
    pub span: SourceSpan,
}

pub struct MethodSignature {
    pub params: Vec<(String, Type)>,
    pub return_type: Type,
    pub defined_in: String,                  // owning type, for `base` resolution
    pub span: SourceSpan,
}

pub struct ProtocolInfo {
    pub name: String,
    pub extends: Vec<String>,
    pub methods: HashMap<String, MethodSignature>,
    pub span: SourceSpan,
}

pub struct FunctionSignature {
    pub params: Vec<(String, Type)>,
    pub return_type: Type,
    pub span: SourceSpan,
}
```

The registry is **pre-seeded with builtins** before Pass 0 looks at the
user's declarations, so that user code can reference them without special
casing in the checker:

| Builtin | Kind | Signature (hulk-docs §) |
|---|---|---|
| `Object` | type | root of the hierarchy; no public members (§A.7.3) |
| `Number`, `String`, `Boolean` | type | implicitly inherit `Object`; cannot be inherited from (§A.7.3) |
| `Iterable` | protocol | `next(): Boolean`, `current(): Object` (§A.11) |
| `Enumerable` | protocol | `iter(): Iterable` (§A.11.3) |
| `Range` | type | `Range(min: Number, max: Number)`, implements `Iterable` with `current(): Number` (§A.11, covariant return) |
| `print(x: Object): Object` | function | (§A.2) — accepts and returns any value; the registry models `print` as generic-over-`Object` by accepting any conforming argument |
| `sqrt(x: Number): Number` | function | §A.2.3 |
| `sin(x: Number): Number` | function | §A.2.3 |
| `cos(x: Number): Number` | function | §A.2.3 |
| `exp(x: Number): Number` | function | §A.2.3 |
| `log(base: Number, x: Number): Number` | function | §A.2.3 |
| `rand(): Number` | function | §A.2.3 |
| `range(min: Number, max: Number): Range` | function | §A.6.2, §A.11 |
| `PI: Number`, `E: Number` | constant | §A.2.3, modeled as zero-argument functions or pre-bound global symbols |

### 5.4 `Environment` — the inherited attribute (scopes)

```rust
pub struct Environment {
    scopes: Vec<HashMap<String, Binding>>,
}

pub struct Binding {
    pub ty: Type,
    pub span: SourceSpan,
}

impl Environment {
    pub fn push_scope(&mut self) { self.scopes.push(HashMap::new()); }
    pub fn pop_scope(&mut self)  { self.scopes.pop(); }

    /// Declares `name` in the *innermost* scope. Per hulk-docs §A.4.5,
    /// shadowing an outer binding is always legal; only a duplicate
    /// *within the same `let` clause list* or the same parameter list is
    /// rejected (see Section 7.4 and 7.5).
    pub fn declare(&mut self, name: &str, ty: Type, span: SourceSpan);

    /// Looks up `name` starting from the innermost scope outward,
    /// implementing lexical shadowing exactly as described in §A.4.5.
    pub fn lookup(&self, name: &str) -> Option<&Binding>;
}
```

A plain expression block (`{ ... }`) does **not** push a new scope: per
§A.4, the only constructs that introduce bindings are `let`, function and
method parameter lists, `for` loop variables, and (Phase 4) `match` case
bindings. Blocks are pure sequencing and must not be modeled as scopes, or
shadowing tests derived directly from the spec's own examples (§A.4.5)
would fail.

### 5.5 `SemanticError` — diagnostics

Modeled directly on `hulk_parser::ParseError`'s `{ kind, span }` shape, so
that `hulk-cli` can report both phases with one rendering code path.

```rust
pub struct SemanticError {
    pub kind: SemanticErrorKind,
    pub span: SourceSpan,
}

pub enum SemanticErrorKind {
    // Name resolution
    UndefinedVariable(String),
    UndefinedFunction { name: String, arity: usize },
    UndefinedType(String),
    UnknownMember { ty: Type, member: String },

    // Redeclaration (global namespace rules, §A.3.1, §A.7)
    DuplicateFunction(String),
    DuplicateType(String),
    DuplicateAttribute { ty: String, attribute: String },
    DuplicateParameter(String),

    // Inheritance (§A.7.3)
    InheritFromBuiltinValueType(String),
    InheritFromUndefinedType(String),
    InheritanceCycle(Vec<String>),
    InvalidOverride { method: String, in_type: String, expected: String, found: String },

    // Protocols (§A.10.3–4)
    ProtocolNotImplemented { ty: Type, protocol: String, missing: Vec<String> },
    InvalidProtocolVariance { method: String, reason: String },

    // Typing
    TypeMismatch { expected: Type, found: Type },
    NotConforming { found: Type, expected: Type },
    ArityMismatch { expected: usize, found: usize },
    InvalidOperator { op: String, operand_types: Vec<Type> },
    NonBooleanCondition(Type),
    NotIterable(Type),
    IndexOnNonVector(Type),
    InvalidAssignTarget,
    SelfIsNotAssignable,
    BaseOutsideOverridingMethod,

    // Inference (§A.9)
    CannotInferType { symbol: String },
    AmbiguousInference { symbol: String, candidates: Vec<Type> },
}
```

### 5.6 `VerifiedProgram` — the analyzer's output

`PRE-SEMANTIC_STATUS.md` already specifies, in its "Contact Points for
Future Module Implementers" section, the intended public entry point:

```rust
hulk_semantic::analyze(program: &Program) -> Result<VerifiedProgram, SemanticError>
```

This plan adopts that name and refines the error side to a `Vec`, in line
with the project's own "Task 1.5: Error Reporting" goal ("multiple error
reporting, not just first"):

```rust
pub fn analyze(program: &hulk_ast::Program) -> Result<VerifiedProgram, Vec<SemanticError>>;

/// Convenience aliases — instantiations of hulk-ast's parametrized tree
/// (Section 5.1), not new struct definitions.
pub type TypedExpr = hulk_ast::Expr<Type>;
pub type TypedProgram = hulk_ast::Program<Type>;

pub struct VerifiedProgram {
    /// Resolved global knowledge: every type, protocol, and function
    /// signature, fully checked and ready for code generation's object
    /// layout and v-table construction (Phase 2 of
    /// PRE-SEMANTIC_STATUS.md's codegen roadmap).
    pub registry: TypeRegistry,
    /// The *same* tree shape `hulk-ast` already defines, instantiated
    /// with `Type` as its annotation parameter instead of `()`: every
    /// expression and every declared symbol carries its resolved `Type`,
    /// and the type system guarantees that field is always present —
    /// there is no `Option<Type>` to unwrap and no separate tree to keep
    /// in sync.
    pub typed_program: TypedProgram,
}
```

As established in Section 5.1, `hulk-ast` is refactored — but only in the
sense that its node types gain an annotation parameter `A` defaulting to
`()`. No field is added to any existing variant, no existing constructor
signature changes its *call site*, and the parser's output type, written
as plain `Program`, is unaffected: it is, and remains, `Program<()>`. The
frontend behavior is preserved exactly. The analyzer gives `hulk-codegen`
an expected annotated structure (`hulk_codegen::generate(verified_program: &VerifiedProgram)`),
with the added guarantee that the type system itself — not a code review or
a runtime check — enforces that every node `hulk-codegen` reads off `typed_program`
has a real, resolved `Type`.

---

## 6. The Analysis Pipeline

Each pass is a complete traversal of the program; splitting the work into
passes (rather than one monolithic visitor) exists to solve the same
problem the attribute-grammar lecture flags repeatedly: some attributes
cannot be computed until *other* attributes, possibly belonging to nodes
that appear later in the source, are already known. HULK's own rule that
"the body of any function can use other functions, regardless of whether
they are defined before or after" (§A.3.1) is a textbook forward-reference
problem, solved the same way `10-semantic.pdf`'s own examples solve it:
collect signatures first, check bodies second.

### 6.1 Pass 0 — Declaration Collection (`passes/collect.rs`)

**Responsibility:** populate the `TypeRegistry` with every global
function, type, and protocol *signature*, without looking at any function
body, method body, or attribute initializer yet.

For each `Declaration` in `Program.declarations`:

* `FunctionDecl` → register a `FunctionSignature`. Reject if the name
  already exists in the function namespace, or collides with a builtin
  function name (`DuplicateFunction`) — HULK has no overloading (§A.3.1:
  *"there are no overloads in HULK"*).
* `TypeDecl` → register a `TypeInfo` with its constructor parameters
  (types not yet resolved — see Pass 1) and an entry per member. Reject
  duplicate type names, duplicate attribute names, duplicate method names
  within the same type, and duplicate parameter names within the same
  constructor/method parameter list.
* `ProtocolDecl` → register a `ProtocolInfo`. Per §A.10.1, *"all method
  declarations in protocol definitions must be fully typed"* — this is
  checked here, immediately, since protocol methods have no body from
  which a type could ever be inferred.

This pass never reports a *type error*; it only reports *shape errors*
(duplicates, missing required annotations on protocol methods). It exists
purely so that every later pass can assume the full set of global names is
already visible, regardless of declaration order.

### 6.2 Pass 1 — Hierarchy & Protocol Resolution (`passes/hierarchy.rs`)

**Responsibility:** resolve every `inherits` and `extends` link collected
in Pass 0 into a usable tree, and validate it.

1. **Parent existence.** For each `TypeDecl` with a parent, verify the
   parent name resolves to a registered type. `UndefinedType` /
   `InheritFromUndefinedType` otherwise.
2. **No inheriting from value types.** Per §A.7.3: *"it is a semantic
   error to inherit from [`Number`, `String`, `Boolean`]"* —
   `InheritFromBuiltinValueType`.
3. **Cycle detection.** Walk each type's parent chain with a visited-set;
   a repeated type indicates `InheritanceCycle`. (HULK's grammar already
   guarantees single inheritance — `TypeParent` is a single optional
   field, not a list — so the only possible cycle shape is a simple
   chain, which keeps this check linear.)
4. **Override signature compatibility.** Per §A.7.4, an overriding method
   *must use the exact same signature* as the parent's (this is a
   stronger rule than protocol variance — §A.10.3's contravariant
   argument / covariant return relaxation applies **only** to protocol
   conformance, not to class-to-class overriding). Any mismatch is
   `InvalidOverride`.
5. **Protocol extension validity.** Per §A.10.1, an `extends`ed protocol
   may only *add* methods or *narrow* an inherited method's signature
   within variance rules (contravariant params, covariant return); it may
   never remove a method. Violations are `InvalidProtocolVariance`.
6. **Flatten attribute/method tables.** For each type, compute the full
   inherited attribute and method set (own members override inherited
   ones of the same name), so that Pass 3's member lookups are O(1) and
   so that `hulk-codegen`'s eventual `.TYPES` flattening can reuse this
   exact structure.

At the end of this pass, `TypeRegistry` contains a fully linked,
acyclic, single-inheritance tree rooted at `Object`, and the **conforming
relation** (`Type::conforms_to`, Section 7.6) becomes well-defined.

### 6.3 Pass 2 — Type Inference (`passes/infer.rs`)

**Responsibility:** assign a concrete `Type` to every expression and to
every symbol declaration that was not explicitly annotated, implementing
the *basic sound inference strategy* described in Section 8.4.

This pass is the synthesized-attribute evaluator described in Section
2.2: a post-order traversal of every function body, method body, and
attribute initializer over the input `Program<()>`, computing a `Type`
for each `Expr` and *constructing the corresponding node of the typed
tree* (`Program<Type>`, Section 5.1) as it returns — attaching the
computed `Type` to that node's `anno` field rather than mutating anything
in place. Because `anno: A` is a required field, not an `Option<Type>`,
a node that could not be soundly typed still needs a value to put there:
this pass uses `Type::Error` (Section 5.2) as that value, and queues the
corresponding `CannotInferType` diagnostic rather than panicking —
inference failures must not abort analysis of the rest of the program, to
satisfy the "report multiple errors" goal. Unannotated `let` bindings,
function parameters/returns, and attributes receive their inferred type
the same way.

### 6.4 Pass 3 — Type Checking (`passes/check.rs`)

**Responsibility:** now that every node of the `Program<Type>` tree built
by Pass 2 carries a concrete `Type` in its `anno` field, verify
*consistency*: every annotated type conforms to its inferred
initializer/body type, every operator is applied to compatible operand
types, every call has the right arity and conforming argument types,
every member access targets an existing, accessible member, and every
assignment target is valid. This pass only reads `&TypedProgram` and
`&TypeRegistry` — it builds nothing further. This is where the bulk of
`SemanticErrorKind` variants are actually raised; the rules are enumerated
construct-by-construct in Section 7.

### 6.5 Driver (`lib.rs`)

```rust
pub fn analyze(program: &Program) -> Result<VerifiedProgram, Vec<SemanticError>> {
    let mut errors = Vec::new();
    let mut registry = builtins::seeded_registry();

    passes::collect::run(program, &mut registry, &mut errors);
    passes::hierarchy::run(&mut registry, &mut errors);
    // Stop before inference/checking if the registry itself is broken
    // (undefined parents, cycles): every later pass assumes a sound
    // hierarchy and would otherwise produce a wall of secondary noise.
    if !errors.is_empty() {
        return Err(errors);
    }

    // `program` here is `&Program<()>`; `typed_program` is the same tree
    // shape instantiated as `Program<Type>` (Section 5.1) — built fresh
    // by Pass 2, not mutated from `program`.
    let typed_program: TypedProgram = passes::infer::run(program, &mut registry, &mut errors);
    passes::check::run(&typed_program, &registry, &mut errors);

    if errors.is_empty() {
        Ok(VerifiedProgram { registry, typed_program })
    } else {
        Err(errors)
    }
}
```

The early-return after Pass 1 is a deliberate, narrow exception to "always
collect every error": a broken type hierarchy invalidates the very
definition of `conforms_to` that Passes 2 and 3 depend on, so continuing
would only generate misleading cascade errors rather than useful ones.
Within Passes 2 and 3 themselves, errors are *accumulated*, never used to
abort the pass — exactly the "multiple errors, not first error" target
from `PRE-SEMANTIC_STATUS.md`'s Task 1.5.

---

## 7. Semantic Rules by AST Construct

Each subsection names the `hulk_ast` node(s) it governs, the hulk-docs
section it implements, and the concrete rule.

### 7.1 Literals (`ExprKind::Literal`)

Trivial, synthesized directly from the literal kind (§A.9.2: *"literals are
the easiest to type-infer, because their type comes directly from the
parser"*): `Literal::Number → Type::Number`, `Literal::String →
Type::String`, `Literal::Boolean → Type::Boolean`.

### 7.2 Variables (`ExprKind::Variable`, `ExprKind::SelfRef`, `ExprKind::BaseRef`)

* `Variable(name)`: looked up in the current `Environment`. Not found →
  `UndefinedVariable`. Note that HULK has **no global variables** —
  only function/protocol/type names are global — so this lookup never
  touches the `TypeRegistry`.
* `SelfRef`: valid only inside a method body; its type is the enclosing
  type, exactly as required by §A.8.2 ("the implicitly defined `self`
  symbol is always assumed as if annotated with type T"). Used outside a
  method → treated as `UndefinedVariable` (there is no global `self`).
* `BaseRef`: valid only inside a method that **overrides** a parent
  method (i.e. the parent type also defines a method of the same name).
  Resolves to the parent's implementation per §A.7.4. Used outside an
  overriding method → `BaseOutsideOverridingMethod`.

### 7.3 Unary / Binary operators (`ExprKind::Unary`, `ExprKind::Binary`)

Implements the operator-typing table from `12-types.pdf`, specialized to
HULK's concrete operator set (§A.2.1, §A.2.2, §A.5):

| Operators | Operand type(s) | Result | hulk-docs § |
|---|---|---|---|
| `+ - * / % ^` (binary), unary `-` | `Number, Number` / `Number` | `Number` | A.2.1 |
| `< <= > >= == !=` | `Number, Number` | `Boolean` | A.5 |
| `& \|` (binary), unary `!` | `Boolean, Boolean` / `Boolean` | `Boolean` | A.5 |
| `@`, `@@` | `{Number, String, Boolean}, {Number, String, Boolean}` | `String` | A.2.2 |

Any operand type outside the listed set produces `InvalidOperator`.

> **Design note (flagged, not specified by hulk-docs):** the specification
> illustrates `==`/`!=` and `@`/`@@` only with `Number`/`String` examples
> and does not define equality or concatenation generically over
> `Object`-rooted user types. This plan restricts `==`/`!=` to `Number`
> operands and `@`/`@@` to the three builtin value types, which matches
> every example in §A.2 and §A.5 exactly. Extending equality to arbitrary
> reference types (structural or identity equality) is an open language
> design question to resolve, not an oversight of this plan.

### 7.4 `let` and `:=` (`ExprKind::Let`, `ExprKind::Assign`)

Implements §A.4 in full:

* Bindings inside one `LetExpr.bindings` are visited **left to right**,
  each one in turn made visible to the *next* binding's initializer
  (§A.4.2: *"every variable is effectively bound in a new scope ... you
  can safely use one variable when defining another"*) — this matches
  the AST shape directly, since the parser already collapses the
  comma-separated `let a = ..., b = ... in body` form into one
  `LetExpr` with a `Vec<LetBinding>` rather than nested `Let` nodes.
* A name may legally repeat across consecutive bindings in the *same*
  `let` (§A.4.5: `let a = 7, a = 7*6 in ...` is valid) — each `declare`
  call simply rebinds in the same (still-open) scope, consistent with
  HULK's general shadowing rule.
* If the binding has a type annotation, the initializer's inferred type
  must conform to it (`NotConforming`); otherwise the variable's type is
  the initializer's inferred type (Pass 2).
* The `LetExpr`'s own type is the type of its `body` (§A.9.2).
* `AssignExpr`: the target must already be declared as a **variable**
  (`AssignTarget::Variable`) — destructive assignment is, per §A.4.6,
  *"the only way a variable can be written to outside of a `let`"* — or
  a `Member`/`Index` target resolved per Sections 7.8/7.9. The new
  value's type must conform to the target's declared type. `self` is
  never a valid assignment target (§A.7.1) → `SelfIsNotAssignable`;
  attempting to assign to an undeclared variable → `UndefinedVariable`;
  attempting `AssignTarget::Variable` against a name that is in scope but
  is actually a function/type name → `InvalidAssignTarget`.

### 7.5 Functions and methods (`FunctionDecl`, `TypeMemberKind::Method`)

* Parameter names must be pairwise distinct within one parameter list
  (`DuplicateParameter`) — note this is *independent* per function: the
  same parameter name may be reused across unrelated functions or
  methods, exactly as §A.9.3 states (*"distinct from each other ... but
  can be equal to ... arguments defined in other functions"*).
* The body is checked in a fresh scope containing exactly the
  parameters (plus `self` and visibility of `base`, for methods); no
  outer local variables leak in, since functions are not closures over
  `let`-bound locals in HULK (only over the global function/type/protocol
  namespace).
* If the return type is annotated, the body's inferred type must conform
  to it; otherwise the function's return type **is** the body's inferred
  type (subject to the recursive-function caveat in Section 8.4.2).
* Each parameter without an explicit annotation is a candidate for
  inference (Section 8.4) from its use inside the body; if no consistent
  type can be derived, `CannotInferType`.

### 7.6 Type conforming relation (`Type::conforms_to`)

Implements §A.8.4 exactly:

```rust
impl Type {
    pub fn conforms_to(&self, other: &Type, registry: &TypeRegistry) -> bool {
        match (self, other) {
            (a, b) if a == b => true,                  // reflexivity
            (_, Type::Object) => true,                 // everything conforms to Object
            (Type::Named(t1), Type::Named(t2)) =>
                registry.is_ancestor(t2, t1)             // T1 <= T2 iff T2 is an ancestor of T1
                || registry.implements_protocol(t1, t2), // structural conformance (§A.10.4)
            (Type::Named(t1), _) if registry.is_protocol(other) =>
                registry.implements_protocol(t1, /* protocol name */ other),
            // Number/String/Boolean conform only to themselves and Object
            // (already covered above); no implicit numeric widening exists
            // in HULK (single Number type, §A.1.1).
            _ => false,
        }
    }
}
```

`lowest_common_ancestor(types: &[Type]) -> Type` walks each type's parent
chain up to `Object` and returns the deepest common node, implementing
§A.9.2's rule for `if`/`elif`/`else` branch unification (and, by
extension, `while`/`for` bodies and the Phase 4 `match` expression).

### 7.7 Conditionals and loops (`IfExpr`, `WhileExpr`, `ForExpr`)

* `IfExpr`/`ElifBranch`: every condition must have inferred type
  `Boolean` (`NonBooleanCondition` otherwise); the whole expression's
  type is the lowest common ancestor of `then_branch`, every elif body,
  and `else_branch` (§A.9.2 — the `else` branch is mandatory in the AST,
  matching the grammar, so there is no "missing else" case to special-case
  unlike languages where `if` without `else` types as unit/void).
* `WhileExpr`: condition must be `Boolean`; the loop's type is the body's
  type (§A.6.1 — *"the return value of the while loop is the return value
  of its expression body"*).
* `ForExpr`: the `iterable` expression's type must implement the
  `Iterable` protocol — i.e. expose `next(): Boolean` and a `current()`
  method — checked structurally via `implements_protocol`. The loop
  variable is declared, in a fresh scope covering only the loop body,
  with the **covariant** return type of `current()` for that specific
  iterable (§A.11.1: *"this transpilation guarantees ... you will get the
  exact covariant type inferred"*) — i.e., when the iterable is a builtin
  `Range`, the bound variable is typed `Number`, not the protocol's
  generic `Object`, by reading the *concrete* `current()` signature off
  the iterable's resolved type rather than off the abstract `Iterable`
  protocol. The loop's own type is the body's type (§A.6.2).

### 7.8 Types, objects, inheritance (`TypeDecl`, `NewExpr`, `MemberExpr`, `AssignTarget::Member`)

* `NewExpr`: the named type must exist (`UndefinedType` otherwise); the
  constructor argument count and types must conform to the type's
  declared constructor parameters (`ArityMismatch` / `NotConforming`).
  Each attribute initializer is checked in a scope containing **only**
  the global namespace and the type's own constructor parameters —
  *not* `self`, and not sibling attributes (§A.7.2: *"you cannot use
  other attributes of the same instance in an attribute initialization
  expression"*). Parent constructor argument expressions (the
  `TypeParent.args`) are checked in a scope containing the global
  namespace plus the **inheriting** type's own constructor parameters
  (§A.7.3), never the parent's.
* `MemberExpr` (read access, `obj.member`): the receiver's type must
  resolve to a known type or protocol; the member must exist somewhere
  in its (flattened, per Pass 1) method or attribute table. Per §A.7
  (*"Attributes are always private ... methods are always public"*),
  attribute access is rejected unless the access occurs **inside a
  method of that exact type** (`self.x`-shaped access); cross-instance or
  external attribute reads are always a semantic error
  (`UnknownMember`/an access-control variant), even between an instance
  and its own subclass, exactly as §A.7 specifies (*"not even
  inheritors"*). Method access has no such restriction (methods are
  always public).
* `AssignTarget::Member` (write access, `obj.field := value`): same
  visibility rule as above, plus the value's type must conform to the
  attribute's declared/inferred type.
* Method bodies are checked in a scope containing the method's own
  parameters plus `self` (typed as the enclosing type) and `base`
  resolution per Section 7.2.

### 7.9 Vectors and indexing (`VectorExpr`, `IndexExpr`)

* `VectorExpr::Literal(items)`: every item must have a mutually
  conforming type; the vector's type is `Vector(lowest_common_ancestor(items))`.
  An empty literal (`[]`) infers as `Vector(Type::Unknown)`, resolved from
  context if possible (e.g. an enclosing `let` annotation), else
  `CannotInferType` (mirrors HULK's general "fail rather than guess"
  contract from §A.9.3).
* `VectorExpr::Comprehension`: the bound variable is declared, scoped to
  the comprehension's head expression only, with the iterable's
  covariant element type (same mechanism as `ForExpr`, Section 7.7); the
  comprehension's type is `Vector(type of head expression)` (§A.12.2).
* `IndexExpr`: the object's type must be `Vector(T)` for some `T`
  (`IndexOnNonVector` otherwise — per §A.12.3, indexing is *not* part of
  the generic `Iterable` protocol, only of the concrete builtin vector
  type); the index expression must be `Number`; the result type is `T`.

### 7.10 `is` and `as` (`TypeTestExpr`, `DowncastExpr`)

* `TypeTestExpr` (`expr is Type`): `expr`'s type must be `Object`-rooted
  (i.e. not `Number`/`String`/`Boolean`, which have no dynamic subtyping
  to test, per §A.8.4: *"the only types that conform to Number, String,
  Boolean are respectively those same types"*); the named type must
  exist. Result type is always `Boolean`.
* `DowncastExpr` (`expr as Type`): same receiver restriction; the named
  type must exist. The result is statically typed as the named type
  (§A.8.6), with the runtime-failure possibility documented as a
  *runtime*, not semantic, error — consistent with the spec's own
  wording (*"the result is a runtime error if the expression is not a
  suitable dynamic type"*). As a quality-of-life diagnostic (not a hard
  error, since it is not specified as one), the analyzer may emit a
  **warning** when the static types of `expr` and the target are
  provably unrelated in the hierarchy (neither conforms to the other),
  since such a cast can never succeed.

### 7.11 Protocols (`ProtocolDecl`)

Protocols never appear as `new`-able or instantiable; they exist purely
as annotation targets and as conformance predicates (§A.10). Their
semantic surface is therefore concentrated in Pass 1 (extension validity,
Section 6.2) and in `Type::conforms_to`'s structural branch (Section 7.6).
There is no expression-level node for "implements"; conformance is always
checked at the point of use — assignment, parameter passing, return —
never declared.

### 7.12 `match` expressions — project extension (`MatchExpr`, `Pattern`)

`MatchExpr` is **not** part of the formal HULK grammar in `hulk-docs.pdf`;
it is a project-level "extension node" explicitly flagged as such in
`hulk-ast`'s own documentation and in `PRE-SEMANTIC_STATUS.md` ("planned
`match` extension node"). It must not be confused with the *compile-time*
structural pattern matching used inside macros (§A.14.5), which operates
on un-evaluated expression trees and is entirely out of this plan's scope
(Section 3). This plan treats the *runtime* `match` as a generalized,
multi-branch `if`:

* The scrutinee (`value`) is evaluated once; its static type `S` is
  computed.
* For each `MatchCase`:
  * `Pattern::Wildcard`: always matches; introduces no binding.
  * `Pattern::Literal(lit)`: the literal's type must equal `S` exactly
    (e.g. a `Number` pattern against a `String` scrutinee is a
    `TypeMismatch`, since it could never match).
  * `Pattern::Variable(name)`: always matches; binds `name : S` for the
    case body only.
  * `Pattern::Type(type_ref, alias)`: `S` must be `Object`-rooted (same
    restriction as `is`/`as`, Section 7.10); the case body is type-checked
    as if `value` had been downcast to `type_ref`, and if `alias` is
    present it is bound to that narrowed type for the case body only
    (the combined effect of `is` + `as` from §A.8.5–6, expressed as one
    construct).
* The whole expression's type is the lowest common ancestor of every
  case body's type (same rule as `IfExpr`, Section 7.6/7.7).
* Exhaustiveness is **not** statically enforced (mirroring `as`'s
  documented runtime-failure semantics, Section 7.10): a `match` with no
  `Wildcard`/`Variable` catch-all simply risks a runtime "no matching
  case" failure, which this plan treats analogously to a failed `as`
  downcast — out of scope for a hard semantic error, but a reasonable
  candidate for a future non-blocking warning.

---

## 8. Algorithms for the Hard Problems

### 8.1 Forward references and mutual recursion

Solved structurally by the pass split (Section 6): Pass 0 makes every
function/type/protocol *signature* visible before any *body* is examined,
so `function cot(x) => 1 / tan(x); function tan(x) => ...` (§A.3.1's own
example, where `cot` is defined before `tan`) type-checks correctly
regardless of order, and mutually recursive functions resolve each other's
signatures trivially. Bodies are then checked in any convenient order in
Pass 2/3 (declaration order is used for determinism, but it is not load-
bearing for correctness).

### 8.2 Recursive functions without an explicit return type

A genuinely hard case, explicitly called out as unsolved future work in
`PRE-SEMANTIC_STATUS.md`'s slide deck ("Cosas por hacer ... Adicionar
soporte para funciones recursivas"). The plan:

1. Before visiting a function's body, **pre-register** its signature in
   the registry with `return_type = Type::Unknown` if unannotated.
2. Visit the body. Every *recursive* call to the function currently being
   inferred observes `Unknown` as the call's result type, which the
   surrounding expression then also infers as `Unknown` rather than
   failing outright (an "optimistic placeholder", not an error yet).
3. After the body is fully visited, compare the body's resulting type
   against the placeholder:
   * If the body's type is concrete (no `Unknown` propagated to the top),
     accept it as the inferred return type — this covers the textbook
     case in §A.9.4, `fib(n) => if (n==0|n==1) 1 else fib(n-1)+fib(n-2)`,
     whose non-recursive branch (`1`) and arithmetic context already pin
     the type to `Number` without ever needing the recursive call's
     result type to be known up front.
   * If the body's type still contains `Unknown` (the recursive result
     was load-bearing and never got resolved — e.g. a function whose
     *only* branch is a bare recursive call with no concrete base case),
     this is a genuine inference failure: emit `CannotInferType` and
     request an explicit annotation, which is the sound, spec-compliant
     fallback per §A.9.3 (*"otherwise it must fail to infer"*).

This keeps the algorithm a strict, bounded **one-pass-with-a-placeholder**
strategy rather than a general fixed-point solver, deliberately trading
some inference power for predictability and a simple implementation,
consistent with the "basic inference strategy" baseline the spec itself
names as sufficient (§A.9.3).

### 8.3 `self`/`base` resolution

`self`'s type is pushed into the `Environment` as a normal binding the
moment a method body's scope is opened, with the enclosing type's name —
no special-casing needed elsewhere. `base` is resolved by looking up the
*current* method's name in the *parent* type's flattened method table
(built in Pass 1); if absent, `BaseOutsideOverridingMethod`.

### 8.4 Type inference strategy and its documented boundary

Per §A.9.1, two distinct failure points exist, and this plan preserves
that distinction precisely:

1. **Inference failure** (Pass 2): the inferer could not assign *any*
   type to some symbol. Reported as `CannotInferType`. The program is
   not further checked for *consistency* once this happens for a given
   symbol — its type becomes `Type::Error`, suppressing cascades from
   that point on (Section 5.2).
2. **Checking failure** (Pass 3): every symbol successfully received a
   type, but two such types are mutually inconsistent (e.g. an annotation
   does not conform to the inferred initializer type). Reported via the
   `TypeMismatch`/`NotConforming` family.

#### 8.4.1 Baseline: the "basic inference strategy"

§A.9.3 explicitly names the trivial sound strategy — *infer types for
expressions, fail for all unannotated symbols* — as a valid, if weak,
implementation. This plan does not stop there; instead it implements the
concrete, bounded extensions already worked through in §A.9.4 as ad-hoc
examples, because they are cheap and mechanical given the structures
already built:

* A `let` binding's type is always inferable (it is exactly the
  initializer's type) — never a failure case.
* An attribute's type is always inferable for the same reason.
* A function/method parameter's type is inferable when the parameter is
  used **only** in a context that pins it to exactly one type: as an
  operand of an arithmetic/comparison/boolean/concatenation operator (Section 7.3
  fixes the operand type unambiguously), as an argument to a call whose
  corresponding parameter is already typed, or as the receiver of a
  member access on a type that can be uniquely identified from the
  member name across the whole registry. If two *different* branches of
  usage would require incompatible types, inference fails
  (`AmbiguousInference`) rather than guessing, per §A.9.3 (*"if more than
  one type ... would be consistent, the type inferer must fail"*).

#### 8.4.2 Recursive functions

Covered in 8.2 as an extension of the same mechanism.

#### 8.4.3 Out of scope: general protocol-synthesis inference (§A.9.5)

The spec's "general strategy" — synthesizing an ad-hoc protocol for every
unannotated parameter from its structural use (`x.f()`, `x.g()`, …) and
iteratively refining it across the whole program — is explicitly
presented in `hulk-docs.pdf` as a *suggestion*, not a requirement, and is
flagged there with its own caveat ("to code a robust type inferrer is much
harder than what the previous explanation might seem"). This plan records
it as **documented future work**, to be revisited only after Phases 1–3
are implemented and tested, exactly mirroring how `PRE-SEMANTIC_STATUS.md`
treats code generation relative to semantic analysis: a clearly named
next step, not a silent gap.

---

## 9. Error Reporting Strategy

* **Multiple errors per run.** Every pass appends to a shared
  `Vec<SemanticError>` rather than returning on first failure, fulfilling
  `PRE-SEMANTIC_STATUS.md`'s Task 1.5 goal directly. The only short-circuit
  is the Pass-1 hierarchy gate described in Section 6.5, justified by
  cascade suppression rather than convenience.
* **Span-accurate diagnostics.** Every `SemanticError` carries the
  `SourceSpan` of the offending AST node, reusing `hulk_ast::SourceSpan`
  so that `hulk-cli` can report `line:col` exactly as it already does for
  `ParseError`.
* **Cascade suppression via `Type::Error`.** Once an expression has been
  reported as erroneous, its type becomes `Type::Error`; every rule in
  Section 7 treats `Type::Error` as automatically conforming to/from
  anything, so a single root-cause mistake (e.g. an undefined variable
  used as an operand) does not generate a chain of unrelated follow-up
  "type mismatch" errors for everything built on top of it.
* **`Display` formatting** mirrors `hulk_parser::ParseError`'s style
  (`"semantic error at line {line}, col {col}: {message}"`), so the two
  phases read consistently from `hulk-cli`'s point of view.

---

## 10. Testing Strategy

Following the existing convention in `hulk-parser` and `hulk-ast` (inline
`#[cfg(test)] mod tests` per source file, driving the public API
end-to-end through `Lexer → parse → analyze`), tests are organized by the
same construct grouping as Section 7:

* **Name resolution:** undefined variable/function/type, correct
  resolution across forward references, shadowing per §A.4.5's own
  worked examples (used verbatim as test fixtures, since the spec already
  states their expected behavior).
* **Redeclaration:** duplicate function/type/attribute/parameter names.
* **Inheritance:** valid chain, cycle detection, inheriting from a
  builtin value type, override signature mismatch, `base` resolution
  (Person/Knight example from §A.7.4, verbatim).
* **Conforming relation:** reflexivity, transitivity, LCA computation
  across a small synthetic hierarchy, protocol structural conformance
  (Hashable/Person example from §A.10.2, verbatim).
* **Operators:** every row of the table in Section 7.3, both the success
  and the rejection case.
* **Control flow:** branch-type unification, non-boolean condition
  rejection, `for`-over-`Range` covariant element typing.
* **Inference:** the three worked examples from §A.9.4 (`fib`, `fact`,
  the `let x = 42` case) used as direct regression fixtures, plus the
  recursive-function placeholder algorithm (Section 8.2) tested against
  both a resolvable and an unresolvable recursive shape.
* **Vectors/indexing:** literal LCA typing, comprehension element typing,
  indexing on a non-vector rejection.
* **`is`/`as`/`match`:** dynamic type test restricted to `Object`-rooted
  types, downcast typing, and the Phase-4 `match` rules of Section 7.12.
* **Multi-error reporting:** a single fixture program containing several
  unrelated mistakes, asserting all of them are reported in one
  `analyze()` call and that no spurious cascade errors appear.

---

## 11. Phased Delivery Roadmap

Adapted from, and made concrete relative to, the "Recommended
Implementation Order" already published in `PRE-SEMANTIC_STATUS.md`:

| Phase | Deliverable | Depends on |
|---|---|---|
| **0** | `hulk-ast` parametrized-AST refactor (Section 5.1): generify `Expr`/`ExprKind`/`Declaration`/`Program` and every nested struct over the annotation parameter `A = ()`, with existing constructors unchanged at the call site. Purely mechanical; no observable behavior change, `hulk-parser`'s test suite untouched. | None (can start immediately, independently of the rest of this plan) |
| **1** | `Environment`, `TypeRegistry` skeleton with builtins, Pass 0 (collection) + Pass 2/3 for: literals, variables, operators, `let`/`:=`, blocks, `if`/`elif`/`else`, `while`, `for` over `Range`, global functions (no inheritance yet) | Phase 0 |
| **2** | Pass 1 (hierarchy), `self`/`base`, `new`, member access/assignment with privacy rules, `is`/`as`, `conforms_to` over the nominal hierarchy | Phase 1 |
| **3** | Protocols (structural conformance, variance, Pass-1 extension validity), vectors (literal/comprehension/indexing), `Iterable`/`Vector<T>` annotation sugar (`T*`/`T[]`) | Phase 2 |
| **4** | `match` expression rules (project extension, Section 7.12) | Phase 3 |
| **Future** | General protocol-synthesis inference (§A.9.5); functor sugar `(T) -> R` and macro semantics, blocked on parser support per Section 3 | Phase 4 |

Each phase ends with `cargo test -p hulk-semantic` green and a short
status note appended to a future `SEMANTIC_STATUS.md`, mirroring exactly
how `PRE-SEMANTIC_STATUS.md` already documents the frontend's own
incremental completion.

---

## 12. `hulk-cli` Integration (forward note)

Once Phase 1 lands, `hulk-cli` should be extended to run
`hulk_semantic::analyze` after `hulk_parser::parse` and before printing
the AST, printing every collected `SemanticError` (not just the AST) when
analysis fails, and printing a confirmation plus the AST when it
succeeds. This is a small, additive change to the existing
`Lexer::tokenize → parse → print` pipeline already in `hulk-cli` and is
intentionally **not** part of this plan's core deliverable, to keep this
document focused on `hulk-semantic` itself.

---

## 13. Known Limitations of This Plan

* Equality (`==`, `!=`) and concatenation (`@`, `@@`) operand typing for
  user-defined reference types is left as an open design question
  (Section 7.3) rather than guessed at, since `hulk-docs.pdf` does not
  specify it.
* `match` (Section 7.12) is a project-specific extension with no
  normative specification in `hulk-docs.pdf`; its rules in this plan are
  this author's best-effort generalization of `if`/`is`/`as`, not a
  transcription of an authoritative source.
* Functor sugar and macros (Section 3) are explicitly excluded because
  the parser does not yet support them; this plan does not attempt to
  retrofit semantic rules for syntax that cannot currently be produced by
  `hulk-parser`.
* The general protocol-synthesis inference strategy of §A.9.5 is
  documented (Section 8.4.3) but deliberately deferred, consistent with
  the specification's own framing of it as an advanced suggestion rather
  than a baseline requirement.
* The `hulk-ast` refactor in Section 5.1 is specified at the level of
  *node kinds* already named throughout this document (the same level of
  detail this plan uses everywhere else), not as a line-by-line diff
  against `hulk-ast`'s actual source files, which were not available
  while writing this plan. The authoritative, exhaustive list of structs
  requiring the `A` parameter must be produced by walking the real crate
  during Phase 0; this plan should not be treated as that list.

---

## 14. References

* `hulk-docs.pdf`, Appendix A — "The HULK Programming Language" (primary
  normative source for every rule in Section 7).
* `10-semantic.pdf` — "Declaración de variables antes de su uso": syntactic
  vs. semantic rules, AST design, the `IContext`/`Validate` pattern that
  motivates `Environment` and the pass-based driver.
* `11-attr-gram.pdf` — "Evaluando expresiones": attribute grammars,
  synthesized vs. inherited attributes, S-attributed/L-attributed
  evaluation order, motivating the visitor design in Section 6.
* `12-types.pdf` — "Sistema de Tipos": type fundamentals and the
  operator-typing table generalized in Section 7.3.
* `PRE-SEMANTIC_STATUS.md` — current frontend completion status, the
  `VerifiedProgram`/`analyze` interface contract adopted in Section 5.6,
  and the implementation-order recommendations adapted in Section 11.
* `GRAMMAR_LL1.md`, `lib.rs` (hulk-ast, hulk-lexer, hulk-parser) — ground
  truth for what the current frontend actually produces, used throughout
  Section 3 and Section 7 to keep every rule implementable against the
  real AST rather than an idealized one.
* Production-compiler precedent for the parametrized-AST design adopted
  in Section 5.1 — rustc's generic `HIR`, GHC's "Trees That Grow"
  pattern, and Scala's `Tree[Type]`. Cited as engineering justification
  for *how* to attach `Type` to the existing tree without duplicating it;
  not a course-supplied or project-internal source, and not a normative
  reference for HULK's own semantics (those remain `hulk-docs.pdf`
  exclusively).
