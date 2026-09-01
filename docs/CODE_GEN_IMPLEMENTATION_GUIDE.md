# HULK Code Generation — Implementation Guide

**Purpose of this document.** This is the execution guide for the code-generation
phase of the HULK compiler. It fixes the architecture, the runtime data
representation, the memory-management model, and an ordered, checkable sequence of
implementation phases, ending at a working `./output` binary that satisfies the
project's delivery requirements (§3). Every design decision below is final for v1
unless explicitly marked as revisitable future work (§7, Phase 11). Where a decision
genuinely depends on the machine the team builds on (for example, the installed LLVM
version) rather than on the language or the architecture, this guide states the
default we commit to and the fallback procedure, so implementation is never blocked
waiting on a judgment call (§9).

---

## 1. Current State of the Compiler Pipeline

```
source text → hulk-lexer → hulk-parser → hulk-semantic → hulk-codegen → ./output
                 (done)        (done)         (done)        (this guide)
```

- **`hulk-ast`** defines the generic syntax tree, parametrized over an annotation type
  (`Expr<A>`, `Program<A>`). It has no dependencies and is shared by every later
  stage.
- **`hulk-lexer`** and **`hulk-parser`** are complete and produce an untyped
  `Program<()>` via a hand-written LL(1) recursive-descent parser.
- **`hulk-semantic`** is complete. Its single entry point is:

  ```rust
  pub fn analyze(program: &hulk_ast::Program) -> Result<VerifiedProgram, Vec<SemanticError>>

  pub struct VerifiedProgram {
      pub registry: TypeRegistry,        // every resolved type/protocol/function signature
      pub typed_program: TypedProgram,   // Program<Type> — every expression carries a resolved Type
      pub warnings: Vec<SemanticError>,  // non-fatal diagnostics
  }
  ```

  `analyze` runs five passes (declaration collection, hierarchy/protocol resolution,
  constructor-parameter inference, type inference, and final checking) and guarantees
  that a successfully returned `VerifiedProgram` contains no unresolved or
  ill-typed expression anywhere in the tree.
- **`hulk-codegen`** is the crate this guide fills in: today it is an empty,
  scaffolded crate with no dependencies and no code beyond a top-level doc comment.
- **`hulk-cli`** currently wires only the front end together: it lexes, parses, calls
  `analyze`, and on success pretty-prints the typed AST. It does not yet invoke code
  generation, does not produce a binary, and has no notion of an output path or
  optimization level — wiring it to `hulk-codegen` is part of this guide (§7, Phase
  9).

**Workspace conventions already in place and binding for new crates.** The root
workspace manifest does not define a `[workspace.dependencies]` table; every crate
pins its own third-party dependencies directly in its own `Cargo.toml` (`hulk-cli`
pins `clap`; `hulk-semantic` pins `indexmap`). `hulk-codegen` and `hulk-rt` follow the
same convention: `inkwell` is pinned directly in `hulk-codegen/Cargo.toml`, not at the
workspace root. No LLVM-related crate is part of the dependency graph yet — this is
expected, since the LLVM toolchain itself has not been provisioned on the build
machine before this guide's Phase 0.

---

## 2. Delivery Requirements

The following requirements define a complete, accepted code-generation phase. They
are restated here as fixed engineering targets, not as items to be checked against an
external document:

| # | Requirement |
|---|---|
| 1 | `make build` produces a `./hulk` executable at the repository root. |
| 2 | `./hulk <file.hulk>`, on a program that passes lexing, parsing, and semantic analysis, produces an executable `./output` in the current directory, runnable on Linux x86_64, with no manual linking step required from the user. |
| 3 | On a program that fails lexing, parsing, or semantic analysis, `./hulk` exits with code `1` (lexical), `2` (syntactic), or `3` (semantic) — whichever is the *most fundamental* error class present, evaluated in that priority order regardless of which other error classes might also apply. |
| 4 | Every diagnostic line is printed to stderr in the exact format `(line,col) TYPE: message`, 1-based line/column numbers, `(0,0)` when no source position is available. |
| 5 | `REPORT.md`, at the repository root, is at least 2000 words and documents the actual design decisions made during this phase (the boxing strategy, the vtable/itable split, the hybrid memory-management model, the entry-point convention) rather than restating the pipeline shape already visible in the source. |
| 6 | Runtime errors produced by `./output` itself (as opposed to failures of `./hulk` to compile the program) are outside the scope of compiler correctness and are not part of this guide's "done when" criteria. |

Two consequences worth stating explicitly because they are easy to lose track of once
implementation starts:

1. **Requirement 4 is not satisfied today and is not solely a `hulk-codegen` concern.**
   `hulk-semantic`'s current `SemanticError` rendering produces
   `"semantic error at line {line}, col {col}: {message}"`, not
   `(line,col) SEMANTIC: message`. The same gap exists in `hulk-lexer` and
   `hulk-parser`. §8 gives the exact change required in each crate.
2. **Requirement 3 only defines exit codes for the three front-end error classes.**
   It is silent on what `./hulk` should exit with if `hulk-codegen` itself fails on a
   program that already passed semantic analysis (an internal compiler error, as
   opposed to a user-facing error in the HULK program). §9 commits to a concrete
   answer rather than leaving this unresolved.

---

## 3. Architecture Overview

```
   VerifiedProgram { registry, typed_program }
                     │
                     ▼
        hulk-codegen::lower   (Phases 2–7)   — pure: TypedAST + Registry → LLVM Module
                     │
                     ▼
        LLVM pass manager      (Phase 8)      — verify + optimize
                     │
                     ▼
        TargetMachine emission (Phase 9)      — Module → object file (.o)
                     │
                     ▼
        link with hulk-rt.a    (Phase 9)      — system `cc`
                     │
                     ▼
              ./output (Linux x86_64 ELF)
```

Two crates carry this work, and they are kept separate deliberately:

- **`hulk-codegen`** — pure compiler logic. Depends on `hulk-ast`, `hulk-semantic`,
  and `inkwell`. No I/O beyond writing the files it is explicitly asked to write
  (`.o`, optionally `.ll` for debugging). This crate does not exist as working code
  yet and is the primary subject of this guide.
- **`hulk-rt`** — a small Rust static library (`#[no_mangle] pub extern "C" fn ...`,
  `crate-type = ["staticlib", "rlib"]`) providing the runtime primitives that
  generated IR calls by name: allocation, reference counting, the tracing garbage
  collector, string/vector operations, math wrappers, and the runtime traps for
  non-exhaustive `match` and failed `as` downcasts. Built once, archived, and linked
  into every compiled HULK program. This crate is new and is added to the workspace
  in Phase 0.

`hulk-codegen` follows the same internal module shape `hulk-semantic` already
established for its own passes — small, single-responsibility modules, each exposing
a `run`/free function:

```
crates/hulk-codegen/
├── Cargo.toml
├── README.md
└── src/
    ├── lib.rs                  – public API: compile(&VerifiedProgram, CodegenOptions) -> Result<...>
    ├── error.rs                – CodegenError (mirrors SemanticError's shape: kind + span)
    ├── context.rs              – CodegenCtx: inkwell Context/Module/Builder + shared maps
    ├── types.rs                – Type -> inkwell type lowering table, struct layouts
    ├── runtime_decls.rs        – extern "C" declarations for every hulk-rt symbol
    ├── layout/
    │   ├── mod.rs
    │   ├── vtable.rs            – per-type vtable + GC field-map construction
    │   └── itable.rs            – per-(type, protocol) interface tables
    ├── lower/
    │   ├── mod.rs               – orchestration: declarations pass, then entry
    │   ├── decl.rs              – FunctionDecl / TypeDecl -> LLVM function/struct defs
    │   ├── expr.rs              – ExprKind -> BasicValueEnum (the bulk of lowering)
    │   ├── pattern.rs           – Match/Pattern lowering
    │   └── ctor.rs              – `new T(args)` construction sequence
    ├── optimize.rs              – LLVM pass-manager wiring
    └── emit.rs                  – TargetMachine setup, .o emission, link-driver invocation
```

