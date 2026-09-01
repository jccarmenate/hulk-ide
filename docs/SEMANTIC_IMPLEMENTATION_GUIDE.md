# HULK Semantic Analysis — Implementation Guide

**Companion to:** `SEMANTIC_IMPLEMENTATION_PLAN.md` (design rationale) and
`PRE-SEMANTIC_STATUS.md` (frontend status).

**Purpose of this document:** a sequential, file-by-file checklist for
*implementing* `hulk-semantic`. Each step names the exact file to touch,
what to put in it, and — for every type and function introduced — what its
job is and why it exists. Follow the steps in order: each one assumes every
previous step is done, because passes are built on top of the registry and
environment, and the registry is built on top of the `Type` system.

---

## Step 0 — Parametrize the AST

**File to modify:** `crates/hulk-ast/src/lib.rs`

**Why this step exists:** Pass 2 (Step 9) needs to produce a tree that is
*identical in shape* to `hulk_ast::Program`, except every expression node
also carries a resolved `Type`. Rather than hand-writing a second tree
(`tast.rs`) that must be kept in sync by hand forever, we make the existing
tree generic over an annotation type `A`, defaulted to `()`. This is a
mechanical, behavior-preserving refactor: every existing call site in
`hulk-parser` and its tests keeps compiling unchanged because `A` defaults
to `()`.

**What to change:**

1. Add a type parameter `A = ()` to every struct/enum that *is*, or
   transitively contains, an `Expr`:
   - `Expr<A>` — fields become `kind: ExprKind<A>`, `anno: A`, `span: SourceSpan`.
     `anno` is the new field: `()` for untyped syntax, `Type` once Pass 2
     has run.
   - `ExprKind<A>` — every variant that boxes a sub-struct containing an
     `Expr` becomes `ExprKind<A>` and boxes that struct's `<A>` instantiation
     (`Unary(Box<UnaryExpr<A>>)`, `Binary(Box<BinaryExpr<A>>)`, …). Variants
     with no nested `Expr` (`Literal`, `Variable`, `SelfRef`, `BaseRef`) stay
     non-generic.
   - All nested structs, recursively: `UnaryExpr<A>`, `BinaryExpr<A>`,
     `LetExpr<A>`, `LetBinding<A>`, `AssignExpr<A>`, `IfExpr<A>`,
     `ElifBranch<A>`, `WhileExpr<A>`, `ForExpr<A>`, `CallExpr<A>`,
     `MemberExpr<A>`, `NewExpr<A>`, `TypeTestExpr<A>`, `DowncastExpr<A>`,
     `VectorExpr<A>`, `VectorComprehension<A>`, `IndexExpr<A>`,
     `MatchExpr<A>`, `MatchCase<A>`, and `Pattern<A>` (only the `Type`
     variant carries an alias that participates in type-checking, but make
     the whole enum generic for uniformity).
   - Declaration-level nodes that contain a body/initializer:
     `FunctionDecl<A>` (its `body: Expr<A>`), `TypeDecl<A>` (its
     `members: Vec<TypeMember<A>>`), `TypeMember<A>`,
     `AttributeDecl<A>` (its `initializer: Expr<A>`), `TypeParent<A>`
     (its `args: Vec<Expr<A>>`), `DeclarationKind<A>`, `Declaration<A>`.
   - `ProtocolDecl` and `ProtocolMethod` stay **non-generic** — protocols
     have signatures only, never a body or initializer, so there is nothing
     in them to annotate.
   - `Program<A>` — `declarations: Vec<Declaration<A>>`, `entry: Expr<A>`.

2. Keep every existing constructor (`Expr::number`, `Expr::string`,
   `Expr::binary`, `Expr::call`, …) with its current signature. Internally
   they now produce `Expr<()>` by writing `anno: ()` — no caller anywhere
   needs to change.

3. `walk_expr` and `AstVisitor` become generic over `A` the same way, with
   no change to their existing behavior for `A = ()`.

**Acceptance check:** `cargo test -p hulk-ast` and `cargo test -p
hulk-parser` pass with **zero changes** to either crate's test code. If a
test needs editing, the refactor was not purely mechanical — revert and
redo.

**Do not** add this generic parameter to `SourceSpan`, `TypeRef`, `Param`,
or `Literal` — none of these contain an `Expr`, so they have nothing to
annotate and must stay exactly as they are today.

---

## Step 1 — Scaffold the `hulk-semantic` crate

**Files to create:**

- `Cargo.toml` (workspace member entry) — modify
  `Cargo.toml` (workspace root) to add `"crates/hulk-semantic"` to
  `members`.
- `crates/hulk-semantic/Cargo.toml` — new package manifest:
  ```toml
  [package]
  name = "hulk-semantic"
  version.workspace = true
  edition.workspace = true

  [dependencies]
  hulk-ast = { path = "../hulk-ast" }
  ```
  No dependency on `hulk-lexer`: `hulk-semantic` never touches tokens, and
  `SourceSpan` is already re-exported from `hulk-ast`.
- `crates/hulk-semantic/src/lib.rs` — crate root; for now just declare the
  module tree (`mod error; mod environment; mod typed; mod types; mod
  passes;`) plus `pub use` re-exports. The `analyze` function itself is
  written in Step 11, after every module it depends on exists.
- Empty module files to create now (filled in later steps):
  - `crates/hulk-semantic/src/error.rs`
  - `crates/hulk-semantic/src/environment.rs`
  - `crates/hulk-semantic/src/typed.rs`
  - `crates/hulk-semantic/src/types/mod.rs`
  - `crates/hulk-semantic/src/types/registry.rs`
  - `crates/hulk-semantic/src/passes/mod.rs`
  - `crates/hulk-semantic/src/passes/collect.rs`
  - `crates/hulk-semantic/src/passes/hierarchy.rs`
  - `crates/hulk-semantic/src/passes/infer.rs`
  - `crates/hulk-semantic/src/passes/check.rs`

**Purpose of this layout:** mirrors the existing crate convention
(`hulk-parser` has one file per concern); separates *global, read-only
knowledge* (`types/`) from *per-traversal, scoped state* (`environment.rs`)
from *the four analysis passes* (`passes/`), so each pass file can be
reviewed and tested in isolation.

