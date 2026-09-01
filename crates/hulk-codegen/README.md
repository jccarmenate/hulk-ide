# hulk-codegen

Code generation for HULK: lowers a fully type-checked `hulk_semantic::VerifiedProgram`
into a Linux x86_64 executable via LLVM 17, links it against the `hulk-rt` runtime
library, and produces a standalone native binary.

---

## Table of Contents

- [Overview](#overview)
- [Public API](#public-api)
- [Compilation Pipeline](#compilation-pipeline)
- [Module Architecture](#module-architecture)
- [Object Model](#object-model)
  - [Memory Layout](#memory-layout)
  - [Virtual Dispatch (Vtables)](#virtual-dispatch-vtables)
  - [Protocol Dispatch (Itables)](#protocol-dispatch-itables)
- [GC Instrumentation](#gc-instrumentation)
  - [Shadow Stack](#shadow-stack)
  - [Field Maps](#field-maps)
  - [Safety Invariants](#safety-invariants)
- [Optimization Pipeline](#optimization-pipeline)
  - [Optimization Levels](#optimization-levels)
  - [Compilation Sequence](#compilation-sequence)
  - [GC Compatibility](#gc-compatibility)
- [Supported Language Features](#supported-language-features)
- [Cross-Compilation](#cross-compilation)
- [Testing](#testing)
- [Setup](#setup)

---

## Overview

`hulk-codegen` is the final stage of the HULK compiler pipeline. It consumes the
`VerifiedProgram` produced by `hulk-semantic` — a fully type-annotated AST paired
with a resolved `TypeRegistry` — and performs the following work in sequence:

1. Builds LLVM struct types for every user-defined class.
2. Declares all functions, methods, and their vtable/itable entries.
3. Lowers every expression in every function body to LLVM IR, including full
   GC instrumentation (shadow-stack push/pop, retain/release, field maps).
4. Optionally runs the LLVM optimization pipeline.
5. Emits a relocatable ELF `.o` object file and links it against `libhulk_rt.a`.

The crate is designed for **cross-compilation**: it can build on any host platform
(Windows, macOS, Linux) and always produces **Linux x86_64 ELF** binaries.

---

## Public API

```rust
pub fn compile(
    verified: &VerifiedProgram,
    opts: &CodegenOptions,
) -> Result<(), CodegenError>
```

`CodegenOptions` controls:

| Field | Type | Default | Description |
|---|---|---|---|
| `output_path` | `PathBuf` | required | Base path for `.o` and final executable (no extension) |
| `opt_level` | `OptLevel` | `Default` | Optimization level applied to IR before emission |
| `emit_ir` | `bool` | `false` | Write a `.ll` text file alongside the `.o` for debugging |
| `link` | `bool` | `true` | Invoke linker after object emission to produce an executable |

```rust
use hulk_codegen::{compile, CodegenOptions, OptLevel};

let opts = CodegenOptions {
    output_path: "output".into(),
    opt_level: OptLevel::Default,
    emit_ir: false,
    link: true,
};
compile(&verified_program, &opts)?;
// produces: output.o  and  output (executable)
```

---

## Compilation Pipeline

Inside `compile()`, the following steps execute in order:

| Step | What happens |
|---|---|
| **1. Build object layouts** | Compute LLVM struct types for every user-defined class: `ObjHeader` fields, inherited attributes (parent-first, declaration order), then own attributes. Byte offsets are cached in `LowerCtx` for all subsequent GEP instructions. |
| **2. Declare functions** | Create LLVM function declarations for every global function and method, including the synthetic `__hulk_main` entry point. All signatures are stored in `LowerCtx` for call-site lookup before any bodies are lowered. |
| **3. Build vtables** | For each type, emit a global `[N x ptr]` constant with one function pointer per method in stable slot order (inherited slots first, then own). Slot 0 is always the GC field map (see [Field Maps](#field-maps)). |
| **4. Build itables** | For every `(concrete type, protocol)` pair materialized anywhere in the program, emit a constant itable with one function pointer per protocol method in declaration order. |
| **5. Define function bodies** | Lower each function and method body to LLVM IR via the recursive `lower_expr` dispatcher. |
| **6. Lower the entry expression** | Compile the program's top-level expression inside `__hulk_main`. |
| **7. Verify (pre-optimization)** | Run `Module::verify()`. Catches codegen bugs with precise LLVM diagnostics before they reach the optimizer, where invalid IR produces undefined behavior rather than useful error messages. |
| **8. Optimize** | Run `Module::run_passes(pipeline_string)` at the requested level. Skipped when `OptLevel::None`. |
| **9. Verify (post-optimization)** | Run `Module::verify()` again. Some passes (`instcombine`, `GVN`) can surface latent type inconsistencies that were invisible before constant propagation. |
| **10. Emit object file** | Call `TargetMachine::write_to_file` to produce a relocatable ELF `.o` for `x86_64-unknown-linux-gnu`. |
| **11. Link** | Invoke `clang` (Windows/macOS) or `cc` (Linux) to link the `.o` against `libhulk_rt.a` and produce the final executable. |

---

## Module Architecture

```
hulk-codegen/src/
|-- lib.rs              Public entry point: compile()
|-- context.rs          LowerCtx -- shared mutable state for the entire lowering pass
|-- layout.rs           Object layout, field offsets, struct type construction
|-- itables.rs          Interface table generation for protocol dispatch
|-- emit.rs             Target machine initialization and object file emission
|-- runtime_decls.rs    Declarations of hulk-rt functions as external LLVM symbols
|-- options.rs          CodegenOptions and OptLevel
|-- error.rs            CodegenError type
`-- lower/
    |-- mod.rs          lower_expr dispatcher + unit tests
    |-- literal.rs      Number, Boolean, String constants
    |-- binding.rs      Variables, let, destructive assignment (:=)
    |-- operators.rs    Unary and binary operators, string concatenation
    |-- control.rs      Blocks, if/elif/else, while
    |-- call.rs         Function calls, virtual method calls, base delegation
    |-- member.rs       Attribute reads, method references (fat-pointer thunks)
    |-- new.rs          Object construction, field initialization, vtable setup
    |-- type_ops.rs     is, as (downcast with runtime check)
    |-- for_loop.rs     for/in loops, vector comprehensions
    |-- pattern.rs      match expressions
    |-- decl.rs         Global function definitions
    `-- method.rs       Method definitions
```

### `LowerCtx`

The central struct threaded through all lowering functions. Owns:

- The LLVM `Context`, `Module`, and `Builder`.
- Maps from HULK name to `FunctionValue`, `GlobalValue`, field-offset tables, and vtable globals.
- The current function's **scope stack** (`Vec<HashMap<String, PointerValue>>`), which mirrors
  HULK lexical scoping. `push_scope` / `pop_scope` drive both variable resolution and GC
  lifetime instrumentation.
- Cached LLVM type references for `ObjHeader`, `i64`, `f64`, `i1`, `ptr`, and the fat-pointer
  struct `{ ptr, ptr }` used for protocol-typed values.

---

## Object Model

### Memory Layout

Every HULK heap object starts with a common **`ObjHeader`** immediately followed by its fields:

```
Offset  Field       LLVM type   Purpose
------  ----------  ---------   -------------------------------------------------
 0      type_tag    i64         Immutable type discriminant; TAG_FREED (0xFF) after free
 8      ref_count   i64         Reference count; poisoned to i64::MIN after free
16      gc_mark     i8          Mark bit for the cycle collector
17-23   (padding)   [7 x i8]   Alignment pad
24      next        ptr         Intrusive list node for GC allocation list
32      vtable      ptr         Pointer to this type's vtable
40+     <fields>    ...         Inherited fields (parent-first), then own fields
```

This layout matches the Itanium C++ ABI for single inheritance — a header common to all
objects in a hierarchy, followed by fields in declaration order — without any of the
complications that arise from multiple inheritance (no secondary vtable pointers, no
`this`-pointer adjustments). HULK allows only single inheritance, so the simplest
form of this model applies throughout.

Field offsets are computed once during step 1 and stored in `LowerCtx`. All field
accesses use constant-index GEP instructions, letting LLVM's verifier check offset
correctness at IR-generation time rather than at runtime.

### Virtual Dispatch (Vtables)

Each type has a global constant vtable — a `[N x ptr]` array of function pointers:

```
Slot  Content
----  ---------------------------------------------------------------
  0   GC field map pointer (see Field Maps below)
  1   First inherited method (from the root ancestor)
 ...  Remaining inherited methods in stable order
  k   First own method
 ...  Remaining own methods in declaration order
```

Slot assignments are computed once in `layout.rs` and reused at every call site.
A virtual method call compiles to:

```llvm
%vt  = load ptr, ptr getelementptr(ObjHeader, ptr %obj, i32 0, i32 4)
%fn  = load ptr, ptr getelementptr([N x ptr], ptr %vt, i32 0, i32 <slot>)
%ret = call <sig> %fn(ptr %obj, <args...>)
```

**Closed-world devirtualization**: when the static receiver type is a sealed built-in
(`Number`, `String`, `Boolean`, `Vector`) or a user type with no observed subtypes
in the compilation unit, the indirect call is replaced with a direct call to the
statically known function, eliminating the vtable load and branch-prediction overhead.
This is always safe because HULK compiles as a closed unit with no separate compilation
or dynamic type loading.

### Protocol Dispatch (Itables)

HULK protocols use structural typing. A protocol-typed value is a **fat pointer**
`{ data_ptr: ptr, itable_ptr: ptr }` — the same representation as a Rust `dyn Trait`.

For every `(concrete type T, protocol P)` pair that appears anywhere in the program,
`hulk-codegen` emits one constant itable:

```llvm
@T.itable.P = constant [M x ptr] [
  ptr @T.method_for_P_slot_0,
  ptr @T.method_for_P_slot_1,
  ...
]
```

All itable pointers are resolved **at compile time** — no runtime table search, no
hash map, no interface cache. This is possible because HULK compiles as a closed unit
and all `(type, protocol)` pairs are enumerable in a single pass over the typed AST.
The resulting dispatch cost is one fat-pointer load plus one function pointer load,
identical to a devirtualized call after the first indirection.

---

## GC Instrumentation

`hulk-codegen` emits two kinds of GC instrumentation as part of normal lowering, both
required for the mark-sweep cycle collector in `hulk-rt` to function correctly.

### Shadow Stack

The mark phase needs to enumerate all live heap pointers on the call stack without
accessing the native C stack (whose layout is platform-specific and not visible to
the runtime). `hulk-codegen` solves this with a **compiler-maintained shadow stack**:
for every variable whose HULK type requires heap allocation (user objects, strings,
vectors, and fat protocol pointers), it emits a `hulk_rt_shadow_push` on scope entry
and a matching `hulk_rt_shadow_pop` on scope exit.

The complete pattern emitted by `declare_var` in `binding.rs`:

```llvm
; --- scope entry ---
%x = alloca ptr
store ptr %init_val, ptr %x
call void @hulk_rt_retain(ptr %init_val)      ; take ownership
call void @hulk_rt_shadow_push(ptr %x)        ; register SLOT ADDRESS with GC

; ... scope body (assignments update the slot via store) ...

; --- scope exit (pop_scope) ---
%x_cur = load ptr, ptr %x
call void @hulk_rt_release(ptr %x_cur)        ; release ownership
store ptr null, ptr %x                        ; null-store safety
call void @hulk_rt_shadow_pop()               ; deregister slot
```

Passing the *address* of `%x` to `hulk_rt_shadow_push` makes `%x` unconditionally
*address-taken* in LLVM's alias analysis. `mem2reg` skips all address-taken allocas,
so these slots permanently remain in memory and are always visible to the GC traversal,
regardless of what the optimizer does. See [GC Compatibility](#gc-compatibility).

For **pointer-typed member assignments** (`:=` on an attribute), `hulk-codegen` emits
retain on the incoming value before releasing the old one, correctly handling the case
where both sides alias the same object.

### Field Maps

The mark phase must follow pointers *inside* heap objects to reach transitively live
data — without a hand-written trace function per type. `hulk-codegen` emits a
**field map** for each user-defined type: a constant `[K+1 x i64]` array of the
byte offsets of every pointer-typed field, terminated by the sentinel `-1`:

```llvm
; type Node(val: Object, next: Node) where val is at +40, next at +48
@Node.field_map = constant [3 x i64] [i64 40, i64 48, i64 -1]
```

The field map lives in **vtable slot 0**, reachable from any object through its existing
vtable pointer — no additional word in `ObjHeader` is needed:

```llvm
@Node.vtable = constant [4 x ptr] [
  ptr @Node.field_map,    ; slot 0: GC field map (reserved for runtime)
  ptr @Node.toString,     ; slot 1: first method
  ...
]
```

Types with no pointer-typed fields emit a sentinel-only map:
`@T.field_map = constant [1 x i64] [i64 -1]`, making the mark loop a no-op for
those types without any special-casing in the collector.

### Safety Invariants

Two compile-time patterns emitted by `hulk-codegen` prevent the most common
memory-safety failures in reference-counted systems:

| Pattern | Where emitted | What it prevents |
|---|---|---|
| **Post-release null-store** | `pop_scope`, immediately after every `hulk_rt_release` on a local slot | Double-free: the slot reads as `null` on any subsequent load; `retain`/`release` treat `null` as a no-op |
| **Header poisoning** *(enforced by `hulk-rt`)* | Inside `hulk_rt_release` before returning to the allocator | Use-after-free: `type_tag` is set to `TAG_FREED (0xFF)` and `ref_count` to `i64::MIN`; the next `retain`/`release` on a dangling pointer panics in debug mode and is a no-op in release |

---

## Optimization Pipeline

### Optimization Levels

`CodegenOptions::opt_level` selects the LLVM pass pipeline:

| `OptLevel` | LLVM pipeline string | Code-gen level | Use |
|---|---|---|---|
| `None` | *(no passes)* | `O0` | IR debugging, unit tests |
| `Less` | `"default<O1>"` | `O1` | Fast builds |
| `Default` | `"default<O2>"` | `O2` | Production |
| `Aggressive` | `"default<O3>"` | `O3` | Benchmarking |

The `TargetMachine` code-generation level is always set to match the IR level, avoiding
the pathological case of an optimized IR module fed into an `O0` instruction selector.

### Compilation Sequence

```
  lower typed AST to LLVM IR
          |
          v
  Module::verify()          <- catches codegen bugs before they reach the optimizer
          |
          v
  Module::run_passes()      <- skipped at OptLevel::None
          |
          v
  Module::verify()          <- catches issues exposed by optimization
          |
          v
  TargetMachine::write_to_file  -> .o
```

### GC Compatibility

LLVM optimization passes cannot invalidate the GC instrumentation:

**`mem2reg`** promotes an `alloca` to SSA only when its address never escapes (only
`load` and `store` uses, address never passed to a function). Every pointer-typed
`alloca` is passed to `hulk_rt_shadow_push`, making it permanently *address-taken*.
`mem2reg` therefore leaves all GC-relevant slots in memory. Numeric (`f64`) and
boolean (`i1`) allocas are never shadow-pushed and are freely promoted, reducing
redundant loads and stores on arithmetic code paths with no GC impact:

| Alloca type | Shadow-pushed | `mem2reg` | GC |
|---|---|---|---|
| `f64` (Number) | No | Promoted to SSA | N/A |
| `i1` (Boolean) | No | Promoted to SSA | N/A |
| `ptr` (object / string / vector) | Yes | Stays in memory | Correct |
| `{ ptr, ptr }` (fat protocol pointer) | Yes | Stays in memory | Correct |

**`instcombine`** and **`simplifycfg`** cannot remove calls to external functions —
LLVM conservatively treats them as having side effects. All shadow-stack and GC
functions are external to the module and are therefore untouchable by these passes.

**`inline`** copies callee instructions into the caller. Push/pop pairs are emitted at
HULK *lexical scope* boundaries, not at LLVM function boundaries. Inlining a HULK
function copies its scope-entry and scope-exit instrumentation intact, preserving
the push/pop balance exactly in the merged body.

---

## Supported Language Features

| Category | Constructs |
|---|---|
| **Literals** | `Number` (f64), `Boolean` (i1), `String` (immortal global constant) |
| **Variables** | `let`/`in`, shadowing, lexical scopes, destructive assignment (`:=`) |
| **Arithmetic** | `+`, `-`, `*`, `/`, `%`, `^` (pow) |
| **Comparisons** | `==`, `!=`, `<`, `<=`, `>`, `>=` |
| **Logical** | `&`, `|`, `!` (short-circuit for `&` and `|`) |
| **String ops** | `@` (concat), `@@` (concat with auto-stringify) |
| **Control flow** | `if`/`elif`/`else`, `while`, `for`/`in` over any `Iterable<T>` |
| **Blocks** | Sequence of expressions; value is the last expression |
| **Functions** | Global declarations, calls (direct and via `Type::Function`), recursion |
| **Methods** | Virtual dispatch via vtable, `base` delegation, `self` |
| **Method references** | `obj.method` without `()` produces a `{ fn_ptr, self_ptr }` thunk |
| **Objects** | `new T(args)`, attribute reads, attribute writes with retain/release |
| **Inheritance** | Single; vtable slots assigned parent-first in stable order |
| **Type tests** | `is` via vtable-chain walk (`hulk_rt_downcast_check`) |
| **Downcasts** | `as` with runtime check; narrows pointer type on success |
| **Vectors** | Literals, `[expr | var in iter]` comprehensions, `v[i]` indexing, `.size()` |
| **Protocols** | Structural conformance, fat-pointer itable dispatch |
| **Pattern matching** | `match` with literal, type+binding, variable, and wildcard patterns |
| **Math builtins** | `sin`, `cos`, `sqrt`, `log`, `exp`, `rand`, `pow`, `PI`, `E` |

---

## Cross-Compilation

`hulk-codegen` always targets `x86_64-unknown-linux-gnu` regardless of the build host.

- On **Linux/WSL**: the host toolchain and the target are the same; produced binaries
  run directly.
- On **Windows**: LLVM's `x86_64-unknown-linux-gnu` target machine is used for IR
  compilation; `clang --target=x86_64-unknown-linux-gnu` handles linking. The resulting
  ELF binary must be copied to a Linux environment (e.g. WSL) for execution.
- On **macOS**: same cross-compilation path as Windows; requires a cross linker
  targeting Linux.

---

## Testing

```bash
cargo test -p hulk-codegen
```

`hulk-codegen` has **79 tests** across two levels:

### Level 1: IR unit tests (`src/lower/mod.rs`, 43 tests)

Each test constructs a minimal `Expr<Type>` node with typed helper functions (`num`,
`bin_op`, `if_expr`, `let_expr`, etc.), runs it through `lower_expr`, and asserts
`Module::verify()` passes. Selected tests also inspect the IR text directly — for
example, verifying that a pointer `alloca` is not promoted after an `O2` pass, or
that a field map contains exactly the expected byte offsets.

These tests are independent of the lexer, parser, and semantic analyzer, and do not
require `hulk-rt` to be linked. Each test exercises exactly one lowering submodule.

### Level 2: Integration / golden tests (`src/lib.rs`, 36 tests)

Each test runs the full pipeline from a HULK source string through all compiler stages
and asserts that the resulting file begins with the ELF magic bytes (`0x7f 'E' 'L' 'F'`).

A subset links the object against `libhulk_rt.a`, executes the binary, and compares
its stdout or exit code against a hard-coded expected value. These **golden tests**
are the only layer that certifies observable runtime behavior — that the emitted code
not only has valid structure but produces the correct program output.

Coverage across both levels:

- Every construct in the language feature table above
- GC instrumentation: shadow-stack IR shape after `O2`; field-map contents for types
  with zero, one, and multiple pointer-typed fields; `gc_collect` reclaiming binary
  and ternary reference cycles without freeing live objects
- Optimization: numeric allocas promoted to SSA at `O2`; pointer allocas retained in
  memory at `O2`; golden test outputs identical at `O0` and `O2`

---

## Setup

### Ubuntu / WSL

```bash
./scripts/setup_llvm17_ubuntu.sh
cargo build -p hulk-rt --release
cargo test -p hulk-codegen
```

### Windows

```powershell
./scripts/setup_llvm17_windows.ps1
# Set $env:LLVM_SYS_170_PREFIX as printed by the script, then:
cargo build -p hulk-rt --release
cargo test -p hulk-codegen
```

Golden tests that execute the final ELF binary are skipped automatically on Windows;
all IR-level and ELF-validity tests run on all platforms.

### Toolchain requirements

- **LLVM 17** development headers (`llvm-17-dev` on Debian/Ubuntu; LLVM 17.0.x installer on Windows)
- **`clang`** or **`gcc`** capable of targeting `x86_64-unknown-linux-gnu`
- **`hulk-rt`** built as a `staticlib` (`cargo build -p hulk-rt --release`) before running
  any test that links and executes a binary