This layout gives each `lower/*.rs` function a natural unit-test seam: lower a
hand-built `TypedExpr` fragment and assert on the emitted `.ll` text, the same way
`hulk-semantic`'s own passes are tested by asserting on `SemanticErrorKind`s.

---

## 4. What the Front End Already Guarantees

These properties of `hulk-ast`/`hulk-semantic` are load-bearing for the design in this
guide — codegen is written to rely on them rather than re-verify them:

1. **`flattened_methods` is already linearized parent-before-child, and slot order is
   stable.** Hierarchy resolution builds each type's flattened method table by
   extending the parent's table with the child's own table; index-preserving map
   extension means an override of method `m` keeps `m`'s original slot, and a type's
   own new methods are appended after every inherited one. Combined with the rule
   that an overriding method's signature must match its parent's exactly (no
   class-level variance), `flattened_methods.keys()` in iteration order **is** the
   vtable slot order for every type, with no extra computation needed in codegen.
2. **Protocol conformance is structural and already fully verified** before codegen
   ever runs (contravariant parameters, covariant return types, checked once during
   hierarchy/type-checking). Codegen never re-checks conformance; it only encodes a
   compile-time witness for it (the interface table, §6.6).
3. **No `Type::Unknown` and no `Type::Error` can appear in a successfully returned
   `VerifiedProgram`.** The final checking pass sweeps the typed tree and reports any
   leftover `Unknown` as a hard error before `analyze` can return `Ok`. Codegen
   treats encountering either of these placeholder types as an internal compiler
   defect (`unreachable!()`/`CodegenError`, §9), never as a case to lower.
4. **Lexical scope discipline is exact and codegen must mirror it.** `let` bodies,
   function/method bodies, `for` bodies, and `match` case bodies each introduce a new
   lexical scope; a bare `{ ... }` block does not. Codegen's own local-variable scope
   stack must push and pop at exactly these points — not re-deciding shadowing rules
   independently, since `hulk-semantic` has already verified them.
5. **Attribute initializers see only the type's own constructor parameters** — never
   `self`, never sibling attributes, never inherited attributes. This means attribute
   initialization has no inter-attribute dependency: codegen evaluates every
   attribute initializer in declaration order, immediately after the constructor
   parameters are bound, with no dependency analysis required.
6. **`base.method()` resolves by signature lookup against the immediate parent only**,
   and the typed tree does not record which parent/method a given `BaseRef` resolves
   to — that context is reconstructed afresh during inference from
   "current owning type + current method name." Codegen must track the same pair
   while lowering a method body and resolve `base` calls the same way: a direct,
   non-virtual call to the immediate parent's implementation.
7. **`self` is never part of `FunctionDecl.params`.** It is bound separately during
   inference. Codegen must synthesize the implicit first parameter for every method's
   LLVM signature, typed as a pointer to the *declaring* type's struct (not the
   most-derived type — a method body only ever touches fields at offsets that are
   fixed regardless of the actual runtime subtype).
8. **`Type::Object` is a real, reachable static type**, not only a theoretical root:
   it is `print`'s parameter type, the fallback whenever an `if`/`match`'s branches
   share no closer common ancestor, and `Enumerable.iter()`'s element type. Codegen
   needs a uniform runtime representation for "a value of statically unknown concrete
   shape," not only for user-declared class instances (§6.5).
9. **A `for` loop's iterable is usually *not* statically `Iterable(T)`.** Type
   inference resolves `for`/comprehension element types uniformly across three
   shapes — `Vector(T)`, `Iterable(T)`, and any `Named` type that structurally
   implements `Iterable` — and in the common case (`for (x in range(1,10))`,
   `for (x in some_vector)`) the iterable's *static* type is a concrete named type or
   `Vector(T)`, not the protocol type. This means the common loop case devirtualizes
   to an ordinary method call on a concrete type, and the more expensive
   protocol-fat-pointer path is only needed when a value's *static* type is literally
   `Iterable(T)` or a bare protocol name.
10. **`Vector` and `Range` are registry-only builtins.** They are seeded directly into
    `TypeRegistry` with hand-built method signatures (`get`/`set`/`next`/`current` for
    `Vector`; `next`/`current` for `Range`) and are never written as a real
    `TypeDecl` in user source. Codegen hand-writes their runtime layout and method
    bodies in `hulk-rt` rather than lowering them from HULK source, because no such
    source exists (§6.8).

---

## 5. Key Design Decisions

These decisions are binding for the implementation phases in §7. Each is stated once
here with its rationale; phase-specific elaboration appears where the decision is
first implemented.

| Topic | Decision |
|---|---|
| LLVM binding | `inkwell`, pinned to the LLVM major version provisioned on the build machine (default target: LLVM 17 — see §9). |
| Target platform | Linux x86_64 only (`x86_64-unknown-linux-gnu`), host build, no cross-compilation in v1. |
| Workspace convention | Each crate pins its own dependencies directly; no root `[workspace.dependencies]` table is introduced. |
| Process exit semantics | Evaluate `entry` for its side effects, discard its resulting value, return `0`. |
| Non-exhaustive `match` at runtime | Call `hulk_rt_match_fail() -> !`: print a diagnostic with source span, then abort. |
| Failed `as` downcast at runtime | Call `hulk_rt_downcast_check`; on failure, call `hulk_rt_downcast_fail() -> !` (diagnostic + abort). `is` performs the same check but only returns the boolean result. |
| String mutability | Strings are immutable and not indexable in v1. Every `@`/`@@` concatenation allocates a new string. |
| Bare `obj.method` (no call) | A legitimate, function-typed value (`Type::Function`). Lowered as a two-word thunk: a function pointer paired with the bound receiver pointer. |
| Memory management | A hybrid model: reference counting as the fast path for immediate, deterministic reclamation, plus a periodic tracing (mark-sweep) collector that reclaims reference cycles reference counting cannot free. Full design in §6.7. |
| `@` / `@@` semantics | Both accept `Number`, `String`, or `Boolean` operands and auto-stringify non-`String` operands. `@@` inserts exactly one literal space between the two stringified operands; `@` inserts none. |
| Vector growth | Vectors are fixed-size once constructed. No `cap` field, no `push`/`append` — only `get`, `set`, `next`, `current`, matching the seeded `Vector` method set exactly. |

### 5.1 Why `inkwell` over raw `llvm-sys` or an MLIR-based toolchain (`melior`)

`llvm-sys` is the unsafe FFI layer `inkwell` itself wraps; using it directly would
mean hand-managing every null-check and string lifetime `inkwell` already handles
correctly, for a project of this size, that is pure overhead with no benefit.
`melior`/MLIR earns its complexity when a compiler needs multiple
progressively-lowered dialects — HULK is a small, single-inheritance,
structurally-typed expression language with no need for that layering, so going
directly to LLVM IR is both the simpler and the more standard choice at this scale.
`inkwell`'s typed builder API (`Context`/`Module`/`Builder`, typed
`BasicValueEnum`/`BasicTypeEnum`) maps closely onto the recursive lowering functions
this guide describes, and catches a large class of "wrong LLVM type used here"
mistakes at Rust-compile time rather than at LLVM-verification time.

### 5.2 Runtime value representation — the central decision

HULK is statically typed, so most expressions have a fully concrete static type and
compile to an **unboxed, monomorphic** representation, exactly like a conventional
ahead-of-time-compiled language. `Type::Object`, however, is a real, reachable static
type (§4, point 8), and protocol-typed positions need to represent "a value whose
exact runtime shape is not determined by the static type alone." A single uniform
boxed representation for every value would be simple but would force boxing a
`Number` on every arithmetic operation — unacceptable for a compiled-to-native
backend. Never boxing anything is unsound, because `if (b) 1 else "s"` is a legal HULK
program whose static type is `Object`, and an LLVM `phi` node at the merge block
requires both incoming values to share one LLVM type.