---

## Step 2 — Define the `Type` system

**File to modify:** `crates/hulk-semantic/src/types/mod.rs`

This is the **synthesized attribute** every expression visit returns
(Section 2.2 of the plan). It must have no lifetime and no reference into
the AST, because it is stored in maps, returned up the tree, and compared
cheaply.

### 2.1 The `Type` enum

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Number,
    String,
    Boolean,
    Object,
    Named(String),
    Vector(Box<Type>),
    Iterable(Box<Type>),
    Unknown,
    Error,
}
```

| Variant | Purpose |
|---|---|
| `Number`, `String`, `Boolean` | The three builtin value types (hulk-docs §A.2.1). |
| `Object` | Root of the nominal hierarchy; every type conforms to it. |
| `Named(String)` | A user-defined `type` or `protocol`, resolved by name through `TypeRegistry`. Classes and protocols share one namespace, since both can appear in annotation position. |
| `Vector(Box<Type>)` | `Vector<T>` — both the `T[]` annotation sugar (§A.12.3) and the type of vector literals/comprehensions. |
| `Iterable(Box<Type>)` | `Iterable<T>` — the `T*` annotation sugar (§A.11.2) and the builtin `Iterable` protocol specialized to an element type. |
| `Unknown` | Internal placeholder while a symbol's type is mid-inference (Step 9.3, recursive functions). Must never survive into a successfully `analyze`d program. |
| `Error` | Poison value assigned to a node *after* an error has been reported for it, so the error does not cascade (Step 10, cascade suppression). |

### 2.2 `conforms_to`

```rust
impl Type {
    pub fn conforms_to(&self, other: &Type, registry: &TypeRegistry) -> bool;
}
```

**Purpose:** implements the `<=` relation from hulk-docs §A.8.4 — "can a
value of `self`'s type be used wherever `other` is expected?" This is the
single predicate every type-checking rule in Step 10 calls; it must not be
reimplemented ad hoc anywhere else.

**Rules, in order of priority:**
1. `self == other` → `true` (reflexivity).
2. `other == Type::Object` → `true` (everything conforms to `Object`).
3. Either side is `Type::Error` → `true` (cascade suppression — an
   already-erroneous type must not trigger a *second* error).
4. Both `Named` → `true` if `registry` reports `other` as a nominal
   ancestor of `self` (single-inheritance chain walk) **or** if `self`'s
   type structurally implements the protocol named by `other`
   (`registry.implements_protocol`, built in Step 8).
5. `self` is `Named` and `other` resolves to a protocol → delegate to
   `implements_protocol` directly.
6. Otherwise (e.g. `Number` vs `String`, or `Number` vs a `Named` type) →
   `false`. There is no implicit numeric widening in HULK — it has exactly
   one numeric type.

### 2.3 `lowest_common_ancestor`

```rust
pub fn lowest_common_ancestor(types: &[Type], registry: &TypeRegistry) -> Type;
```

**Purpose:** implements §A.9.2's rule for unifying the type of multi-branch
constructs. Walks each type's ancestor chain up to `Object` and returns
the deepest node common to all chains. Used by:
- `if`/`elif`/`else` (Step 10.7) — LCA of every branch.
- `while`/`for` bodies — trivially their own type, but the helper is reused
  for consistency.
- The `match` extension (Step 10.12) — LCA of every case body.
- Vector literals (Step 10.9) — LCA of every item.

If the input slice is empty, or any element is `Type::Error`, return
`Type::Error` (propagate, don't crash).

---

## Step 3 — Build the `TypeRegistry`

**File to modify:** `crates/hulk-semantic/src/types/registry.rs`

This is the **global, context-independent knowledge** built once (Steps 7–8)
and read-only afterward — the analyzer's equivalent of the global scope in
the `IContext` design referenced by the plan, specialized for HULK's
separate type/protocol and function namespaces.

### 3.1 Data structures

```rust
pub struct TypeRegistry {
    types: HashMap<String, TypeInfo>,
    protocols: HashMap<String, ProtocolInfo>,
    functions: HashMap<String, FunctionSignature>,
}
```
**Purpose:** the single source of truth for "does this name exist, and
what does it mean?" Three separate maps because HULK never resolves a
function name and a type name through the same syntax (`f(...)` vs.
`new T(...)`), so there is no ambiguity to arbitrate between them.

```rust
pub struct TypeInfo {
    pub name: String,
    pub params: Vec<(String, Type)>,        // type constructor parameters
    pub parent: Option<ParentLink>,
    pub attributes: HashMap<String, AttributeInfo>,
    pub methods: HashMap<String, MethodSignature>,
    pub span: SourceSpan,
}
```
**Purpose:** everything known about one `type` declaration. `attributes`
and `methods` start as *only this type's own* members (Step 7) and are
*flattened* to include inherited ones in Step 8.6.

```rust
pub struct ParentLink {
    pub name: String,
    pub args: Vec<hulk_ast::Expr>, // Expr<()> — untyped, collected in Pass 0
}
```
**Purpose:** records the `inherits Base(args)` clause exactly as written,
before the parent name has even been verified to exist. Kept untyped
because Pass 0 runs before any type inference; Pass 2 re-visits these
`args` to build their typed counterparts.

```rust
pub struct AttributeInfo {
    pub declared_type: Option<Type>, // None until inferred in Pass 2
    pub span: SourceSpan,
}

