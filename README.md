# HULK Compiler

![CI](https://github.com/D4R102004/hulk-compiler/actions/workflows/ci.yml/badge.svg)

> A complete, production-quality compiler for the [HULK language](https://matcom.github.io/hulk/appendix-hulk-syntax.html),
> implemented entirely in Rust.  
> Universidad de La Habana — Compilers 2026.

---

## Table of Contents

- [What is HULK?](#what-is-hulk)
- [Language Extensions](#language-extensions)
- [Architecture Overview](#architecture-overview)
- [Crate Reference](#crate-reference)
- [Requirements](#requirements)
- [Setup](#setup)
- [Usage](#usage)
- [Component Deep Dives](#component-deep-dives)
  - [Lexer](#lexer-hulk-lexer)
  - [Parser](#parser-hulk-parser)
  - [Semantic Analyzer](#semantic-analyzer-hulk-semantic)
  - [Code Generator](#code-generator-hulk-codegen)
  - [Runtime Library](#runtime-library-hulk-rt)
  - [Optimization Pipeline](#optimization-pipeline)
- [Memory Management](#memory-management)
- [Testing](#testing)
- [Design Decisions](#design-decisions)
- [Known Limitations](#known-limitations)
- [Team](#team)

---

## What is HULK?

HULK (*Havana University Language for Kompilers*) is a statically-typed, object-oriented,
expression-based language designed at UH for teaching compilers. Every construct is an
expression; there are no statements. The language supports:

- **Primitive types**: `Number` (64-bit float), `String`, `Boolean`
- **User-defined types** with single inheritance (`inherits`)
- **Protocols** — structural typing with variance-correct checking
- **`let`/`in` bindings**, destructive assignment (`:=`), block expressions
- **Control flow**: `if`/`elif`/`else`, `while`, `for`/`in`
- **Object construction** (`new`), member access, virtual dispatch via `self` and `base`
- **Type tests** (`is`) and safe downcasts (`as`), with runtime checks
- **Built-in math**: `sin`, `cos`, `sqrt`, `log`, `exp`, `rand`, `PI`, `E`

This compiler covers all HULK features through §16.8 of the official specification and adds
three language extensions described below.

---

## Language Extensions

### 1. `match` Expressions

Structured pattern matching that is always an expression and performs type narrowing:

```hulk
function classify(x: Object): String {
    match x {
        case n: Number  => "number";
        case s: String  => "string";
        case b: Boolean => "bool";
        case _          => "other";
    }
}
```

**Patterns supported:**
| Pattern | Syntax | Semantics |
|---|---|---|
| Wildcard | `_` | Always matches; no binding |
| Literal | `case "hi"` | Equality check |
| Variable | `case v` | Binds scrutinee to `v` |
| Type + binding | `case n: Number` | Runtime type test; narrows `n` to `Number` |

The type of a `match` expression is the lowest common ancestor (LCA) of all arm types.
A non-exhaustive match (no catch-all) emits a `NonExhaustiveMatch` warning; at runtime,
an unmatched scrutinee calls `hulk_rt_match_fail()`.

### 2. First-Class Function Types

The type `(T₁, ..., Tₙ) -> R` allows functions and methods to be stored in variables,
passed as arguments, and returned:

```hulk
function apply(f: (Number) -> Number, x: Number): Number { f(x); }

apply(function (x: Number): Number -> x * x, 5);  // 25
```

Method references capture the receiver as a two-word thunk `{fn_ptr, self_ptr}`:

```hulk
let c = new Counter(0) in {
    let inc: () -> Number = c.inc in { inc(); inc(); };
};
```

Subtyping follows the standard rule: **contravariant in parameters, covariant in return**.

### 3. Vectors and Comprehensions

`Vector<T>` (written `T[]` in type position) with literal syntax and Haskell-style
comprehensions:

```hulk
let squares = [x * x | x in range(1, 6)] in
    for (s in squares) print(s);   // 1 4 9 16 25
```

`Vector<T>` is covariant and implements `Iterable<T>` automatically.  
The ambiguity between the `|` boolean operator and the comprehension separator
is resolved by a special `ExprNoTopOR` production in the grammar.

---

## Architecture Overview

```
Source Code (.hulk)
       │
       ▼
┌──────────────┐
│  hulk-lexer  │  DFA tokenizer → Vec<Token> with SourceSpan
└──────┬───────┘
       │
       ▼
┌──────────────┐
│ hulk-parser  │  Hand-written LL(1) recursive-descent → Program<()>
└──────┬───────┘
       │
       ▼
┌─────────────────┐
│ hulk-semantic   │  Five-pass analyzer → VerifiedProgram (Program<Type>)
└──────┬──────────┘
       │
       ▼
┌──────────────────────────────────────┐
│           hulk-codegen               │
│  Typed AST → LLVM IR → .o (ELF)     │
│  + vtables, itables, shadow-stack    │
│  + LLVM optimization pipeline        │
└──────┬───────────────────────────────┘
       │ links against
       ▼
┌──────────────────────────────────────┐
│             hulk-rt                  │
│  Hybrid GC · strings · vectors       │
│  builtins · match/downcast support   │
└──────────────────────────────────────┘
       │
       ▼
  Linux x86_64 Executable
```

Each stage is its own Rust crate and depends only on earlier ones.
The full pipeline is wired together by `hulk-cli`.

---

## Crate Reference

| Crate | Status | Description |
|---|---|---|
| `hulk-ast` | ✅ Complete | Generic, annotation-parametrized AST shared by all stages |
| `hulk-lexer` | ✅ Complete | Hand-written DFA tokenizer with multi-error recovery |
| `hulk-parser` | ✅ Complete | LL(1) recursive-descent parser with precise error messages |
| `hulk-semantic` | ✅ Complete | Five-pass type system: collect → hierarchy → infer → check → resolve |
| `hulk-codegen` | ✅ Complete | LLVM 17 backend: object model, vtables, itables, GC instrumentation, optimization |
| `hulk-rt` | ✅ Complete | Runtime: hybrid GC (ref-counting + mark-sweep), strings, vectors, builtins |
| `hulk-cli` | ✅ Complete | CLI driver wiring the full pipeline with structured error output |

---

## Requirements

- **Rust** 1.78 or later (`rustup` recommended)
- **LLVM 17** development libraries and tools — required only by `hulk-codegen` and `hulk-rt`
  - Ubuntu/Debian: `apt install llvm-17-dev clang-17`
  - macOS (Homebrew): `brew install llvm@17`
  - Windows: install the LLVM 17.0.x release binary; point `LLVM_SYS_170_PREFIX` at it

The codegen crate targets **Linux x86_64 ELF** unconditionally (cross-compilation is
supported from Windows via `clang --target=x86_64-unknown-linux-gnu`).

---

## Setup

```bash
git clone https://github.com/D4R102004/hulk-compiler.git
cd hulk-compiler
cargo build --all
```

---

## Usage

### Full compilation pipeline (CLI)

```bash
cargo run -p hulk-cli -- path/to/program.hulk
```

On **success**: prints any warnings, then exits `0`.  
On **failure**: prints every error found (never just the first), each with source location, and exits non-zero.

### Compile to executable

```bash
# Build the runtime static library
cargo build -p hulk-rt --release

# Compile a HULK source file to a native Linux x86_64 executable
cargo run -p hulk-cli -- --compile path/to/program.hulk -o program

# Run (on Linux or WSL)
./program
```

### Optimization level

```bash
cargo run -p hulk-cli -- --compile program.hulk -O2   # default<O2>
cargo run -p hulk-cli -- --compile program.hulk -O3   # aggressive
cargo run -p hulk-cli -- --compile program.hulk -O0   # no optimization
```

### Run all tests

```bash
cargo test --all
```

---

## Component Deep Dives

### Lexer (`hulk-lexer`)

A hand-written **Deterministic Finite Automaton (DFA)** — no table, no code generator.
Each state is a branch of native Rust code, which is faster to execute and easier to debug
than Flex-style table-driven lexers.

**Key design choices:**

- **Keywords as post-process**: everything that looks like an identifier is emitted as
  `Identifier`; a single post-processing pass converts reserved words to their tokens.
  This keeps the DFA simple and makes adding keywords trivial.
- **`base` as a contextual keyword**: resolved by the parser, not the lexer — the same
  approach Kotlin uses for soft keywords like `it` and `field`, rather than treating `base`
  as unconditionally reserved as C# does.
- **Multi-error recovery**: on encountering a bad character, the lexer emits an error token
  and advances, so a single compilation pass surfaces *all* lexical errors rather than
  stopping at the first.

**Error token kinds:**

| Kind | Trigger |
|---|---|
| `UnexpectedChar` | Character outside the HULK alphabet |
| `UnterminatedString` | String literal not closed before end-of-line/EOF |
| `InvalidEscape` | Unknown escape sequence inside a string (e.g. `\q`) |

Every error token carries a `SourceSpan` (line, column, length) used by the CLI to
print underlined diagnostics.

---

### Parser (`hulk-parser`)

A **hand-written LL(1) predictive recursive-descent parser**. Expression precedence is encoded
directly in the grammar via a hierarchy of non-terminals; left recursion is eliminated using
tail-production loops. The grammar is documented in `GRAMMAR_LL1.md` inside the crate.

**Supported constructs**: global functions, types with inheritance, protocols, `let`/`in`,
blocks, `if`/`elif`/`else`, `while`, `for`/`in`, function/method calls, member access,
object construction, destructive assignment (`:=`), vectors, indexing, `is`/`as`, and
`match` (the extension node).

**Error quality**: because the parser is hand-written, every error site has explicit
context available on the Rust call stack. Errors read like
_"expected `in` after the `for` variable"_, not the generic _"syntax error"_ produced by
LR table parsers when the stack state is opaque. All parse functions return
`Result<Expr, ParseError>` where `ParseError` carries the `SourceSpan` of the unexpected token
and a description of the syntactic context.

**Extensibility**: adding a new primary expression (e.g. `match`) requires only adding a
branch to `parse_primary` and implementing the corresponding `finish_*` method — no table
regeneration, no grammar file to recompile.

---

### Semantic Analyzer (`hulk-semantic`)

**Entry point:**
```rust
pub fn analyze(program: &hulk_ast::Program) -> Result<VerifiedProgram, Vec<SemanticError>>
```

`analyze` never stops at the first error — it collects all `SemanticError` instances
(each with severity `Error` or `Warning` and a `SourceSpan`) and returns the full set.

**`VerifiedProgram`** is the contract between the frontend and the backend:
```rust
pub struct VerifiedProgram {
    pub registry:      TypeRegistry,   // all resolved type/protocol/function signatures
    pub typed_program: TypedProgram,   // Program<Type> — every expr carries a resolved Type
    pub warnings:      Vec<SemanticError>,
}
```

Internally, `analyze` runs **five passes** over a shared `TypeRegistry`:

| # | Pass | Responsibility |
|---|---|---|
| 0 | **collect** | Registers all function, type, and protocol signatures so forward references work regardless of declaration order. |
| 1 | **hierarchy** | Resolves `inherits`/`extends` links; rejects cycles; checks override and protocol-variance rules; flattens attribute/method tables with DFS interval indexing for O(1) subtype checks. |
| 1.5 | **resolve constructor params** | Infers unannotated type-constructor parameters and propagates them through `inherits Parent(args)` clauses. |
| 2 | **infer** | Builds the fully typed tree; assigns a type to every expression using bidirectional inference (synthesis bottom-up + type-push top-down). Unresolvable expressions produce `Type::Unknown`. |
| 3 | **check** | Re-validates explicit annotations, sweeps for residual `Unknown` types, and enforces attribute privacy. `hulk-codegen` is guaranteed to never receive a `Unknown`-containing tree. |

**Type system coverage:**
- Builtin value types: `Number`, `String`, `Boolean`
- Nominal root: `Object`
- User-defined types (single inheritance) and protocols (structural, with contra/covariant checking)
- `Vector<T>` (covariant) and `Iterable<T>`
- `Type::Function { params, return_type }` for first-class function values
- LCA (Lowest Common Ancestor) resolution for multi-branch constructs (`if`, `match`, vector literals)
- Bidirectional type inference: synthesis + type-push propagation; `Type::Unknown` sentinel for the `check` pass

---

### Code Generator (`hulk-codegen`)

Lowers a `VerifiedProgram` to a **Linux x86_64 ELF object file** via LLVM 17 (`inkwell`).

#### Object Layout

Every HULK object starts with a common `ObjHeader`:

```
struct ObjHeader {
    type_tag:  i64,       // immutable type discriminant
    ref_count: i64,       // reference count (GC half 1)
    gc_mark:   i8,        // mark bit (GC half 2)
    next:     *ObjHeader, // GC allocation list
    vtable:   *VTable,    // virtual dispatch table
}
```

Fields are laid out as: header → inherited fields (parent-first) → own fields (declaration order).
This matches the Itanium C++ ABI for single inheritance without any of the multiple-inheritance
complications. Virtual dispatch loads the vtable pointer, indexes to the method slot, and calls
indirectly. For sealed types (`Number`, `String`, `Boolean`, `Vector`) and user types with no
observed subtypes in the compilation unit, the indirect call is replaced with a direct call
(**closed-world devirtualization**).

#### Protocols and Interface Tables (Itables)

Protocols (structural typing) compile to **interface tables**: for each `(concrete type, protocol)`
pair materialized anywhere in the program, the compiler builds a constant table with one function
pointer per protocol method. Protocol-typed values are **fat pointers** `{ data_ptr, itable_ptr }`.

Table resolution is entirely **compile-time** — the same model as Rust trait objects — because
HULK compiles as a closed unit with all types visible. This avoids the runtime table construction
cost of Go interfaces or the class-loading-time cost of Java interfaces.

#### Supported Lowerings

| Construct | Notes |
|---|---|
| Literals, variables, `let`, blocks | Full scope management with retain/release |
| Unary/binary operators | Arithmetic, comparison, logical, string concatenation |
| `if`/`elif`/`else`, `while` | Standard SSA-form control flow |
| `for`/`in` | Devirtualized iteration over `Iterable<T>` |
| Function calls (free, method, `base`) | Static, virtual, and base-delegation |
| `new` | Heap allocation + field initialization + vtable setup |
| Member access and assignment | GEP-based with retain/release on pointer-typed fields |
| `is` / `as` | Runtime vtable-chain walk (`hulk_rt_downcast_check`) |
| Vector literals and comprehensions | With GC-managed heap backing array |
| `match` | Sequential pattern evaluation; non-exhaustive → `hulk_rt_match_fail` |
| Protocol dispatch | Fat-pointer itable indexing |
| Math builtins | Direct LLVM intrinsic calls |

---

### Runtime Library (`hulk-rt`)

A small C-ABI static library (`libhulk_rt.a`) linked into every HULK executable.

**Allocation and lifecycle:**
| Function | Behaviour |
|---|---|
| `hulk_rt_alloc(size)` | Allocates `size` bytes, links into the global allocation list, updates `ALLOC_BYTES`, triggers `gc_collect` if over threshold |
| `hulk_rt_retain(ptr)` | Increments `ref_count`; no-op for `null` and immortal string literals |
| `hulk_rt_release(ptr)` | Decrements `ref_count`; at zero, calls the type-specific destructor and frees |
| `hulk_rt_shadow_push(slot)` | Registers the *address* of a pointer-typed local in the shadow stack |
| `hulk_rt_shadow_pop()` | Pops the most recent entry (LIFO, mirrors scope exit) |
| `hulk_rt_gc_collect()` | Full mark-sweep cycle: walk shadow stack → mark reachable objects via field maps → sweep allocation list |

**String operations:** `hulk_rt_str_concat`, `hulk_rt_str_eq`, `hulk_rt_to_string` (Number/Boolean → String).

**Vector operations:** `hulk_rt_vector_new`, `hulk_rt_vector_push`, `hulk_rt_vector_get`,
`hulk_rt_vector_size`, `hulk_rt_dynamic_vector_to_vector`.

**Match/downcast support:** `hulk_rt_downcast_check`, `hulk_rt_match_fail`.

**Math builtins:** `hulk_rt_sin`, `hulk_rt_cos`, `hulk_rt_sqrt`, `hulk_rt_log`,
`hulk_rt_exp`, `hulk_rt_rand`, `hulk_rt_pow`.

---

## Memory Management

HULK programs can construct self-referential structures, so a pure reference-counting scheme
would permanently leak any cycle. The design uses a **hybrid model** — the same approach as CPython:

### Half 1: Reference Counting (fast path)

Every allocation is tracked by an `i64 ref_count` in `ObjHeader`.
`hulk-codegen` emits `hulk_rt_retain` / `hulk_rt_release` calls systematically:
at object construction, on pointer-typed member assignment, and at every scope exit
(`pop_scope` releases all pointer-typed locals in reverse declaration order).

When `ref_count` reaches zero, the type-specific destructor (`gc_free_object`) frees
secondary allocations (string data buffer, vector backing array, etc.) before freeing
the header. Reference counting alone handles all acyclic programs with zero overhead
from the mark-sweep collector.

### Half 2: Cycle Collection via Mark-Sweep

A mark-sweep collector runs automatically when `ALLOC_BYTES` exceeds a configurable
threshold. It requires two pieces of infrastructure emitted by `hulk-codegen`:

**Shadow stack** — a compiler-maintained root set, avoiding direct access to the native C stack:

```llvm
%x = alloca ptr
store ptr %init_val, ptr %x
call void @hulk_rt_shadow_push(ptr %x)   ; register address of slot
; ... scope body ...
%x_val = load ptr, ptr %x
call void @hulk_rt_release(ptr %x_val)
store ptr null, ptr %x                   ; prevent double-free
call void @hulk_rt_shadow_pop()
```

**Field maps** — per-type arrays of pointer-field byte-offsets, embedded in vtable slot 0:

```llvm
@Node.field_map = constant [3 x i64] [i64 48, i64 56, i64 -1]
                          ;             ^val     ^next   ^sentinel

@Node.vtable = constant [N x ptr] [
  ptr @Node.field_map,   ; slot 0 — reserved for GC
  ptr @Node.toString,    ; slot 1
  ...
]
```

The mark phase walks the shadow stack, dereferences each slot to get an object pointer,
loads `vtable[0]` to get the field map, and recursively marks each pointed-to object.
The sweep phase walks the global allocation list and frees objects whose `gc_mark` bit
was not set.

### GC Safety Invariants

Two compile-time invariants guarantee freedom from double-free:

1. **Post-release null-store**: `hulk-codegen` always emits `store ptr null` into the
   local slot immediately after `hulk_rt_release`. Subsequent accidental loads produce
   `null`, which all runtime functions treat as a no-op.

2. **Header poisoning on free**: `hulk_rt_release` writes `type_tag = TAG_FREED (0xFF)`
   and `ref_count = i64::MIN` before returning memory to the allocator. Any dangling
   pointer that reaches `retain`/`release` is detected immediately (debug: `panic!`;
   release: safe no-op).

---

## Optimization Pipeline

Enabled via `--compile -O2` (default) or `-O3` (aggressive). Disabled with `-O0`.

The compilation sequence inside `hulk-codegen::compile()` is strictly:

```
verify IR → run LLVM passes → verify IR again → emit object file
```

Double verification catches both codegen bugs (first verify) and passes that expose
latent type inconsistencies (second verify, e.g. after `instcombine` or `GVN`).

**Passes applied** (via `Module::run_passes` with the LLVM pass pipeline string):

| Pass | Effect |
|---|---|
| `mem2reg` | Promotes `alloca`s to SSA registers, eliminating redundant loads/stores |
| `instcombine` | Algebraic simplification of instruction sequences |
| `simplifycfg` | Removes unreachable blocks, merges trivial branches |
| `inline` | Inlines small callees into their call sites |

**GC compatibility is structurally guaranteed**: `mem2reg` only promotes an `alloca`
if its address never escapes (no uses other than `store`/`load`). Every pointer-typed
`alloca` is passed to `hulk_rt_shadow_push`, making it *address-taken* — LLVM
permanently excludes it from `mem2reg`. Numeric (`f64`) and boolean (`i1`) allocas
are never shadow-pushed and are freely promoted to SSA registers, reducing arithmetic
overhead with no impact on GC correctness.

`instcombine` and `simplifycfg` cannot eliminate calls to external functions
(all shadow-stack and GC functions are external to the LLVM module), so they cannot
remove GC instrumentation. Inlining preserves push/pop balance because the pairs are
emitted at HULK lexical scope boundaries, not at LLVM function boundaries.

---

## Testing

**237 tests** across the workspace (`cargo test --all`):

| Crate | Tests |
|---|---|
| `hulk-ast` | 4 |
| `hulk-lexer` | 16 |
| `hulk-parser` | 9 |
| `hulk-semantic` | 86 |
| `hulk-codegen` | 79 |
| `hulk-rt` | 43 |
| **Total** | **237** |

### Two-level strategy for `hulk-codegen`

**Level 1 — IR unit tests** (`src/lower/mod.rs`, 43 tests):  
Construct a minimal hand-built `Expr<Type>` node, run it through `lower_expr`,
and assert `Module::verify()` passes — optionally inspecting the emitted IR text.
These tests are fully independent of the lexer/parser/semantic pipeline and do not
require `hulk-rt` to be linked.

**Level 2 — Golden tests** (`src/lib.rs`, 36 tests):  
Run the full pipeline from HULK source string through to a `.o` file; assert the
ELF magic bytes are present. A subset links the object against `libhulk_rt.a`,
executes the binary, and compares stdout (or exit code, for the non-exhaustive
`match` trap) against expected output. These are the only tests that certify
*observable runtime behaviour*.

### Coverage highlights

- All HULK constructs from literals through protocols and `match`
- GC: shadow-stack IR shape (pointer allocas remain in memory after `mem2reg`);
  field-map contents; `gc_collect` reclaims binary and ternary cycles without
  freeing live objects
- Optimization: IR before/after `O2` pipeline; numeric allocas promoted to SSA;
  pointer allocas not promoted
- Runtime robustness: `TAG_FREED` poisoning detection; absence of leaks in
  `hulk_rt_dynamic_vector_to_vector`; correct cleanup by `reset_gc_state`
  for types with secondary allocations

---

## Design Decisions

### Generic annotated AST

`Expr<A>` is parametrized by its annotation type (`()` before semantic analysis,
`Type` after). This is the *Trees That Grow* / *annotated AST* pattern used by GHC
and rustc: the Rust type system statically prevents mixing typed and untyped trees.

### Hand-written LL(1) parser over a generator

A generated parser (e.g. `pest`, `lalrpop`) would provide formal LL(1)/LR(1) correctness
guarantees. The hand-written approach was chosen for control over error messages and
because adding new constructs (`match`, function types) requires only adding branches
to `parse_primary` — no table regeneration.

### Closed-world compilation

Every HULK program is compiled as a single unit. This enables:
- All `(type, protocol)` pairs resolved at compile time → constant itable construction
- Sealed and leaf types devirtualized → direct calls instead of indirect dispatch
- Complete type hierarchy visible → precise GC field maps without runtime reflection

### Hybrid GC over pure tracing or pure RC

Pure RC leaks cycles. Pure tracing (stop-the-world mark-sweep globally) has higher
average overhead. The hybrid model pays RC cost only for the common acyclic case and
runs mark-sweep only when the allocation budget is exceeded, minimizing average pause time.

### LLVM as backend over custom IR

LLVM provides register allocation, instruction selection, scheduling, and a mature
optimization pipeline for free. The `inkwell` Rust bindings provide type-safe access
to the LLVM C API at the cost of pinning to a specific LLVM version (17).

---

## Known Limitations

- **Mutual recursion without return-type annotations**: two mutually recursive functions
  where *both* lack explicit return types produce a deterministic "cannot infer type" error.
  Resolving this would require Hindley-Milner-style global unification.

- **Stop-the-world GC pauses**: the mark-sweep collector examines all live objects on
  every cycle. A generational collector would limit collection to the young generation
  and reduce average pause time significantly.

- **Uniform vector element storage**: all vector elements are stored as heap pointers,
  even for `Number`. A future optimization would unbox numeric vectors to a flat `f64` array.

- **No interprocedural optimization**: the LLVM pipeline inlines and optimizes within
  functions but does not perform whole-program dead-code elimination or type-specialization
  of polymorphic functions beyond the local devirtualization the backend already performs.

- **Golden test coverage gap**: the current golden-test suite (link + execute + assert stdout)
  covers primarily the language extensions and the most recently added features. Older
  constructs (OOP with deep inheritance, `base` delegation, downcast chains) are validated
  at IR/ELF level only.

---

## Team

| Name | GitHub |
|------|--------|
| Darío Francisco Alfonso Urrutia | [@D4R102004](https://github.com/D4R102004) |
| Juan Carlos Carmenate Díaz | [@Juank404](https://github.com/JuanCMath) |
| Sebastian González Alfonso | [@sebagonz106](https://github.com/sebagonz106) |