**Decision: per-`Type` monomorphic lowering, with boxing only at `Object`
boundaries.**

| HULK `Type` | LLVM representation | Notes |
|---|---|---|
| `Number` | `f64`, unboxed | matches `Literal::Number(f64)` directly |
| `Boolean` | `i1` in registers, `i8` in memory | standard truncation/extension at load/store boundaries |
| `String` | `ptr` to a `{ i64 len, ptr data }` heap struct | immutable; concatenation always allocates a new one |
| `Named(T)` (a class) | `ptr` to `T`'s object struct | never null — HULK has no explicit null literal |
| `Named(P)` (a protocol) | fat pointer `{ ptr data, ptr itable }` | §6.6 |
| `Vector(T)` | `ptr` to `{ ObjHeader, i64 len, ptr data }` | elements stored as uniform 8-byte pointers (§6.8) |
| `Iterable(T)` | fat pointer `{ ptr data, ptr itable }` | only materialized when a value's *static* type is literally `Iterable(T)` (§4, point 9) |
| `Object` | `ptr` to a heap `HulkBox` | the one place real boxing happens |

The `Object` box:

```rust
#[repr(C)]
struct HulkBox {
    tag: i8,       // 0=Number 1=Boolean 2=String 3=<class id> ... (drives the GC mark phase too)
    payload: i64,  // f64 bitcast to i64 for Number; bool widened to i8 then to i64; a pointer otherwise
}
```

Boxing happens only at a site whose *target* static type is `Object` (directly, or
transitively through `Vector(Object)`/`Iterable(Object)`) and whose *source* static
type is concrete. Unboxing happens only where a tag must be inspected: chiefly
`print`, and the runtime support for `is`/`as`/`match` type patterns. Arithmetic,
attribute access, and method dispatch on a concretely-typed receiver never touch this
box. This mirrors the same boxed/unboxed split production statically-typed languages
use at their dynamic boundary (Java's primitives versus `Object`, OCaml's unboxed
floats versus its polymorphic boxed values) — a standard technique, applied here at
exactly the points HULK's own type system identifies as genuinely needing it.

The `Object` box is heap-allocated unconditionally in v1; this is correct and simple,
and LLVM's own optimizer can sometimes promote short-lived boxes to the stack once
`mem2reg`/SROA see through them. Hand-rolled escape analysis for box elision is
deferred to Phase 11 and should only be pursued with measured evidence that boxing
overhead is material.

### 5.3 `hulk-rt`: the runtime support library

A focused set of `extern "C"` functions, declared in `hulk-codegen/src/runtime_decls.rs`
and implemented once in `hulk-rt`, covering exactly what generated IR cannot do with
bare LLVM instructions:

| Symbol | Purpose |
|---|---|
| `hulk_rt_alloc(size: i64) -> ptr` | the single allocation entry point; every allocation is linked into the global allocation list used by the tracing collector (§6.7) |
| `hulk_rt_retain(ptr)` / `hulk_rt_release(ptr)` | reference-count fast path: `retain` increments; `release` decrements and frees immediately at zero |
| `hulk_rt_gc_collect()` | the mark-sweep cycle collector; invoked when the allocation-byte threshold is exceeded |
| `hulk_rt_shadow_push(slot: *mut ptr)` / `hulk_rt_shadow_pop()` | root-set registration for the tracing collector |
| `hulk_rt_print_object(HulkBox)` | `print`'s implementation; switches on the box's tag |
| `hulk_rt_string_concat(a, b) -> HulkString*` | `@` |
| `hulk_rt_string_concat_space(a, b) -> HulkString*` | `@@` |
| `hulk_rt_number_to_string`, `hulk_rt_bool_to_string` | auto-stringification for mixed-type `@`/`@@` operands |
| `hulk_rt_sqrt/sin/cos/exp/log/rand` | thin wrappers, used only where no LLVM intrinsic exists for the operation on the pinned LLVM version |
| `hulk_rt_range_new/_next/_current` | the builtin `Range` type |
| `hulk_rt_vector_new/_get/_set/_next/_current` | the builtin `Vector` type |
| `hulk_rt_downcast_check(obj, target_vtable) -> bool` | runtime ancestor check backing `is`/`as`/`match` type patterns |
| `hulk_rt_match_fail() -> !` | non-exhaustive `match` trap |
| `hulk_rt_downcast_fail() -> !` | failed `as` trap |

`hulk-rt` is written in Rust for consistency with the rest of the project, compiled
as a `staticlib`, and archived once; every compiled HULK program links against this
one archive. Wherever LLVM ships an intrinsic for a builtin math function
(`llvm.sqrt.f64`, etc.), prefer it over a `hulk-rt` call — this lets the optimizer
constant-fold and reason about it in a way an opaque function call cannot; fall back
to a `hulk-rt` wrapper only where no intrinsic exists (`rand`, `range`, vector
operations).

### 5.4 String and Vector runtime layout

```rust
#[repr(C)]
pub struct HulkString { pub len: i64, pub data: *mut u8 }   // immutable; concat allocates a new instance

#[repr(C)]
pub struct HulkVector { pub header: ObjHeader, pub len: i64, pub data: *mut *mut u8 }  // no cap field
```

Every vector element is stored as a uniform 8-byte pointer, regardless of the
vector's element type: `String`/`Named`/`Iterable`-typed elements are pointers
already; `Number`/`Boolean` elements are boxed through the same `Object`-boxing path
used elsewhere. This keeps `hulk-rt`'s vector routines fully type-erased — one
generic implementation instead of one monomorphized variant per element type. This is
a deliberate simplicity-over-raw-throughput trade: it costs one extra indirection for
element types that could in principle have been stored inline (`Vector(Number)`), in
exchange for a single, simple, correct vector implementation. Revisit with profiling
data if `Vector(Number)`-heavy workloads turn out to be performance-sensitive; this is
exactly the kind of decision Phase 11 exists to revisit once the backend is otherwise
complete.

`Vector` and `Range`'s methods are hand-written `hulk-rt` functions, called directly
(never through a vtable, since both are sealed builtin types with no possible HULK-source
subtype) — never lowered from a HULK-source method body, because none exists.

---

## 6. Object Model: Layout, Dispatch, Protocols, and Memory

This section gives the full technical specification for the largest and most
consequential part of code generation: how class instances are represented, how
virtual dispatch and protocol conformance are compiled, and how memory is reclaimed.

### 6.1 Object header

Every heap object that participates in dispatch or garbage collection begins with a
common header:

```rust
#[repr(C)]
pub struct ObjHeader {
    pub ref_count: i64,
    pub gc_mark: bool,           // i8 in memory
    pub next: *mut ObjHeader,    // intrusive link in the global allocation list
    pub vtable: *const VTable,
}
```

The header carries four responsibilities in one word group:

- **`vtable`** doubles as a unique, compile-time-enumerable runtime type identity.
  `is`/`as` checks compare against a small, statically known set of vtable addresses
  (a type's ancestor chain is fully enumerable from the registry), and ordinary
  virtual calls index through it.
- **`ref_count`** drives the fast reclamation path described in §6.7.
- **`gc_mark`** and **`next`** exist purely to support the tracing collector: `next`
  threads every live allocation into one global list so the sweep phase can visit
  every object without a separate registry, and `gc_mark` is the mark bit the
  collector sets during its traversal.

This header is modestly larger than a reference-counting-only design would need
(which requires only `ref_count` and `vtable`), but the additional two fields are what
make cycle collection possible without giving up reference counting's fast,
deterministic reclamation for the common, acyclic case. See §6.7 for the full
justification.

### 6.2 Object layout and single-inheritance dispatch

```
struct T {                 // for `type T(...) inherits Parent(...) { attrs; methods }`
    ObjHeader header;
    <Parent's own fields, in Parent's declared order>   // recursively
    <T's own attribute fields, in declaration order>
}
```

This is the standard single-inheritance layout used by, for example, the Itanium C++
ABI's single-inheritance case — HULK's lack of multiple inheritance means this is the
simple case of that ABI, with none of its multiple-inheritance thunk machinery.

**Vtable contents:** one function pointer per entry of the type's flattened method
table, in its iteration order (§4, point 1) — this order already matches the slot
order a single-inheritance vtable needs, with no additional computation. Build these
as LLVM global constants (`@T.vtable = constant [N x ptr] [...]`) once per concrete
type, after every method has been declared but before any has necessarily been fully
defined (a two-pass declare/define split, mirroring `hulk-semantic`'s own collection
pass).

**Alongside each vtable, emit a GC field map**: a compact static array listing the
byte offsets of every pointer-typed field in `T`'s struct (including inherited
fields). This lets the tracing collector's mark phase walk any live object generically
— look up its concrete type through the header's vtable, read the associated field
map, and follow each listed offset — without needing a hand-written or
per-type-generated trace function. The field map is built once, at the same point the
vtable itself is built, directly from the struct layout already known to codegen.