pub struct MethodSignature {
    pub params: Vec<(String, Type)>,
    pub return_type: Type,
    pub defined_in: String, // owning type name, used for `base` resolution
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

### 3.2 Builtin seeding

**Function to add:** `pub fn seeded_registry() -> TypeRegistry` (place in
`registry.rs`, exported and called once from `lib.rs::analyze`, Step 11).

**Purpose:** pre-populates the registry with every name HULK programs may
reference without declaring, so that Step 7 (collection) and later passes
never special-case "is this a builtin?" — a builtin is simply a name that
was already in the registry before the user's declarations were collected.

Populate exactly this table (cross-referenced to hulk-docs §):

| Name | Kind | Signature |
|---|---|---|
| `Object` | type | root; no parent; no public members (§A.7.3) |
| `Number`, `String`, `Boolean` | type | parent `Object`; flagged so Step 8.2 can reject `inherits Number` etc. |
| `Iterable` | protocol | `next(): Boolean`, `current(): Object` (§A.11) |
| `Enumerable` | protocol | `iter(): Iterable` (§A.11.3) |
| `Range` | type | ctor `(min: Number, max: Number)`; implements `Iterable` with `current(): Number` (covariant return, §A.11) |
| `print` | function | `(x: Object): Object` |
| `sqrt`, `sin`, `cos`, `exp` | function | `(x: Number): Number` |
| `log` | function | `(base: Number, x: Number): Number` |
| `rand` | function | `(): Number` |
| `range` | function | `(min: Number, max: Number): Range` |
| `PI`, `E` | constant | modeled as zero-arg `FunctionSignature`s returning `Number`, or as pre-bound global `Binding`s consulted by variable lookup before reporting `UndefinedVariable` |

### 3.3 Query helpers

Add these methods to `TypeRegistry` — every later pass calls these instead
of touching the internal maps directly:

```rust
impl TypeRegistry {
    pub fn lookup_type(&self, name: &str) -> Option<&TypeInfo>;
    pub fn lookup_protocol(&self, name: &str) -> Option<&ProtocolInfo>;
    pub fn lookup_function(&self, name: &str) -> Option<&FunctionSignature>;
    pub fn is_protocol(&self, ty: &Type) -> bool;
    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool;
    pub fn implements_protocol(&self, type_name: &str, protocol_name: &str) -> bool;
}
```

- `is_ancestor` — walks `descendant`'s `parent` chain (built in Step 8)
  looking for `ancestor`. Used by `Type::conforms_to`.
- `implements_protocol` — for every method in the protocol's *flattened*
  method set (including `extends`ed parents), checks the type has a method
  of the same name whose parameters are **contravariant** and return type
  **covariant** relative to the protocol's signature (§A.10.3). This is the
  one place protocol *variance* rules and nominal *override* rules
  deliberately differ (Step 8.4) — keep them in separate functions, don't
  merge them.

---

## Step 4 — Build the `Environment` (scopes)

**File to modify:** `crates/hulk-semantic/src/environment.rs`

This is the **inherited attribute** (Section 2.2/2.3 of the plan): the
scope stack threaded top-down through every visitor in Steps 9–10.

```rust
pub struct Environment {
    scopes: Vec<HashMap<String, Binding>>,
}

pub struct Binding {
    pub ty: Type,
    pub span: SourceSpan,
}
```

Methods to implement:

```rust
impl Environment {
    pub fn new() -> Self;                 // one root scope, for globals if any
    pub fn push_scope(&mut self);
    pub fn pop_scope(&mut self);
    pub fn declare(&mut self, name: &str, ty: Type, span: SourceSpan);
    pub fn lookup(&self, name: &str) -> Option<&Binding>;
}
```

- `push_scope` / `pop_scope` — called around: a `let`'s body, a function or
  method body, a `for` loop body, and (Step 10.12) each `match` case body.
  **Never** called for a plain `{ ... }` block — per §A.4, blocks are pure
  sequencing, not a scope boundary. Getting this wrong breaks the
  shadowing examples in §A.4.5 used as regression fixtures in Step 13.
- `declare` — inserts into the **innermost** scope, overwriting any
  existing entry of the same name in that same scope. This is intentional:
  §A.4.5 explicitly allows `let a = 7, a = 7 * 6 in print(a)` — declaring
  the same name twice **within one `let`'s binding list** is legal and
  simply rebinds. Shadowing an *outer* scope is always legal too, because
  `lookup` only ever sees the innermost matching entry.
  Function/method parameter lists and `for`/`match` bindings call
  `declare` exactly once per name; *duplicate names within one such list*
  are rejected earlier, in Step 7 (Pass 0), not here — `Environment` itself
  has no opinion on whether a duplicate is an error, only `let` permits it.
- `lookup` — searches scopes innermost-to-outermost, returns the first hit.
  Returns `None` if the name is not bound anywhere, which every caller in
  Steps 9–10 turns into `SemanticErrorKind::UndefinedVariable`.

---

## Step 5 — Define `SemanticError`

**File to modify:** `crates/hulk-semantic/src/error.rs`

**Purpose:** the diagnostic type every pass appends to, modeled directly on
`hulk_parser::ParseError`'s `{ kind, span }` shape so `hulk-cli` (Step 12)
can render both phases through one code path.

```rust
pub struct SemanticError {
    pub kind: SemanticErrorKind,
    pub span: SourceSpan,
}
```

Define `SemanticErrorKind` with **every** variant below — group them as
shown so each pass in Steps 7–10 can be checked against this list for
completeness:

```rust
pub enum SemanticErrorKind {
    // Name resolution
    UndefinedVariable(String),
    UndefinedFunction { name: String, arity: usize },
    UndefinedType(String),
    UnknownMember { ty: Type, member: String },

    // Redeclaration
    DuplicateFunction(String),
    DuplicateType(String),
    DuplicateAttribute { ty: String, attribute: String },
    DuplicateParameter(String),

    // Inheritance
    InheritFromBuiltinValueType(String),
    InheritFromUndefinedType(String),
    InheritanceCycle(Vec<String>),
    InvalidOverride { method: String, in_type: String, expected: String, found: String },

    // Protocols
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

    // Inference
    CannotInferType { symbol: String },
    AmbiguousInference { symbol: String, candidates: Vec<Type> },

    // Non-blocking diagnostics (Step 10.10, Step 10.12)
    UnreachableDowncast { from: Type, to: Type },
    NonExhaustiveMatch,
}
```

> The last two variants (`UnreachableDowncast`, `NonExhaustiveMatch`) are
> the *optional* quality-of-life diagnostics the plan calls out as "not a
> hard error." Implement them as warnings: add a `pub severity:
> Severity` field (`enum Severity { Error, Warning }`) to `SemanticError`
> so `hulk-cli` (Step 12) can print them without aborting on success, and
> so `analyze` only returns `Err` when at least one `Severity::Error`
> exists. Treating these as a real, typed part of `SemanticErrorKind`
> instead of an ad-hoc side channel keeps the "implement everything in the
> plan, including optional parts" requirement honest.

Implement `Display` for both `SemanticError` and `SemanticErrorKind`
mirroring `ParseError`'s format: `"semantic error at line {line}, col
{col}: {message}"` (warnings can say `"semantic warning at ..."`).

---

## Step 6 — Typed-tree aliases and `VerifiedProgram`

**File to modify:** `crates/hulk-semantic/src/typed.rs`

**Purpose:** name the specific instantiation of `hulk-ast`'s now-generic
tree (Step 0) that this crate produces, without declaring any new struct.

```rust
pub type TypedExpr = hulk_ast::Expr<crate::types::Type>;
pub type TypedProgram = hulk_ast::Program<crate::types::Type>;
```

**File to modify:** `crates/hulk-semantic/src/lib.rs` (struct definition;
the `analyze` function itself is Step 11)

```rust
pub struct VerifiedProgram {
    pub registry: TypeRegistry,
    pub typed_program: TypedProgram,
}
```

`registry` carries every resolved type/protocol/function signature ready
for `hulk-codegen`'s object-layout and v-table construction.
`typed_program` is the *same tree shape* `hulk-parser` produces, with
`Type` instead of `()` in every `anno` field — guaranteed by the type
system to be fully annotated, no `Option` to unwrap.

---

## Step 7 — Pass 0: Declaration Collection

**File to modify:** `crates/hulk-semantic/src/passes/collect.rs`

**Responsibility:** populate `TypeRegistry` with every global function,
type, and protocol *signature* — no body, method body, or attribute
initializer is inspected yet. This solves the forward-reference problem
(§A.3.1: a function may call another function defined later in the file)
structurally: by the time any body is checked (Steps 9–10), every name is
already visible regardless of source order.

```rust
pub fn run(
    program: &hulk_ast::Program,         // Program<()>
    registry: &mut TypeRegistry,
    errors: &mut Vec<SemanticError>,
);
```

**What it does, per declaration:**

- **`FunctionDecl`** → build a `FunctionSignature` (params/return type
  start as `Type::Unknown` if unannotated — Step 9 fills them in) and
  insert into `registry.functions`.
  - If the name already exists (user function or builtin) →
    `DuplicateFunction`. HULK has no overloading (§A.3.1).
  - Within the parameter list, if two parameters share a name →
    `DuplicateParameter`.
- **`TypeDecl`** → build a `TypeInfo` with its constructor parameters and
  one entry per member (attributes → `AttributeInfo` with `declared_type:
  None` if unannotated; methods → `MethodSignature`).
  - Duplicate type name → `DuplicateType`.
  - Duplicate attribute/method name within the same type →
    `DuplicateAttribute`.
  - Duplicate parameter name within one constructor or method parameter
    list → `DuplicateParameter`.
- **`ProtocolDecl`** → build a `ProtocolInfo`. Per §A.10.1, *every* method
  declaration in a protocol must be fully typed (protocols have no body to
  infer from) — verify every `ProtocolMethod`'s params and return type are
  present; this is enforced **immediately**, here, not deferred to later
  passes, since there is no later pass that could ever fill in a missing
  protocol-method type.

**What it deliberately does not do:** report any *type* error (a type
mismatch, an undefined reference inside a body). Only *shape* errors
(duplicates, missing protocol annotations) belong here — this pass exists
purely to make every global name visible before any body is examined.

---

## Step 8 — Pass 1: Hierarchy & Protocol Resolution

**File to modify:** `crates/hulk-semantic/src/passes/hierarchy.rs`

**Responsibility:** resolve every `inherits`/`extends` link collected in
Step 7 into a validated, linked tree, and make `Type::conforms_to`
well-defined.

```rust
pub fn run(registry: &mut TypeRegistry, errors: &mut Vec<SemanticError>);
```

Implement these checks, in this order (each builds on the previous one
being sound):

1. **8.1 Parent existence.** For every `TypeInfo` with a `ParentLink`,
   confirm `registry.lookup_type(parent.name)` exists. Otherwise emit
   `InheritFromUndefinedType`.
2. **8.2 No inheriting from value types.** Reject a `ParentLink` whose name
   is `Number`, `String`, or `Boolean` (§A.7.3: *"it is a semantic error to
   inherit from these types"*) → `InheritFromBuiltinValueType`.
3. **8.3 Cycle detection.** For each type, walk its parent chain with a
   visited-name set; a repeat → `InheritanceCycle(path)`. Because
   `TypeParent` is a single optional field (not a list — HULK has no
   multiple inheritance), the only possible cycle shape is a simple chain,
   so this check is linear per type.
4. **8.4 Override signature compatibility.** For every type with a parent,
   for every method the type redeclares that the parent also defines,
   require the **exact same** parameter types and return type (§A.7.4 —
   note this is *stricter* than protocol variance; class-to-class
   overriding has no contravariance/covariance relaxation). Mismatch →
   `InvalidOverride { method, in_type, expected, found }`. Implement this
   as its own function, separate from protocol variance (next item) —
   they must not share code, since their rules differ.
5. **8.5 Protocol extension validity.** For every `ProtocolInfo` with
   `extends`, the parent protocol's methods may only be: inherited
   unchanged, or narrowed within variance (`extends`-defined method's
   parameters **contravariant**, return type **covariant** relative to the
   parent's). A protocol extension may never *remove* a method. Violation
   → `InvalidProtocolVariance`.
6. **8.6 Flatten attribute/method tables.** For every `TypeInfo`, in
   parent-to-child order (a type's parent must already be flattened before
   the type itself — process types in topological order using the
   acyclic chains from step 8.3), copy the parent's `attributes` and
   `methods` maps into the child's, then let the child's own members
   overwrite same-named inherited entries. After this step, member lookup
   in Step 10.8 is a single `HashMap` access — no chain walk needed at
   check time, and `hulk-codegen`'s future `.TYPES` flattening can reuse
   this exact structure directly.

After this pass returns with no errors, `registry.is_ancestor` and
`registry.implements_protocol` (Step 3.3) are guaranteed well-defined, and
`Type::conforms_to` (Step 2.2) can be safely called by every subsequent
pass.

---

## Step 9 — Pass 2: Type Inference

**File to modify:** `crates/hulk-semantic/src/passes/infer.rs`

**Responsibility:** assign a concrete `Type` to every expression and to
every unannotated symbol declaration, *while building* the `TypedProgram`
(Step 6) node by node. This is the synthesized-attribute evaluator: a
post-order traversal where each visit returns `(TypedExpr, Type)` — the
freshly built typed node and its type — using an `Environment` (Step 4)
threaded down as the inherited attribute.

```rust
pub fn run(
    program: &hulk_ast::Program,            // Program<()>
    registry: &mut TypeRegistry,
    errors: &mut Vec<SemanticError>,
) -> TypedProgram;
```

### 9.1 Expression visitor

Implement one function per `ExprKind` variant (mirroring the
`parse_*`-per-nonterminal convention already used in `hulk-parser`),
all going through a single dispatcher:

```rust
fn infer_expr(
    expr: &hulk_ast::Expr,
    env: &mut Environment,
    registry: &mut TypeRegistry,
    errors: &mut Vec<SemanticError>,
) -> TypedExpr;
```

Per-construct rules (each builds the corresponding `TypedExpr` node with
its `anno` set to the resulting `Type`):

- **Literals** (§A.9.2) — trivial: `Number(_) → Type::Number`,
  `String(_) → Type::String`, `Boolean(_) → Type::Boolean`. No environment
  lookup needed.
- **`Variable(name)`** — `env.lookup(name)`; not found →
  `UndefinedVariable`, type becomes `Type::Error`. HULK has no global
  variables, so this never touches `TypeRegistry`.
- **`SelfRef`** — valid only when the current method scope bound `self`
  (Step 9.4 establishes this binding when entering a method body); its
  type is the enclosing type. Outside a method → `UndefinedVariable` (no
  global `self`).
- **`BaseRef`** — valid only inside a method that *overrides* a method of
  the same name in the parent (checked via
  `registry.lookup_type(parent).methods.get(name)`); resolves to that
  signature. Otherwise → `BaseOutsideOverridingMethod`.
- **Unary/Binary operators** — implement exactly this table (§A.2.1,
  A.2.2, A.5); any operand combination not listed → `InvalidOperator`:

  | Operators | Required operand type(s) | Result |
  |---|---|---|
  | `+ - * / % ^` (binary), unary `-` | `Number` (both/one) | `Number` |
  | `< <= > >= == !=` | `Number, Number` | `Boolean` |
  | `& \|` (binary), unary `!` | `Boolean` (both/one) | `Boolean` |
  | `@`, `@@` | each operand in `{Number, String, Boolean}` | `String` |

  > Design note to preserve: hulk-docs does not define `==`/`!=` or
  > `@`/`@@` generically over arbitrary `Object`-rooted types. Restrict
  > `==`/`!=` to `Number` operands and `@`/`@@` to the three builtin value
  > types, matching every spec example exactly. Do not silently extend
  > this — if broader equality/concatenation is wanted later, it is a
  > deliberate language-design decision, not a bug fix.
- **`Let`** — bindings visited strictly **left to right**; each
  binding's initializer is inferred *before* `env.declare` runs for that
  binding, then declared in the *current* (still-open) scope so the
  **next** binding's initializer can see it (§A.4.2). If a binding has a
  type annotation, check the initializer's type `conforms_to` it
  (`NotConforming` otherwise) and use the annotation as the declared type;
  otherwise use the inferred type. After all bindings, `env.push_scope()`
  is *not* needed again (bindings already share one open scope) — infer
  the `body` in that same scope, then `env.pop_scope()`. The `Let`'s own
  type is the body's type.
- **`Assign`** — resolve the target:
  - `AssignTarget::Variable(name)` — must already be declared; `self` is
    never assignable (`SelfIsNotAssignable`); a name in scope but bound to
    a function/type rather than a variable → `InvalidAssignTarget`; not
    found → `UndefinedVariable`.
  - `AssignTarget::Member`/`Index` — delegate to the same rules as read
    access (Step 9.7/9.8) plus a conformance check against the new value.
  The new value's type must `conforms_to` the target's declared type. The
  whole `Assign` expression's type is the value's type (per §A.4.6, `:=`
  returns the assigned value).
- **`Block`** — no new scope (§A.4 — blocks are pure sequencing, see Step
  4). Infer each sub-expression in order; the block's type is the last
  expression's type (or `Type::Object` for an empty block, matching "no
  side effect" programs like `42;`).
- **`If`** — every condition (`condition`, each `elif`) must infer as
  `Boolean` (`NonBooleanCondition` otherwise). The expression's type is
  `lowest_common_ancestor` of `then_branch`, every elif body, and
  `else_branch` (Step 2.3). The grammar guarantees `else` is always
  present, so there is no "missing else" case.
- **`While`** — condition must be `Boolean`; type is the body's type
  (§A.6.1).
- **`For`** — infer `iterable`; it must implement the `Iterable` protocol
  (`registry.implements_protocol`) or be a `Vector<T>`/`Iterable<T>`
  (`NotIterable` otherwise). Determine the loop variable's **covariant**
  element type by reading the *concrete* `current()` signature off the
  iterable's resolved type (not the abstract `Iterable` protocol's generic
  `Object`) — e.g. a `Range` yields `Number`, per §A.11.1. `push_scope`,
  declare the loop variable with that type, infer `body`, `pop_scope`. The
  `For`'s type is the body's type.
- **`Call`** — resolve the callee:
  - A bare `Variable` callee names a global function: look up
    `FunctionSignature` in the registry; arity mismatch →
    `ArityMismatch`; each argument's type must conform to the
    corresponding parameter type; not found → `UndefinedFunction`.
  - Any other callee shape (after `Member` access) is a method call,
    handled together with `MemberExpr` below.
  Result type is the signature's return type.
- **`Member`** (read, `obj.member`) — infer `object`; its type must
  resolve to a known type/protocol. Look up `member` in the (flattened,
  Step 8.6) method or attribute table.
  - **Attributes are private** (§A.7): reading `obj.attr` is only legal
    when this access occurs literally as `self.attr` *inside a method of
    that exact type* — reject cross-instance and external reads, even
    from a subclass, with `UnknownMember`. The simplest correct check: the
    `object` sub-expression's `ExprKind` is `SelfRef`, *and* the attribute
    exists on the type currently bound to `self` in scope.
  - **Methods are always public** — no such restriction; just verify the
    method exists. If the `Member` is immediately the callee of a `Call`,
    treat the whole chain as one method invocation: check the call's
    arguments against the method's parameters, result type is the
    method's return type (this is the "method call" half of the `Call`
    rule above).
- **`New`** — the named type must exist (`UndefinedType` otherwise);
  constructor argument count/types must conform to the type's declared
  constructor parameters (`ArityMismatch`/`NotConforming`). Type is
  `Type::Named(type_name)`.
  *(Attribute-initializer and parent-constructor-argument scoping rules
  for `New`/`TypeDecl` are cross-cutting with Step 9.5 below — implement
  them together.)*
- **`TypeTest`** (`is`) — receiver must be `Object`-rooted (not one of
  `Number`/`String`/`Boolean`, which have no dynamic subtyping per
  §A.8.4); named type must exist. Always `Boolean`.
- **`Downcast`** (`as`) — same receiver restriction; named type must
  exist. Statically typed as the named type (§A.8.6 — runtime failure is
  *not* a semantic error). **Optional diagnostic to implement** (do not
  skip): if the receiver's static type and the target type are provably
  unrelated in the hierarchy (neither `conforms_to` the other), push a
  `Severity::Warning` `UnreachableDowncast` — the cast can never succeed.
- **`Vector::Literal`** — infer every item; type is
  `Vector(lowest_common_ancestor(items))`. Empty literal → `Vector(Unknown)`;
  if an enclosing context (e.g. a `let` annotation) can resolve `Unknown`,
  do so, otherwise `CannotInferType`.
- **`Vector::Comprehension`** — infer `iterable` exactly like `For`'s
  iterable; `push_scope`, declare the bound variable with its covariant
  element type, infer the head expression, `pop_scope`. Type is
  `Vector(head expression's type)`.
- **`Index`** — object's type must be `Vector(T)` (`IndexOnNonVector`
  otherwise — indexing is **not** part of the generic `Iterable` protocol,
  only of the concrete vector type per §A.12.3); index expression must be
  `Number`; result type is `T`.
- **`Match`** (project extension, no normative spec — implement per Step
  9.6 below, kept separate because it is not in `hulk-docs.pdf`).

### 9.2 Declaration visitor

```rust
fn infer_function(decl: &FunctionDecl, registry: &mut TypeRegistry, errors: &mut Vec<SemanticError>) -> FunctionDecl<Type>;
fn infer_type(decl: &TypeDecl, registry: &mut TypeRegistry, errors: &mut Vec<SemanticError>) -> TypeDecl<Type>;
```

- **`infer_function`** — fresh `Environment`; declare each parameter
  (with its annotation, or `Type::Unknown` placeholder if absent — see
  9.3); infer the body; if the parameter was unannotated, attempt the
  bounded inference strategy (9.3) from how it was used in the body; if
  the function's return type was unannotated, it **is** the body's
  inferred type (subject to the recursive-function algorithm below).
- **`infer_type`** — for each attribute: infer the initializer in a scope
  containing **only** the global namespace and the type's own constructor
  parameters (not `self`, not sibling attributes — §A.7.2: order of
  attribute initialization is unspecified, so attributes cannot see each
  other). For each method: fresh scope with the method's parameters plus
  `self` bound to `Type::Named(type_name)`. For the `ParentLink.args` (if
  any): infer each in a scope containing the global namespace plus the
  **inheriting** type's own constructor parameters — never the parent's.