**Dispatch rule:**
- `expr.method(args)`, where `expr`'s static type is `Named(T)`: load the vtable
  pointer from `expr`'s header, index by the method's flattened slot number, call
  indirectly. This is necessary because the runtime object may be an instance of a
  subtype of `T` with an overriding implementation.
- `base.method(args)`: **not** a vtable load. A direct, non-virtual call to the
  immediate parent's own implementation, resolved using the "current owning type +
  current method name" tracking described in §4, point 6.
- **Devirtualization:** `Number`, `String`, `Boolean`, `Vector`, and `Range` are
  sealed — they can never be subclassed in HULK source — so calls to their methods
  skip the vtable entirely and call the unique implementation directly. Extend this
  to any user-declared type observed to have zero subtypes within the compilation
  unit. This is the same "no virtual subtypes observed → direct call"
  devirtualization LLVM itself performs when it can see through an indirection; doing
  it here, with full knowledge of the type hierarchy, is strictly cheaper and more
  reliable than relying on LLVM to rediscover it through an opaque vtable pointer.

### 6.3 Object construction (`new T(args)`)

1. `hulk_rt_alloc(sizeof(T))`; initialize `ref_count = 1`, `gc_mark = false`, link
   `next` into the global allocation list, and store `T`'s vtable pointer.
2. Evaluate and store every attribute initializer in declaration order, walking the
   hierarchy parent-first (§4, point 5 guarantees this needs no dependency analysis).
3. Every attribute field whose static type is heap-allocated (`String`, `Named(_)`,
   `Vector(_)`, the protocol fat pointer's data slot, or a boxed `Object`) must be
   retained (`hulk_rt_retain`) when stored into the new object — the constructor
   argument's own local binding will itself be released when its scope ends, and
   without a retain here the object would end up with a dangling field once that
   release runs.

### 6.4 Method bodies

Lowered like free functions, with two additions:

- Synthesize the implicit `self` parameter (§4, point 7): typed as a pointer to the
  *declaring* type's struct, not the most-derived type.
- Track `(current_owner_type, current_method_name)` while lowering a body, exactly
  mirroring the inference pass's own bookkeeping, because a `BaseRef` node carries no
  record of which parent/method it resolves to — that context must be reconstructed
  the same way at codegen time (§4, point 6).

### 6.5 Member access and assignment

- **Attribute read** (`Member` outside a `Call` position, resolving to an attribute):
  a `GEP` to the statically known field offset.
- **Bare method reference** (`Member` outside a `Call` position, resolving to a
  method): a legitimate `Type::Function` value per §5. Lower it as a two-word thunk —
  a function pointer paired with the bound `self` pointer — not a true closure, since
  HULK has no captured-variable lambdas. This is the only case in which a method
  reference is allowed to escape a `Call`'s callee position; anything else reaching
  codegen as a method-typed `Member` outside both shapes indicates an internal
  compiler defect (§9) rather than a language construct to lower.
- **Assignment to a `Member`/`Index` target**: `GEP` (or vector-index call) followed
  by a store. Every such store must release the old value (if the field's static type
  is heap-allocated) before overwriting, and retain the new value — the same
  retain/release discipline as construction, applied at mutation sites.

### 6.6 Protocols and `Iterable(T)`: fat pointers, not boxing

A protocol-typed static type (`Named(P)` where `P` is a protocol, or `Iterable(T)`)
needs a representation that lets one call site dispatch correctly across any concrete
type that structurally implements `P`, where each concrete type's method for `P` may
live at a different vtable slot than another implementing type's.

**Decision:** Rust-trait-object-style fat pointers, `{ ptr data, ptr itable }`, where
`itable` is a per-`(concrete type, protocol)` static array containing one function
pointer per *protocol* method, in the protocol's own declared order (the consumer
only ever knows `P`'s order, never the concrete type's). Because HULK's structural
conformance is fully static and already verified by the front end, the itable address
for any given conversion is known at compile time — there is no runtime itable lookup
at all, unlike Go's interfaces, which build their equivalent lazily at runtime.

**Generation is reachability-pruned**: emit an itable for `(T, P)` only if some
expression in the program actually converts a `T`-typed value into a `P`-typed (or
`Iterable(T)`-typed) slot. A single sweep over call sites, `let` bindings,
assignments, and parameter bindings is sufficient to find every such pair; a full
points-to analysis is not needed.

**`for` loops and vector comprehensions almost never need an itable** (§4, point 9):
the common case lowers directly to devirtualized `next()`/`current()` calls on the
iterable's concrete static type. Itables are reserved for the genuinely rare case of
a value whose *static* type is a bare protocol name or `Iterable(T)`.

**`match` patterns:**
- `Wildcard`/`Variable` patterns always match.
- `Literal` patterns compare by value (`hulk_rt_string_equals` for `String`).
- `Type(name, binding)` patterns reuse `hulk_rt_downcast_check`, binding the
  pattern-named variable only inside that case's body, mirroring the rule that match
  case bodies introduce their own lexical scope.
- No case matching at runtime calls `hulk_rt_match_fail()`.

### 6.7 Memory management: hybrid reference counting with a tracing cycle collector

**Decision.** HULK programs use a hybrid memory-management strategy combining:

1. **Reference counting** as the primary, fast-path mechanism, giving immediate,
   deterministic reclamation of acyclic structures — the large majority of real HULK
   programs (strings, vectors, trees, ordinary object graphs).
2. **A tracing, mark-sweep garbage collector** that runs periodically to reclaim
   reference cycles, which reference counting alone can never free (a self-referential
   `type Node(v, next: Node)`-style structure is fully expressible in HULK and is a
   real possibility, not a hypothetical).

This closes the one correctness gap a pure reference-counting design would otherwise
leave open — unbounded memory growth in any program that happens to build a cycle —
while keeping the common, acyclic case exactly as fast as pure reference counting:
objects that never participate in a cycle are still freed the instant their count
reaches zero, with no GC pause involved at all.

**Why not reference counting alone.** Reference counting is correct and cheap for
acyclic data but leaves cycles permanently leaked: an object whose only remaining
references are from other objects in the same cycle will never have its count reach
zero. Accepting that as a known v1 limitation is acceptable only if HULK programs are
guaranteed never to build self-referential structures — they are not, since the
language has no restriction preventing it. A correctness gap that scales with how
much a program uses a perfectly legal language feature is not an acceptable trade,
so this guide commits to closing it rather than documenting around it.

**Why not a tracing collector alone.** A pure tracing collector (no reference
counting) would reclaim cycles correctly but gives up reference counting's main
advantage: predictable, immediate reclamation with no collection pause on the common
path. For short-lived, single-`entry`-expression batch programs — the typical shape
of a HULK program — paying a tracing pass's overhead on every allocation, instead of
only when memory pressure actually requires it, is strictly worse than the hybrid
approach for no correctness benefit.