### 9.3 Bounded parameter/return inference

A parameter or return type is inferable, without a general solver, exactly
when its uses pin it to one type:
- It is an operand of an arithmetic/comparison/boolean/concatenation
  operator → the operator table (9.1) fixes it unambiguously.
- It is passed as an argument to a call whose corresponding parameter is
  already typed → take that parameter's type.
- It is the receiver of a member access whose member name resolves to
  exactly one type/protocol across the whole registry → take that type.

If two different uses would require *incompatible* types →
`AmbiguousInference` (§A.9.3: "if more than one type ... would be
consistent, the type inferer must fail" — do not guess). If no use pins a
type at all → `CannotInferType`.

### 9.4 `self` / `base` resolution

When opening a method body's scope, `env.declare("self", Type::Named(owner), span)`
immediately — no special-casing anywhere else in the visitor; `self` is
just a normal binding with type. `base` is resolved by name in the
*parent* type's flattened method table (Step 8.6); if the current method
has no parent override of the same name, `BaseOutsideOverridingMethod`.

### 9.5 Recursive functions without an explicit return type

Implement exactly this bounded "one-pass-with-a-placeholder" strategy —
do not implement a general fixed-point solver:

1. Before visiting a function's body, register its signature with
   `return_type = Type::Unknown` if unannotated.
2. Visit the body. A recursive call to the function being inferred
   observes `Unknown` as its result type; this propagates as `Unknown`
   through the surrounding expression rather than failing immediately
   (an "optimistic placeholder").
3. After the body is fully visited:
   - If the body's resulting type is concrete (no `Unknown` reached the
     top) — accept it as the inferred return type. This covers
     `fib(n) => if (n==0|n==1) 1 else fib(n-1)+fib(n-2)` (§A.9.4): the
     base-case branch (`1`) already pins the type to `Number` without
     needing the recursive call's result up front.
   - If `Unknown` is still present at the top — the recursive result was
     load-bearing and never resolved. Emit `CannotInferType` and require
     an explicit annotation (§A.9.3's documented fallback).

### 9.6 `match` expression rules (project extension)

Implemented here, in the inference pass, exactly like a generalized `if`:

- Infer the scrutinee `value` once; let `S` be its type.
- For each `MatchCase`:
  - `Pattern::Wildcard` — always matches, no binding.
  - `Pattern::Literal(lit)` — the literal's type must equal `S` exactly;
    otherwise `TypeMismatch` (it could never match).
  - `Pattern::Variable(name)` — always matches; `push_scope`, declare
    `name: S` for the case body only, `pop_scope` after the body.
  - `Pattern::Type(type_ref, alias)` — `S` must be `Object`-rooted (same
    restriction as `is`/`as`); the case body is checked as if `value` had
    been downcast to `type_ref`; if `alias` is present, `push_scope`,
    declare it with the narrowed type for the case body only, `pop_scope`.
- The whole expression's type is `lowest_common_ancestor` of every case
  body's type.
- **Exhaustiveness is not statically enforced** (mirrors `as`'s documented
  runtime-failure semantics). **Optional diagnostic to implement:** if no
  case is `Wildcard` or bare `Variable`, push a `Severity::Warning`
  `NonExhaustiveMatch` — do not block compilation, but surface the risk.

---

## Step 10 — Pass 3: Type Checking

**File to modify:** `crates/hulk-semantic/src/passes/check.rs`

**Responsibility:** Pass 2 already built a fully-typed `TypedProgram` (no
`anno` is ever absent). Pass 3 only *reads* `&TypedProgram` and
`&TypeRegistry` — it builds nothing further — and verifies **consistency**:
every annotation conforms to its inferred counterpart, every operator/call/
assignment/member access that Pass 2 already typed is also *legal*.

```rust
pub fn run(
    typed_program: &TypedProgram,
    registry: &TypeRegistry,
    errors: &mut Vec<SemanticError>,
);
```

In practice, most of the individual rules enumerated in Step 9 (operator
operand types, arity, attribute privacy, `is`/`as` receiver restrictions,
etc.) are most naturally *detected* during Pass 2's bottom-up traversal,
since that is where both operand types are first known. This guide's Step
9 therefore already raises the corresponding `SemanticErrorKind` inline,
at the point of inference, rather than deferring every check to a second
full traversal. Keep Pass 3 as a **second, focused traversal** whose sole
job is the checks that genuinely require the *complete*, already-typed
tree to be available — i.e. checks that compare an inferred type against
a type that could only be settled after the rest of inference finished:

- **Annotation vs. inferred-type conformance**, globally: for every
  `let` binding, attribute, function/method parameter, and return type
  that carried an *explicit* annotation, re-confirm (now against the
  fully resolved `TypedProgram`, after recursive placeholders from Step
  9.5 have settled) that the inferred type still conforms to it. This
  catches cases where Step 9.5's optimistic placeholder resolved to a
  type that, in hindsight, conflicts with an explicit annotation written
  elsewhere in a mutually recursive group.
- **Protocol conformance at the point of use**: every place a value of a
  nominal type is passed where a protocol type is expected (parameter,
  return, assignment), confirm `registry.implements_protocol` holds —
  Pass 2's `conforms_to` calls already do this per-expression, but Pass 3
  re-verifies it for declarations as a whole (e.g. a function declared to
  return a protocol type, whose body's *concrete* returned type is only
  fully known after every branch of inference has completed).
- **Final sweep for unresolved placeholders**: confirm no `Type::Unknown`
  remains anywhere in `typed_program`. Any survivor is a Step 9.5 case
  that should have already produced `CannotInferType`, but this assertion
  pass exists as a safety net so a future bug in Step 9 fails loudly in
  tests rather than silently shipping an `Unknown` to `hulk-codegen`.

Keep this pass intentionally small: its existence is about correctness
under the *cascading/forward-reference* edge cases that span across Pass
2's single traversal order, not about duplicating every rule already
implemented in Step 9.

---

## Step 11 — Wire the driver (`analyze`)

**File to modify:** `crates/hulk-semantic/src/lib.rs`

```rust
pub fn analyze(program: &hulk_ast::Program) -> Result<VerifiedProgram, Vec<SemanticError>> {
    let mut errors = Vec::new();
    let mut registry = types::registry::seeded_registry();

    passes::collect::run(program, &mut registry, &mut errors);
    passes::hierarchy::run(&mut registry, &mut errors);

    // Early exit: a broken hierarchy invalidates `conforms_to` itself.
    // Continuing would only produce misleading cascade errors.
    if errors.iter().any(|e| e.severity == Severity::Error) {
        return Err(errors);
    }

    let typed_program = passes::infer::run(program, &mut registry, &mut errors);
    passes::check::run(&typed_program, &registry, &mut errors);

    if errors.iter().any(|e| e.severity == Severity::Error) {
        Err(errors)
    } else {
        Ok(VerifiedProgram { registry, typed_program })
    }
}
```

**Purpose of each call, in order:**
1. `seeded_registry()` (Step 3.2) — every builtin name exists before any
   user declaration is even looked at.
2. `collect::run` (Step 7) — every global signature visible, regardless of
   declaration order.
3. `hierarchy::run` (Step 8) — inheritance/protocol links resolved and
   validated; `conforms_to` becomes safe to call.
4. **Early-return gate** — the *only* short-circuit in the whole driver,
   justified purely by cascade suppression (a broken type tree poisons
   every later type comparison). Steps 2–3 within Pass 2/3 themselves must
   **never** early-return on the first error — accumulate everything,
   per the "multiple errors, not first error" requirement.
5. `infer::run` (Step 9) — builds the typed tree.
6. `check::run` (Step 10) — consistency sweep over the typed tree.
7. Final result — `Ok` only if zero `Severity::Error` diagnostics exist;
   `Severity::Warning` diagnostics (Step 5) are returned too (extend
   `VerifiedProgram` with a `pub warnings: Vec<SemanticError>` field, or
   return them alongside `Ok` via a tuple) so `hulk-cli` (Step 12) can
   still print them on a successful compile.

---

## Step 12 — Integrate with `hulk-cli`

**File to modify:** `crates/hulk-cli/src/main.rs`

**Current behavior (per `PRE-SEMANTIC_STATUS.md`):** `Lexer::tokenize` →
`hulk_parser::parse` → print the AST with `{:#?}`.

**New behavior to implement:**

1. Run `hulk_semantic::analyze(&program)` immediately after a successful
   parse.
2. On `Err(errors)` — print every `SemanticError` (not just the first),
   using the `Display` impl from Step 5, one per line, then exit with a
   non-zero status. Do **not** print the AST in this branch — a program
   that fails semantic analysis has no verified output worth showing.
3. On `Ok(verified)` — print any accumulated warnings first (clearly
   labeled, e.g. `warning: ...`), then print confirmation plus either the
   existing AST debug dump or, optionally, `verified.typed_program`'s
   debug dump (more useful now that every node carries a `Type`).

This is intentionally the **last** step: it depends on every previous
module compiling and `analyze` existing with a stable signature.

---

## Step 13 — Testing strategy & fixtures

**Files to create:** inline `#[cfg(test)] mod tests` blocks in each
`passes/*.rs` and `types/*.rs` file, following the existing convention in
`hulk-ast`/`hulk-parser` (test through the public API:
`Lexer::tokenize → hulk_parser::parse → hulk_semantic::analyze`).

Cover every group below — each one maps to a section of this guide, so a
missing group means a missing implementation, not just a missing test:

| Test group | Exercises | Fixture source |
|---|---|---|
| Name resolution | Step 9.1 (`Variable`, `SelfRef`), forward references, shadowing | §A.4.5 worked examples, used verbatim |
| Redeclaration | Step 7 | Synthetic duplicate-name programs |
| Inheritance | Step 8.1–8.4 | Valid chain, cycle, value-type inheritance, override mismatch, `base` (Person/Knight, §A.7.4, verbatim) |
| Conforming relation | Step 2.2 | Reflexivity, transitivity, LCA over a small synthetic hierarchy, Hashable/Person structural conformance (§A.10.2, verbatim) |
| Operators | Step 9.1 operator table | Every row, success and rejection case |
| Control flow | Step 9.1 `If`/`While`/`For` | Branch unification, non-boolean condition rejection, `for`-over-`Range` covariant typing |
| Inference | Step 9.3, 9.5 | `fib`, `fact`, `let x = 42` (§A.9.4, verbatim); a resolvable and an unresolvable recursive shape |
| Vectors/indexing | Step 9.1 | Literal LCA typing, comprehension element typing, indexing-on-non-vector rejection |
| `is`/`as`/`match` | Step 9.1, 9.6, warnings | Dynamic test restricted to `Object`-rooted types, downcast typing, unreachable-downcast warning, non-exhaustive-match warning |
| Multi-error reporting | Step 11 | One fixture with several unrelated mistakes; assert all are reported and no spurious cascades appear (i.e. an `UndefinedVariable` does not also produce a `TypeMismatch` for everything built on top of it, thanks to `Type::Error`) |

---

## Step 14 — Delivery order checklist

Implement and land in exactly this order; each phase should leave
`cargo test --all` green before the next begins.

| Phase | Steps covered | Gate before moving on |
|---|---|---|
| 0 | Step 0 (AST refactor) | `hulk-ast`/`hulk-parser` tests unchanged and passing |
| 1 | Steps 1–7, plus Step 9/10 limited to: literals, variables, operators, `let`/`:=`, blocks, `if`/`elif`/`else`, `while`, `for` over `Range`, global functions (no types yet) | `hulk-semantic` tests for this subset pass |
| 2 | Step 8 (hierarchy), `self`/`base` (9.4), `New`, member access/assignment with privacy, `is`/`as` | Inheritance + privacy test group green |
| 3 | Protocols (8.5, 3.3 `implements_protocol`), vectors (9.1), `T*`/`T[]` sugar (already parsed — just confirm registry/type-side handling) | Protocol + vector test groups green |
| 4 | `match` (9.6) | `is`/`as`/`match` test group green |
| 5 | Step 12 (`hulk-cli` integration) | Manual run on a sample `.hulk` file shows correct diagnostics or confirmation |
| Future (explicitly out of scope here) | General protocol-synthesis inference (§A.9.5); functor sugar `(T) -> R`; macro semantics (`def`, `@symbol`, `$symbol`, structural pattern matching) | Blocked on `hulk-parser` support — do not attempt inside `hulk-semantic` until the parser produces the relevant nodes |

---

## Appendix — Quick file/responsibility index

| File | Owns |
|---|---|
| `crates/hulk-ast/src/lib.rs` | Parametrized tree (`Expr<A>`, `Program<A>`, …) |
| `crates/hulk-semantic/src/types/mod.rs` | `Type`, `conforms_to`, `lowest_common_ancestor` |
| `crates/hulk-semantic/src/types/registry.rs` | `TypeRegistry`, `TypeInfo`, `ProtocolInfo`, `FunctionSignature`, builtin seeding, query helpers |
| `crates/hulk-semantic/src/environment.rs` | `Environment`, `Binding` — scope stack |
| `crates/hulk-semantic/src/error.rs` | `SemanticError`, `SemanticErrorKind`, `Severity`, `Display` |
| `crates/hulk-semantic/src/typed.rs` | `TypedExpr`, `TypedProgram` aliases |
| `crates/hulk-semantic/src/passes/collect.rs` | Pass 0 |
| `crates/hulk-semantic/src/passes/hierarchy.rs` | Pass 1 |
| `crates/hulk-semantic/src/passes/infer.rs` | Pass 2 (+ `match`, recursion, `self`/`base`) |
| `crates/hulk-semantic/src/passes/check.rs` | Pass 3 |
| `crates/hulk-semantic/src/lib.rs` | `analyze`, `VerifiedProgram`, module wiring |
| `crates/hulk-cli/src/main.rs` | Calls `analyze`, prints diagnostics |