**Object header support** (§6.1): `ref_count`, `gc_mark`, and `next` together turn
every heap allocation into a node of one intrusive global allocation list, with the
bookkeeping the collector needs already present at zero extra indirection.

**Retain/release (the fast path, unchanged in character from pure reference
counting):**
- `hulk_rt_retain(ptr)`: increments `ref_count`.
- `hulk_rt_release(ptr)`: decrements `ref_count`; if it reaches zero, the object is
  unlinked from the allocation list and freed immediately — no need to wait for a GC
  cycle in the common, acyclic case.

**The mark-sweep collector (`hulk_rt_gc_collect`):**
- **Mark.** Traverse from the root set (the shadow stack described below; HULK has no
  global variables, so there is no separate global root set to scan), following every
  pointer-typed field of every reached object. The field map emitted alongside each
  type's vtable (§6.2) tells the marker which struct offsets are pointers, without
  requiring a hand-written trace function per type. `Vector`'s elements are pointers
  by uniform convention (§5.4), so a vector's contribution to the mark phase is simply
  "follow all `len` slots." A boxed `Object`'s tag byte indicates whether its payload
  is a pointer.
- **Sweep.** Walk the global allocation list (via each object's `next` link); free any
  object whose `gc_mark` bit was not set during the mark phase; clear the mark bit on
  every surviving object, ready for the next collection.
- **Root set / shadow stack.** Codegen emits a push onto a runtime-maintained shadow
  stack for every pointer-typed local variable that is introduced — function
  parameters, `self`, and `let`-bound pointers — at the point each is bound, and a pop
  at the corresponding scope exit, mirroring exactly the same scope-push/pop
  discipline already used for the ordinary local-variable scope stack (§4, point 4).
  An alloca whose address is passed to `hulk_rt_shadow_push` necessarily escapes the
  function, which correctly prevents LLVM's later `mem2reg` pass from promoting it
  away — only pointer-typed locals need this treatment; `Number`/`Boolean` locals are
  unaffected and continue to be promoted normally.
- **Trigger.** `hulk_rt_alloc` maintains a running total of allocated bytes;
  `hulk_rt_gc_collect()` is invoked automatically once that total exceeds a
  configurable threshold (a sensible default such as several megabytes, tunable via
  an environment variable read once at process start). For a typical short HULK batch
  program, the threshold is frequently never reached, so the collector contributes
  zero runtime overhead in the common case while still guaranteeing boundedness for
  any program that does build cycles or simply allocates heavily.

**Cost accounting.** The header grows by roughly two machine words compared to a
reference-counting-only header (`gc_mark` plus `next`), and codegen gains a small,
constant amount of additional work at every pointer-typed binding site (the
shadow-stack push/pop) and at module build time (the per-type field map, built once
alongside the vtable, not per allocation). Both costs are paid once, are small in
absolute terms, and buy back full correctness for cyclic data — which is the
appropriate trade for a compiler whose stated goal is production-grade correctness
without sacrificing the common case's performance.

**Forward compatibility.** The header layout (`ref_count`, `gc_mark`, `next`,
`vtable`) is stable; later refinements (an incremental or generational collector, a
write barrier to shrink pause times further) can be added without changing the ABI
`hulk-codegen` emits. Codegen's responsibility ends at correctly emitting
retain/release calls and shadow-stack registrations — none of that work needs to
change if `hulk-rt`'s collector implementation is later refined.

### 6.8 `is` / `as`

Both operators call `hulk_rt_downcast_check`, which walks the object's vtable-pointer
ancestor chain — a chain whose length is small and fully known at compile time from
the registry. `is` simply returns the resulting boolean. `as` additionally traps via
`hulk_rt_downcast_fail() -> !` on a failed check, after first emitting a diagnostic to
stderr — the same trap shape used for non-exhaustive `match`.

---

## 7. Phase-by-Phase Implementation Plan

Each phase states a goal, the concrete steps, the reasoning behind the ordering, and
the condition under which the phase is considered complete. Phases are strictly
ordered: do not begin a phase whose predecessor's "done when" condition has not been
met, since each phase's tests assume every earlier phase's machinery already works.

### Phase 0 — Environment setup and workspace groundwork

**Goal:** remove every environment-level blocker before any lowering code is written.
All tooling required here is free and open-source.

**Steps:**

1. Provision LLVM and a system linker on the build machine (see §9 for the exact
   version-selection procedure):
   ```bash
   llvm-config --version
   which cc clang gcc ld
   ```
   If LLVM is not yet installed, install LLVM 17 (the default target, §9) before
   proceeding — there is no way to validate Phase 1 without it. If a C compiler/linker
   driver is missing, install the platform's standard build toolchain package (for
   example, Ubuntu's `build-essential`), which provides `cc`/`gcc`/`ld` at no cost.
2. Add `hulk-rt` to the workspace member list, alongside the existing crates.
3. `crates/hulk-codegen/Cargo.toml`:
   ```toml
   [package]
   name = "hulk-codegen"
   version.workspace = true
   edition.workspace = true

   [dependencies]
   hulk-ast = { path = "../hulk-ast" }
   hulk-semantic = { path = "../hulk-semantic" }
   inkwell = { version = "0.5.0", features = ["llvm17-0"] }  # adjust per §9 if needed

   [dev-dependencies]
   hulk-lexer = { path = "../hulk-lexer" }
   hulk-parser = { path = "../hulk-parser" }
   ```
   The `dev-dependencies` split mirrors `hulk-semantic`'s own `Cargo.toml`:
   `hulk-codegen` never needs the lexer/parser at runtime, only for whole-pipeline
   integration tests.
4. `crates/hulk-rt/Cargo.toml`:
   ```toml
   [package]
   name = "hulk-rt"
   version.workspace = true
   edition.workspace = true

   [lib]
   crate-type = ["staticlib", "rlib"]
   ```
   `rlib` is kept alongside `staticlib` so `hulk-rt`'s own test suite can run with
   plain `cargo test -p hulk-rt`.
5. Stub both crates so the workspace builds end to end:
   - `hulk-rt/src/lib.rs`: one `#[no_mangle] pub extern "C" fn hulk_rt_noop() {}`, to
     prove the staticlib actually emits a symbol table.
   - `hulk-codegen/src/lib.rs`: keep the existing top-level doc comment; add nothing
     else yet.
6. Add a `hulk-rt` unit test asserting `size_of::<ObjHeader>()`/`align_of` match the
   constant codegen will assume for the header — this guards against the two crates'
   notion of the header layout silently diverging during later refactors.

**Why this order:** LLVM's major version hard-selects an `inkwell` feature at compile
time. Discovering a mismatch after lowering code already exists means redoing the
type signatures of every lowering function; resolving it here costs nothing.

**Done when:** `cargo build -p hulk-codegen -p hulk-rt` succeeds with the stub
contents above, and the installed `llvm-config --version` matches the pinned
`inkwell` feature.

---

### Phase 1 — LLVM scaffolding and an end-to-end smoke test

**Goal:** validate the entire IR-to-object-to-link-to-run chain before any real
expression lowering exists — every later phase reuses this chain unchanged, so
validating it early isolates a much smaller class of bugs than discovering a link
failure after 1500 lines of expression lowering already exist.

**Steps:**

1. `hulk-codegen/src/context.rs`:
   ```rust
   pub struct CodegenCtx<'ctx> {
       pub context: &'ctx inkwell::context::Context,
       pub module: inkwell::module::Module<'ctx>,
       pub builder: inkwell::builder::Builder<'ctx>,
       pub functions: std::collections::HashMap<String, inkwell::values::FunctionValue<'ctx>>,
       // Phase 2 adds a `types: HashMap<String, TypeLayout<'ctx>>` field here.
   }
   ```
2. `hulk-codegen/src/lib.rs`, a smoke-test body that only emits `main() -> i32 { ret
   0 }`:
   ```rust
   pub fn compile(
       verified: &hulk_semantic::VerifiedProgram,
       opts: &CodegenOptions,
   ) -> Result<(), CodegenError> {
       let context = inkwell::context::Context::create();
       let module = context.create_module("hulk_main");
       let builder = context.create_builder();
       let mut ctx = CodegenCtx { context: &context, module, builder, functions: Default::default() };

       let i32_t = ctx.context.i32_type();
       let main_fn = ctx.module.add_function("main", i32_t.fn_type(&[], false), None);
       let entry_bb = ctx.context.append_basic_block(main_fn, "entry");
       ctx.builder.position_at_end(entry_bb);
       ctx.builder.build_return(Some(&i32_t.const_int(0, false)))?;

       ctx.module.verify().map_err(CodegenError::LlvmVerification)?;
       if let Some(ll_path) = &opts.emit_llvm_path {
           ctx.module.print_to_file(ll_path).map_err(CodegenError::Io)?;
       }
       Ok(())
   }
   ```
3. Add a temporary `--emit-llvm <path>` flag to `hulk-cli` (it already depends on
   `clap`) to inspect the generated `.ll` text during development. Phase 9 formalizes
   the permanent CLI wiring; this flag only needs to exist long enough to validate
   this phase and the ones that follow.

**Done when:** `Module::verify()` passes, the printed `.ll` is syntactically valid,
and `clang out.ll -o a.out && ./a.out; echo $?` prints `0`.

---

### Phase 2 — `Type` to LLVM type lowering table

**Goal:** implement the representation table from §5.2 once, so every later phase
calls one function instead of re-deriving the mapping ad hoc.

**Steps:**

1. `hulk-codegen/src/types.rs`:
   ```rust
   pub fn llvm_type<'ctx>(ctx: &CodegenCtx<'ctx>, ty: &hulk_semantic::Type) -> BasicTypeEnum<'ctx> {
       use hulk_semantic::Type;
       match ty {
           Type::Number => ctx.context.f64_type().into(),
           Type::Boolean => ctx.context.bool_type().into(),
           Type::String | Type::Vector(_) | Type::Object => ctx.context.ptr_type(Default::default()).into(),
           Type::Named(name) if ctx.registry_is_protocol(name) => fat_pointer_type(ctx).into(),
           Type::Named(_) => ctx.context.ptr_type(Default::default()).into(),
           Type::Iterable(_) => fat_pointer_type(ctx).into(),
           Type::Unknown | Type::Error => unreachable!(
               "Type::Unknown/Error cannot reach codegen on the success path — \
                hulk-semantic's final checking pass guarantees this"
           ),
           Type::Function { .. } => todo!("Phase 5 — method-reference thunk lowering"),
       }
   }
   ```
2. Declare the `ObjHeader`, `HulkString`, and `HulkVector` struct types once, in a
   layout that byte-for-byte matches the `repr(C)` structs written in `hulk-rt` (§6.1,
   §5.4). The Phase 0 size/alignment test guards this match over time.
3. Implement object-struct layout construction for every `TypeDecl`, walking the
   registry's types in **parent-before-child** order. This ordering already exists as
   a reusable utility inside `hulk-semantic` (the same Kahn's-algorithm topological
   sort the hierarchy-flattening pass itself uses); re-export it as part of
   `hulk-semantic`'s public surface and call it directly from codegen rather than
   re-implementing the same sort independently (see §9 for the rationale and the
   fallback if exporting it turns out to be impractical).

**Done when:** for a hand-written two-level inheritance fixture, the generated
struct's LLVM `size_of` matches a hand-computed expectation, asserted in a unit test.

---

### Phase 3 — Core expression lowering (no objects, no functions yet)

**Goal:** lower every expression form that does not depend on Phase 4/5's machinery.

**Steps, in order, each independently testable on a hand-built `TypedExpr`:**

1. **`Literal`** — `Number`/`Boolean` are trivial constants. `String` needs a runtime
   allocation: emit a global byte-array constant for the bytes, then either populate a
   `HulkString` header at module-init time via `hulk_rt_alloc`, or, if the pinned LLVM
   version accepts constant structs with pointer fields cleanly, build the constant
   directly — verify both approaches against the version chosen in Phase 0 before
   committing to one.
2. **`Variable`/`Let`/`Block`** — `alloca` plus `load`/`store` for every local (the
   standard front-end pattern; Phase 8's `mem2reg` pass cleans this up later).
   Maintain a scope stack that mirrors the front end's own scope discipline exactly
   (§4, point 4): push/pop around `let` bodies, function/method bodies, `for` bodies,
   `match` case bodies; never push a scope for a plain `{ ... }` block.
3. **`Unary`/`Binary`** on `Number`/`Boolean` — direct `build_float_add`/
   `build_float_compare`/etc. `Power` (`^`) has no single LLVM instruction; lower it
   to the `llvm.pow.f64` intrinsic. `Concat`/`ConcatSpace` need the auto-stringify and
   space-insertion behavior from §5: route any non-`String` operand through
   `hulk_rt_number_to_string`/`hulk_rt_bool_to_string` before concatenating.
4. **`If`/`While`** — standard basic-block-with-`phi` lowering. The `phi`'s type is
   the `If`'s own already-resolved type; if that type is `Object`, every branch value
   must be boxed before reaching the merge block, even branches whose own type is
   concrete. Test this explicitly with an `if (b) 1 else "s"`-shaped fixture, since it
   is the first real exercise of the §5.2 boxing rule.
5. **`Assign`**, `Variable` targets only — `Member`/`Index` targets are deferred to
   Phases 5/6 once struct and vector layout exist.

**Why `alloca` everywhere instead of hand-rolled SSA:** this is the conventional LLVM
front-end pattern precisely because `mem2reg` is both correct and free; hand-rolling
SSA bookkeeping in `hulk-codegen` would duplicate a solved problem for no benefit.

**Done when:** a fixture set covering arithmetic, a `let` chain, an
`if`/`elif`/`else` with mixed-type branches forcing `Object`, and a `while` loop all
produce verified IR and, once linked through Phase 1's pipeline, print the expected
values.

---

### Phase 4 — Free functions and calls

**Goal:** every `DeclarationKind::Function` becomes a real, callable LLVM function,
regardless of declaration order.

**Steps:**

1. **Two-pass declare/define**, mirroring the front end's own collection-then-bodies
   split: declare every function's LLVM signature first (via Phase 2's type table),
   then lower every body.
2. Lower bodies, including direct self-recursion, with no special-casing required:
   by the time a `VerifiedProgram` exists, the front end's placeholder-and-patch
   strategy for recursive return types has already fully resolved every
   `Type::Unknown` in the tree, so codegen never sees a partially-typed recursive
   function.
3. Builtins (`print`, `sqrt`, `sin`, `cos`, `exp`, `log`, `rand`, `range`, `PI`, `E`)
   are never lowered from HULK source — none exists. Resolve a `Call` whose callee
   name matches one of these directly to a `hulk-rt` call or an LLVM intrinsic
   (Phase 7 has the full wiring).

**Why this order:** functions have no vtable and no inheritance concerns, making this
the simplest place to validate call resolution and recursion before Phase 5 adds
genuinely complex dispatch machinery on top.

**Done when:** a fixture with several free functions, including one self-recursive
function (factorial) and one call to each builtin, links and runs correctly.

---

### Phase 5 — Types: layout, construction, attributes, methods, inheritance

This is the largest phase; §6.1–6.5 already specify its content in full detail. The
steps below are the implementation order.

**Steps:**

1. Implement object layout and the per-type vtable/GC-field-map pair (§6.2).
2. Implement `ctor.rs` — `new T(args)` construction (§6.3), including the retain
   discipline for heap-allocated attribute fields.
3. Build per-type vtable globals only after every method has been declared (not yet
   defined) — the same two-pass approach as Phase 4.
4. Lower method bodies (§6.4): synthesize the implicit `self` parameter, and track
   `(current_owner_type, current_method_name)` for `base` resolution.
5. Implement the dispatch rule, including devirtualization for sealed types and for
   any type observed to have zero subtypes in the compilation unit (§6.2).
6. Implement `Member` lowering for both attribute reads and bare method references
   (§6.5), and extend `Assign` to cover `Member`/`Index` targets with the
   retain/release discipline from §6.5 and §6.7.
7. Implement `is`/`as` via `hulk_rt_downcast_check` (§6.8).

**Done when:** a three-level inheritance fixture (a base type with one virtual
method, a middle override, and a leaf adding a new method) constructs instances,
calls the overridden method through a base-typed reference and observes the
most-derived behavior (proving vtable dispatch is real, not accidentally static), a
`base.method()` call from inside an override reaches the immediate parent
specifically (proving the non-virtual path is correctly distinguished from the
virtual one), and a fixture that deliberately builds a reference cycle between two
instances — then drops every external reference to both — has its memory reclaimed
once `hulk_rt_gc_collect` runs, confirmed by an allocation counter in the `hulk-rt`
test harness returning to its pre-cycle value.

---

### Phase 6 — Protocols, `Iterable`, `for`, vector comprehension, `match`

**Steps:**

1. Implement itables (`layout/itable.rs`) as specified in §6.6, generated only for
   reachable `(type, protocol)` pairs.
2. Implement `For`/`VectorExpr::Comprehension` lowering: devirtualized
   `next()`/`current()` calls in the common concrete-type case, falling back to the
   itable fat pointer only when the iterable's *static* type is genuinely
   `Iterable(T)` or a bare protocol name (§4, point 9; §6.6).
3. Implement `pattern.rs` — `Match`/`Pattern` lowering exactly as specified in §6.6,
   including the `hulk_rt_match_fail()` trap for the no-match case.

**Done when:** a fixture with a user-declared protocol (one parameter typed by
protocol name, called against two different concrete types at different call sites),
a `for` over both a `Vector` literal and a `range(...)` call, and a `match` mixing
literal/type/wildcard cases all produce correct output — and a deliberately
non-exhaustive `match` fixture aborts with a diagnostic instead of producing garbage.

---

### Phase 7 — Vectors and remaining builtins end-to-end

**Steps:**

1. Implement `HulkVector` exactly as specified in §5.4 — `get`/`set`/`next`/`current`
   only, no `cap` field, no growth operations.
2. Implement vector-literal lowering: `hulk_rt_vector_new(items.len())`, store each
   (boxed if needed) item, and retain each stored pointer, since the vector now holds
   an independent reference to it.
3. Wire every remaining builtin (`print`, `sqrt`, `sin`, `cos`, `exp`, `log`, `rand`,
   `range`, `PI`, `E`). Prefer LLVM intrinsics over `hulk-rt` calls wherever one
   exists, so the optimizer can still constant-fold them; confirm what the pinned
   LLVM version actually lowers each intrinsic to (some became libm calls rather than
   hardware intrinsics in certain LLVM releases) before relying on constant-folding
   behavior. `print`'s signature is `(x: Object) -> Object`, so every call to `print`
   boxes its argument first regardless of the argument's own concrete type — this
   makes `print` a natural boxing-correctness smoke test, exercised on every fixture
   that produces observable output.

**Done when:** the full fixture corpus from Phases 1–6, plus a vector-heavy fixture,
produce byte-identical `print` output against a hand-computed expected-output file.

---

### Phase 8 — Optimization pipeline

**Steps:**

1. `optimize.rs`: wire `inkwell`'s pass-manager API for the pinned LLVM version
   (older releases expose the legacy `PassManager`; newer releases expose
   `PassBuilder`/`run_passes` — confirm which the Phase 0 version provides). At
   minimum: `mem2reg` (promotes Phase 3's deliberate `alloca`-everywhere lowering into
   real SSA — the single highest-value pass for this front end's style),
   `instcombine`, `simplifycfg`, and `inline` (this is where `base.method()` direct
   calls and Phase 5's devirtualized calls actually get inlined). Prefer a single
   `-O2`-equivalent pipeline call over hand-listing dozens of individual passes if the
   pinned version's API offers one.
2. Call `Module::verify()` both before and after optimization: before, to catch a
   codegen bug as a clear verifier error rather than a confusing optimizer crash;
   after, to catch the rarer case of a pass exposing a latent bug pre-optimization
   verification alone would not have caught.
3. Make the optimization level a `CodegenOptions` field, defaulting to a real
   optimization level rather than `-O0` — producing optimal code is an explicit
   project goal, not an afterthought.

**Done when:** comparing `.ll` output before and after the pipeline on the
factorial/vtable fixtures shows real improvement (inlined `base` calls, promoted
allocas), and every functional fixture from Phases 1–7 still passes after
optimization.

---

### Phase 9 — Object emission, linking, executable, CLI integration

This phase produces the concrete shape required by §2: `make build` → `./hulk`;
`./hulk file.hulk` → `./output`.

**Steps:**

1. `emit.rs`: `Target::initialize_native`, build a `TargetMachine` for the host
   triple (Linux x86_64 only, §5), `TargetMachine::write_to_file` with
   `FileType::Object` to produce `.o`.
2. Invoke the system linker driver as a subprocess — the same approach `rustc` itself
   uses, rather than reimplementing a linker:
   ```bash
   cc out.o -L<hulk-rt build dir> -lhulk_rt -lm -o ./output
   ```
   (`-lm` only if any `hulk-rt` math wrapper pulls in libm symbols not already
   replaced by pure LLVM intrinsics in Phase 7.)
3. Default the output path to `./output` in the current directory, with no flag
   required — keep a `-o`/positional override available for development, but the
   zero-argument default must be exactly `./output`, per §2.
4. `hulk-cli`: on a successful `analyze`, call `hulk_codegen::compile(&verified,
   &opts)` in place of (or, behind a `--print-ast`/`--emit-llvm` debug flag continuing
   what Phase 1 added temporarily, in addition to) printing the typed AST.
5. Implement the entry-point thunk from §5: initialize `hulk-rt` (including the
   allocation-byte counter used to trigger garbage collection), evaluate the lowered
   `entry` expression for side effects, discard its value, return `0`.
6. `./hulk` itself, per the build requirement in §2. To remove any ambiguity about the
   compiled binary's name, set it explicitly in `hulk-cli/Cargo.toml` rather than
   relying on Cargo's package-name default:
   ```toml
   [[bin]]
   name = "hulk-cli"
   path = "src/main.rs"
   ```
   ```makefile
   build:
       cargo build --release
       cp target/release/hulk-cli ./hulk
   ```

**Done when:** `./hulk program.hulk && ./output` works end-to-end for the full
fixture corpus, with no manual `clang`/`llc` step required by the user, and `make
build` from a clean checkout produces a working `./hulk`.

---

### Phase 10 — Testing strategy

**Steps:**

1. **Unit tests per `lower/*.rs` function**, in the same style `hulk-semantic` uses
   for its own passes: hand-build a small `TypedExpr`/`TypedProgram` (reusing
   `hulk-ast`'s existing `Expr::number`/`Expr::binary`/etc. helper constructors,
   instantiated with `Type` instead of `()`), lower it, and assert either on the
   textual `.ll` output (snapshot-style) or on `Module::verify()` succeeding plus a
   specific expected instruction pattern.
2. **Golden end-to-end fixtures**, built from scratch under
   `crates/hulk-codegen/tests/fixtures/` as `*.hulk` + `*.expected_stdout` pairs, one
   per language feature exercised in Phases 1–7. Run the full `lex → parse → analyze
   → compile → link → execute` pipeline as a subprocess test (`std::process::Command`)
   and diff captured stdout against the expected file — this is the tier that proves
   the backend actually works end-to-end, not merely that it produces well-formed IR.
3. **JIT-based fast unit tests**, as a recommended addition alongside the golden
   corpus: `inkwell::execution_engine::ExecutionEngine` can JIT-execute a `Module`
   in-process without the object-emission/linking round trip, making it well suited
   to the per-function unit tests in step 1 — reserve the slower, full link-and-run
   tier for the golden corpus in step 2.
4. Reuse the front end's own integration-test idiom directly: its tests already
   lex, parse, and `analyze` real source strings inline — the codegen golden tests
   should start from that exact same `analyze(&program)` call, continuing one step
   further into `compile`.

**Done when:** `cargo test -p hulk-codegen` covers every `ExprKind`/`DeclarationKind`
variant at least once, and the golden corpus's subprocess-based tests pass, exercised
through the same `make build` / `./hulk` path used for delivery.

---

### Phase 11 — Polish and future work (non-blocking)

These items improve the backend but are explicitly out of scope for the first working
compiler; pursue them only once Phase 10 passes in full:

1. Source-level debug information (`DICompileUnit`/`DISubprogram` via `inkwell`'s
   debug-info builder), mapping generated code back to `.hulk` source lines using the
   `SourceSpan`s already present on every AST node.
2. Cross-compilation support (additional `TargetMachine` triples beyond the v1
   Linux x86_64 target).
3. Revisit the hybrid memory model's GC threshold and trigger heuristics with real
   profiling data, particularly for any long-running or allocation-heavy HULK
   programs that emerge once the language sees real use beyond short batch scripts.
4. Revisit the "always-pointer vector elements" simplification (§5.4) with benchmark
   data if `Vector(Number)`-heavy programs turn out to be a meaningful workload.
5. Evaluate stack allocation (escape-analysis-friendly patterns) for short-lived
   `Object` boxes, per the open sub-decision noted in §5.2, once there is measured
   evidence that heap boxing is a meaningful cost in real programs.

---

## 8. Diagnostics: Error Format and Exit Codes

Requirement 4 of §2 is not satisfied by the front end's current diagnostic rendering
and must be fixed as part of this phase, even though the change lives outside
`hulk-codegen` itself:

1. **Error message format.** `hulk-semantic`'s current `SemanticError::Display`
   produces:
   ```rust
   write!(f, "{} at line {}, col {}: {}", prefix, self.span.line, self.span.col, self.kind)
   ```
   where `prefix` is `"semantic error"`/`"semantic warning"`. Replace it with:
   ```rust
   impl fmt::Display for SemanticError {
       fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
           write!(f, "({},{}) SEMANTIC: {}", self.span.line, self.span.col, self.kind)
       }
   }
   ```
   For warnings, use the parallel format `(line,col) WARNING: message` rather than
   `SEMANTIC`, since a warning is not the error class the exit-code priority in §2
   refers to and should be visually distinguishable from it. The alternative —
   suppressing warnings entirely in the graded CLI path — was considered and
   rejected: silently dropping diagnostics such as a non-exhaustive `match` warning
   removes information a user would reasonably want, for no benefit to delivery
   compliance, which only constrains the format of the error classes that affect the
   exit code.
2. **Apply the identical change to `hulk-lexer` and `hulk-parser`**, with `TYPE` set
   to `LEXICAL` and `SYNTACTIC` respectively.
3. **Exit-code classification in `hulk-cli`:** lex first, and exit `1` immediately on
   any lexical error without attempting to parse; otherwise parse, and exit `2` on
   any syntactic error without attempting semantic analysis; otherwise call
   `analyze`, and exit `3` on any hard semantic error; otherwise proceed to
   `hulk_codegen::compile`. This directly implements the "most fundamental error
   wins" priority from §2 by construction, since each stage only runs if the previous
   one fully succeeded.
4. **`REPORT.md`** is a writing deliverable, not a coding one, but should document the
   actual decisions made in this guide — the boxing strategy, the vtable/itable
   split, the hybrid memory-management model and why it was chosen over the
   alternatives in §6.7, the entry-point convention — rather than only restating the
   pipeline shape already visible in the source. Start a running notes file during
   Phases 5–9 rather than reconstructing the reasoning retroactively at the end.

---

## 9. Engineering Decisions Requiring Local Confirmation

A small number of decisions genuinely depend on the specific machine or repository
state the team is working against, rather than on the language or architecture. Each
is given a concrete default and an alternative below, so implementation is never
blocked on them — confirm the default during the indicated phase and switch to the
listed alternative only if the stated condition holds.

| # | Decision | Default (commit to this unless the condition below applies) | Alternative, and when to use it |
|---|---|---|---|
| 1 | LLVM major version / `inkwell` feature | LLVM 17, `inkwell` feature `llvm17-0`. Run `llvm-config --version` during Phase 0; if it reports `18.x`, switch to feature `llvm18-1` instead — both are free, well-supported `inkwell` targets. | If neither 17 nor 18 is installed and installing one is not possible in the build environment, pick the closest `inkwell`-supported major version to whatever `llvm-config` reports and update `hulk-codegen/README.md` to document the exact version pinned. |
| 2 | Internal compiler error exit code | Exit code `4`, classification `INTERNAL`, format `(line,col) INTERNAL: message` (or `(0,0) INTERNAL: message` when no span applies), for the case where `hulk-codegen::compile` fails on a program that already passed semantic analysis. | Reuse exit code `3` (treat as a semantic failure) if the grading or tooling around this project specifically expects only three exit codes to ever occur. Code `4` is preferred because it avoids conflating a compiler defect with a user-facing language error, which is materially more useful for debugging during development. |
| 3 | `topological_order` visibility | Make the existing parent-before-child sort in `hulk-semantic` part of its public API and call it directly from `hulk-codegen`, rather than re-implementing the same Kahn's-algorithm sort a second time. | Re-implement the same sort in `hulk-codegen` against `TypeRegistry`'s already-public `types`/`TypeInfo.parent` fields, if changing `hulk-semantic`'s public surface is undesirable for project-governance reasons. The exported version is preferred: it removes any risk of the two crates' notion of hierarchy order silently diverging after a future change to either pass. |
| 4 | Linker/build-toolchain availability | Install the platform's standard free build toolchain package (providing `cc`/`gcc`/`ld`) as part of Phase 0's environment setup, and verify with `which cc`. | None needed — this is a standard, free, zero-cost installation step on any Linux build host and should simply be done rather than treated as a contingency. |

---

## 10. Module/File Checklist

```
crates/hulk-codegen/
├── Cargo.toml                  Phase 0
├── README.md                   Phase 0 (document the pinned LLVM version, §9)
└── src/
    ├── lib.rs                  Phase 1  — compile() entry point
    ├── error.rs                Phase 1  — CodegenError
    ├── context.rs              Phase 1  — CodegenCtx
    ├── types.rs                Phase 2  — Type -> inkwell type table, struct layouts
    ├── runtime_decls.rs        Phase 4  — extern "C" decls for every hulk-rt symbol
    ├── layout/
    │   ├── mod.rs              Phase 5
    │   ├── vtable.rs           Phase 5  — per-type vtable + GC field-map construction
    │   └── itable.rs           Phase 6  — per-(type,protocol) interface tables
    ├── lower/
    │   ├── mod.rs              Phase 3  — declarations pass, then entry
    │   ├── decl.rs             Phase 4  — FunctionDecl/TypeDecl -> LLVM defs
    │   ├── expr.rs             Phase 3–7 — ExprKind -> BasicValueEnum
    │   ├── pattern.rs          Phase 6  — Match/Pattern lowering
    │   └── ctor.rs             Phase 5  — `new T(args)` construction
    ├── optimize.rs             Phase 8
    └── emit.rs                 Phase 9

crates/hulk-rt/
├── Cargo.toml                  Phase 0
└── src/
    └── lib.rs                  Phases 1, 4, 5, 6, 7 — alloc, retain/release, the
                                  mark-sweep collector and shadow stack, print,
                                  string/vector ops, range, downcast check, match_fail

crates/hulk-codegen/tests/fixtures/   Phases 1–10 — *.hulk + *.expected_stdout
```
