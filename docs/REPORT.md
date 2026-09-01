# HULK Compiler

### Design, Implementation, and Extensions

**Project Report**
Courses: *Programming Languages* and *Compilation*

**Authors:**

| Name | Handle |
|---|---|
| Darío Francisco Alfonso Urrutia | `@D4R102004` |
| Juan Carlos Carmenate Díaz | `@Juank404` |
| Sebastian González Alfonso | `@sebagonz106` |

Faculty of Mathematics and Computer Science
University of Havana
June 2026

Repository: <https://github.com/D4R102004/hulk-compiler>

---

## Abstract

This report describes the design and implementation of a complete compiler for the HULK language (*Havana University Language for Kompilers*), developed as the final project for the Programming Languages and Compilation courses at the University of Havana. The compiler is written entirely in Rust and comprises a hand‑written lexer, a hand‑written LL(1) recursive‑descent parser, a five‑pass semantic analyzer with type inference, and a code generator targeting LLVM 17 via the `inkwell` crate. The compiler covers all HULK features through type verification (§16.8 of the official specification) and introduces three language extensions: **match expressions** for structured pattern matching, **function types** enabling first‑class function values, and **vectors** with functional‑style comprehension syntax. The report analyzes the key design decisions, compares them with the solutions adopted by languages such as Kotlin, Rust, Haskell, and Python, and documents the validation test suite that was carried out.

**Keywords:** compilers, HULK, Rust, LLVM, type inference, pattern matching, first‑class functions, vectors.

---

## Table of Contents

1. [Introduction](#1-introduction)
2. [General Description of the HULK Language](#2-general-description-of-the-hulk-language)
3. [Proposed Extensions](#3-proposed-extensions)
4. [Design and Architecture Analysis](#4-design-and-architecture-analysis)
5. [Testing](#5-testing)
6. [Discussion](#6-discussion)
7. [Conclusions](#7-conclusions)
8. [Limitations and Recommendations](#8-limitations-and-recommendations)
9. [References](#9-references)
10. [Appendix: Complete Grammar](#10-appendix-complete-grammar)

---

## 1. Introduction

### 1.1 History of compilation and programming languages

The birth of compilers is closely tied to the origin of modern programming. In 1952, Grace Hopper developed the first known compiler, the A‑0 System, which translated symbolic mathematical code into machine instructions [1]. This milestone marked the start of a new era: for the first time, programmers could express algorithms at a level of abstraction higher than assembly language.

The decisive leap came in 1957 with FORTRAN (*Formula Translation*), developed by John Backus and his team at IBM [2]. FORTRAN demonstrated that it was possible to generate machine code as efficient as hand‑written code, dispelling the doubts of those who believed abstraction necessarily sacrificed performance. FORTRAN was followed by LISP in 1958, which introduced recursion and treated functions as first‑class objects [3] — concepts that remain relevant in language design today.

The 1960s and 1970s saw the consolidation of the formal theory behind compilation. Noam Chomsky formulated the hierarchy of formal grammars [4], while Donald Knuth defined LR(k) languages [5], laying the mathematical foundations of syntactic analysis. The publication of the “dragon book” (*Compilers: Principles, Techniques, and Tools*) by Aho, Sethi, and Ullman in 1986 codified the accumulated knowledge into a canonical reference still used in university courses worldwide [6].

The modern state of the art includes compiler infrastructures of considerable complexity, such as GCC and LLVM. The latter, originally developed by Chris Lattner in 2003, introduced a transformative concept: a low‑level, typed *intermediate representation* (IR) that allows the *front end* (analysis and typing) to be fully separated from the *back end* (native code generation) [7]. Today, LLVM is the backbone of compilers for languages such as Clang (C/C++), Swift, Kotlin Native, and Rust.

### 1.2 The HULK language and the project context

HULK (*Havana University Language for Kompilers*) is a didactic programming language designed at the Faculty of Mathematics and Computer Science of the University of Havana, conceived specifically for the study of compilation. HULK is a statically typed, object‑oriented, expression‑based language: every syntactic construct — including blocks, conditionals, and loops — produces a value.

The language includes a type system with inference, single inheritance, polymorphism, protocols (equivalent to structural interfaces), iterables, and vectors. Its deliberately compact design allows a team of students to implement a complete compiler over the course of a single semester, covering all the classic phases: lexical, syntactic, and semantic analysis, and code generation.

This project, developed as the final assignment for the Programming Languages and Compilation courses, consists of a complete implementation of a HULK compiler in the Rust language, generating native code for Linux x86_64 via the LLVM 17 *backend*.

### 1.3 Motivation and objectives of the extensions

The base HULK specification covers the fundamental mechanisms of statically typed object‑oriented programming. However, the original language lacks modern expressive tools that allow more concise, safe, and readable code to be written. This project introduces three extensions to the language, each motivated by concrete needs:

1. **`match` expressions.**
   The absence of pattern matching forces verbose, error‑prone chains of `if`/`elif`/`else`. The `match` expression allows the value and type of an expression to be inspected declaratively, following the model of languages such as Rust [17], Kotlin [19], and Haskell [23].

2. **Function types and first‑class functions.**
   In base HULK, functions are global entities and cannot be passed as arguments or stored in variables. This extension introduces the type `(T₁, ..., Tₙ) -> R` to represent functions as values, enabling higher‑order functional programming: passing functions as arguments, returning functions from functions, and method references.

3. **Vectors with comprehensions.**
   Homogeneously typed collections are essential in any practical language. This extension adds the type `Vector<T>` with literal syntax (`[1, 2, 3]`) and indexing, as well as functional‑style vector comprehensions (`[expr | var in iterable]`).

The three design goals guiding these extensions are: (1) *consistency* with HULK's expression‑based semantics, (2) statically verified *type safety*, and (3) *expressiveness* comparable to that of modern languages such as Kotlin and Rust.

### 1.4 Structure of the report

The rest of the report is organized as follows. Section 2 describes the original HULK language, its paradigms, and its type system. Section 3 presents the three proposed extensions in detail, with their formal syntax, typing rules, dynamic semantics, and comparisons with other languages. Section 4 analyzes the complete architecture of the compiler and justifies the most relevant design decisions. Section 5 documents the testing methodology and the most significant test cases. Section 6 discusses the difficulties encountered and the alternatives that were discarded. Section 7 presents the conclusions of the work, and Section 8 summarizes the limitations identified in the current state of the compiler together with prioritized recommendations for future work.

---

## 2. General Description of the HULK Language

### 2.1 Paradigm and language philosophy

HULK (*Havana University Language for Kompilers*) is a statically typed, object‑oriented, **expression‑based** programming language. Unlike languages such as C or Java, where control‑flow constructs are *statements* (with no return value), in HULK every construct produces a value: blocks, conditionals, loops, and pattern matching are all expressions that can appear anywhere a value is expected. This philosophy, shared with languages such as Rust [17], Scala [18], and Kotlin [19], simplifies the language's semantics and encourages a more declarative programming style.

The simplest possible HULK program illustrates this idea:

**Listing 1.** *Minimal HULK program*
```hulk
print("Hello, World!");
```

A HULK program consists of a sequence of *global declarations* (functions, types, and protocols) followed by a single *entry expression* that constitutes the starting point of execution.

### 2.2 Type system

HULK implements a static type system with partial inference. Types are organized into a nominal hierarchy with `Object` as the root, and the system supports both explicit annotations and automatic inference in most contexts.

#### 2.2.1 Primitive types and hierarchy

The implemented compiler's type system defines the following fundamental types, represented as variants of the `Type` enum in the `hulk-semantic` module:

**Table 1.** *Primitive types of the HULK type system*

| HULK type | Internal variant | Description |
|---|---|---|
| `Number` | `Type::Number` | Floating‑point number (f64) |
| `String` | `Type::String` | Immutable string |
| `Boolean` | `Type::Boolean` | Logical value (`true`/`false`) |
| `Object` | `Type::Object` | Root of the nominal hierarchy |
| `T` (user‑defined) | `Type::Named(String)` | User‑defined type or protocol |
| `Vector<T>` | `Type::Vector(Box<Type>)` | Homogeneous collection (extension) |
| `T*` | `Type::Iterable(Box<Type>)` | Iterable protocol |
| `(T)->R` | `Type::Function{...}` | Function type (extension) |

`Number`, `String`, and `Boolean` are primitive value types: they cannot be inherited from and are represented without boxing during code generation. `Object` is the universal supertype — every HULK type conforms to `Object`. The types `Vector<T>` and `(T)->R` are extensions introduced by this project, described in detail in Section 3.

The conformance relation (*subtyping*) $T_1 \leq T_2$ is implemented in the `conforms_to` method and follows these rules:

1. **Reflexivity:** $T \leq T$ for every $T$.
2. **Top:** $T \leq \texttt{Object}$ for every $T$.
3. **Nominal inheritance:** $T \leq U$ if $U$ is an ancestor of $T$ in the inheritance hierarchy.
4. **Structural conformance to protocols:** $T \leq P$ (protocol) if $T$ implements all the methods of $P$ with the correct signatures.
5. **Covariance of `Vector`:** $\texttt{Vector}<T> \leq \texttt{Vector}<U>$ if $T \leq U$.
6. **Function types:** $(A_1,...,A_n) \to R_1 \leq (B_1,...,B_n) \to R_2$ if $B_i \leq A_i$ (contravariance in parameters) and $R_1 \leq R_2$ (covariance in return type).

#### 2.2.2 Type inference

HULK allows functions to be declared without explicit type annotations. The compiler infers types from usage:

**Listing 2.** *Type inference in HULK*
```hulk
// No annotations: the compiler infers Number -> Number
function square(x) {
    x * x;
}

// With explicit annotations
function add(x: Number, y: Number): Number {
    x + y;
}
```

The implemented semantic analysis resolves type inference in five ordered passes (see Section 4).

### 2.3 Main language features

#### 2.3.1 Variables and lexical scope

HULK uses `let ... in` as its variable‑binding construct. Scope is lexical, and variables can be shadowed in inner scopes:

**Listing 3.** *Variables and shadowing in HULK*
```hulk
let x = 10 in {
    if (x == 10) print("ok") else print("fail");
    let x = 20 in {       // x is shadowed
        if (x == 20) print("ok") else print("fail");
    };
    if (x == 10) print("ok") else print("fail");
};
```

The destructive assignment operator `:=` modifies the value of an existing variable without creating a new binding:

**Listing 4.** *Destructive assignment*
```hulk
let i = 0 in
let result = 0 in {
    while (i < 5) {
        result := result + i;
        i := i + 1;
    };
    print(result);   // prints 10
};
```

#### 2.3.2 Functions

Functions in HULK are global and support two syntactic forms: the *inline* form with `=>` and the block form with `{}`. They support recursion and mutual references (thanks to the declaration‑collection pass, which registers every signature before inferring the bodies; this is examined further in Section 4):

**Listing 5.** *Mutual recursion in HULK*
```hulk
function is_even(n: Number): Boolean {
    if (n == 0) true else is_odd(n - 1);
}

function is_odd(n: Number): Boolean {
    if (n == 0) false else is_even(n - 1);
}
```

The available built‑in functions are: `print`, `sqrt`, `sin`, `cos`, `exp`, `log`, `rand`, and `range`. In addition, `PI` and `E` are modeled as zero‑arity functions that act as mathematical constants.

#### 2.3.3 Object orientation

HULK implements object orientation with single inheritance and polymorphism. Types are declared with the `type` keyword, may take constructor parameters, and may implement methods. All attributes are private (accessible only within the type that declares them), and all methods are public and virtual:

**Listing 6.** *Types, inheritance, and polymorphism in HULK*
```hulk
type Animal(n: String) {
    name: String = n;
    sound(): String { "..."; }
}

type Dog(n: String) inherits Animal(n) {
    sound(): String { "Woof"; }
}

type Cat(n: String) inherits Animal(n) {
    sound(): String { "Meow"; }
}

// Polymorphism: a variable of type Animal holds a Dog
let a: Animal = new Dog("Rex") in print(a.sound()); // "Woof"
```

The `base` keyword allows delegation to the immediate parent's method:

**Listing 7.** *Using `base` for delegation*
```hulk
type FancyPrinter(prefix: String, suffix: String)
        inherits Printer(prefix) {
    sfx: String = suffix;
    format(msg: String): String {
        base(msg) @ self.sfx;   // delegates to Printer.format and appends a suffix
    }
}
```

#### 2.3.4 Protocols

Protocols in HULK define structural interfaces: a type implements a protocol if it possesses all of the protocol's methods with the correct signatures, with no need to declare this explicitly. This is *structural typing* (similar to Go's *traits* [16] or TypeScript's *implicit interfaces* [26]):

**Listing 8.** *Protocols as structural interfaces*
```hulk
protocol Printable {
    to_string(): String;
}

type Point(x: Number, y: Number) {
    x: Number = x;
    y: Number = y;
    to_string(): String { "point"; }
    // Point implicitly implements Printable
}

let p: Printable = new Point(1, 2) in
    print(p.to_string());
```

#### 2.3.5 Control flow

HULK offers the classic control constructs — `if`/`elif`/`else`, `while`, and `for` — all as expressions. The `for` loop iterates over any expression of type `Iterable<T>`, including ranges and vectors:

**Listing 9.** *Control flow in HULK*
```hulk
// if/elif/else as an expression
function grade(score: Number): String {
    if (score >= 90) "A"
    elif (score >= 80) "B"
    elif (score >= 70) "C"
    else "F";
}

// for over a range
for (x in range(1, 6))
    print(x);
```

### 2.4 The lexical analyzer

The HULK lexer, hand‑implemented in the `hulk-lexer` crate, recognizes the following token groups:

**Table 2.** *Token categories of the HULK lexer*

| Category | Tokens |
|---|---|
| Literals | `Number(f64)`, `StringLit(String)`, `True`, `False` |
| Arithmetic | `Plus`, `Minus`, `Star`, `Slash`, `Caret`, `Percent` |
| Strings | `At` (`@`), `AtAt` (`@@`) |
| Comparison | `EqEq`, `Neq`, `Lt`, `Gt`, `Leq`, `Geq` |
| Booleans | `And`, `Or`, `Not` |
| Assignment | `Assign` (`=`), `ColonEq` (`:=`) |
| Delimiters | `LParen`, `RParen`, `LBrace`, `RBrace`, `LBracket`, `RBracket` |
| Separators | `Semicolon`, `Comma`, `Colon`, `Dot` |
| Arrows | `Arrow` (`->`), `FatArrow` (`=>`) |
| Keywords | `let`, `in`, `if`, `elif`, `else`, `while`, `for`, `function`, `type`, `inherits`, `new`, `self`, `base`, `is`, `as`, `protocol`, `match`, `case` |

The lexer detects and reports the following lexical errors: unterminated strings (`UnterminatedString`), unrecognized characters (`UnexpectedChar`), and invalid escape sequences (`InvalidEscape`). It is worth noting that the word `base` is a *contextual keyword*: the lexer emits it as `TokenKind::Base`, but the parser also accepts it as an identifier in positions naming a variable, attribute, or parameter. It is important to note that this differs from C#, where `base` is a reserved word in every position; the mechanism instead is closer to Kotlin's *soft keywords*, such as `by` or `where`, which are reserved only in the syntactic position where they carry special meaning and remain valid identifiers in any other context [19].

---

## 3. Proposed Extensions

This project introduces three extensions to the original HULK language, all implemented with real modifications to the syntax, the AST, and the semantic analysis. Each extension is motivated by concrete shortcomings of the base language and justified through comparisons with modern languages.

### 3.1 `match` expressions

#### 3.1.1 Description

The **`match` expression** introduces structured pattern matching into HULK. The base language offers only `if`/`elif`/`else` for discriminating between values, which results in verbose code whenever a value must be compared against multiple cases. The `match` expression, being an expression (not a statement), produces a value that can be used directly in any context.

The extended grammar for `match` is:

**Listing 10.** *BNF grammar of `match`*
```text
MatchExpr  ::= 'match' Expr '{' MatchCase+ '}'
MatchCase  ::= 'case' Pattern '=>' Expr ';'
Pattern    ::= '_'                          -- wildcard
             | Literal                     -- literal value
             | Identifier                  -- variable binding
             | TypeRef (',' Identifier)?   -- type pattern
```

Examples of each kind of pattern:

**Listing 11.** *Literal patterns*
```hulk
let x = 42 in
    match x {
        case 1  => print("one");
        case 42 => print("forty-two");
        case _  => print("other");
    };
```

**Listing 12.** *Type patterns with an inheritance hierarchy*
```hulk
type Animal { sound(): String { "..."; } }
type Dog inherits Animal { sound(): String { "Woof"; } }
type Cat inherits Animal { sound(): String { "Meow"; } }

let a: Animal = new Dog() in
    match a {
        case d: Dog => print(d.sound());   // narrows to Dog
        case c: Cat => print(c.sound());   // narrows to Cat
        case _      => print("unknown");
    };
```

**Listing 13.** *`match` as an expression — the result is assigned*
```hulk
function describe(s: String): String {
    match s {
        case "hello" => "greeting";
        case "bye"   => "farewell";
        case _       => "unknown";
    }
}
print(describe("hello"));   // greeting
```

The semantic analyzer implements `match` inference in the `infer_match` function of the `hulk-semantic` module. The typing rules are:

1. The scrutinee (the expression following `match`) is inferred normally.
2. Each case infers its body within an environment extended according to the pattern:
   - `Wildcard` (`_`): adds no bindings.
   - `Literal`: checks type conformance with the scrutinee.
   - `Variable(name)`: binds `name` to the scrutinee's type.
   - `Type(T, binding)`: checks that `T` is a known type; if there is a binding, it binds it with type `T` (narrowing).
3. The type of the whole `match` is the *lowest common ancestor* (LCA) of the types of all case bodies.
4. If no case is a *catch‑all* (`_` or a variable), the compiler emits a `NonExhaustiveMatch` warning.

At runtime, evaluation of `match x { case p => e }` proceeds as follows:

1. `x` is evaluated exactly once and the result is stored.
2. The cases are evaluated in declaration order.
3. For the first pattern that matches, its body is evaluated and the result is returned.
4. If no pattern matches, the runtime calls `hulk_rt_match_fail()`, which terminates the program.

Matching for type patterns uses `hulk_rt_downcast_check`, which walks the vtable chain to verify whether the object is an instance of the expected type.

#### 3.1.2 Comparison with other languages

**Table 3.** *Comparison of match/switch across different languages*

| Language | Construct | Differences from HULK |
|---|---|---|
| Rust [17] | `match` | Exhaustiveness is mandatory; destructuring patterns for structs/enums |
| Kotlin [19] | `when` | Can be either an expression or a statement; supports ranges |
| Haskell [23] | `case of` | Exhaustiveness checked; guards with `\|` |
| Python [24] | `match` (3.10+) | Structural patterns; sequence destructuring |
| Java [12] | `switch` (14+) | Switch expression with `->`; no automatic type narrowing |
| HULK | `match` | Always an expression; type narrowing; warning if non‑exhaustive |

The decision to emit a *warning* (not an error) for non‑exhaustive matches follows Kotlin's model [19], which also allows non‑exhaustive matches but warns the programmer. Rust [17], by contrast, rejects them at compile time — a safer decision, but one that hinders incremental development.

### 3.2 Function types and first‑class functions

#### 3.2.1 Description

In base HULK, functions are global entities and cannot be stored in variables or passed as arguments. This extension introduces the **function type** `(T₁,...,Tₙ) -> R`, which allows functions and methods to be treated as first‑class values. This enables higher‑order functional programming: function composition, passing callbacks, and method references.

The function type is written as:

**Listing 14.** *Function type syntax*
```text
TypeRef ::= ... | '(' TypeList? ')' '->' TypeRef
```

Example usage:

**Listing 15.** *Higher‑order function with a function type*
```hulk
function apply(f: (Number) -> Number, x: Number): Number {
    f(x);
}

{
    if (apply(function (x: Number): Number -> x + 1, 5) == 6)
        print("ok")
    else
        print("fail");
}
```

Method references are obtained by accessing a method without calling it:

**Listing 16.** *Method reference as a value*
```hulk
type Counter(init: Number) {
    val: Number = init;
    inc(): Number { self.val := self.val + 1; self.val; }
}

let c = new Counter(0) in {
    let f: () -> Number = c.inc in {  // reference to the method
        print(f());   // calls c.inc()
        print(f());   // calls c.inc() again
    };
};
```

The type system extends the `Type` enum with the variant:

**Listing 17.** *`Type::Function` variant in Rust*
```rust
pub enum Type {
    // ... existing types ...
    /// Function type: (params) -> return_type.
    /// Used for method references and lambda expressions.
    Function {
        params: Vec<Type>,
        return_type: Box<Type>,
    },
    // ...
}
```

The subtyping relation for function types follows the standard rule from type theory: **contravariance in parameters, covariance in return type**:

$$
\frac{B_i \leq A_i \quad R_1 \leq R_2}
     {(A_1,...,A_n) \to R_1 \leq (B_1,...,B_n) \to R_2}
$$

This rule guarantees that a more specific function can be used wherever a more general function is expected.

When the callee of a call has type `Type::Function`, the semantic analyzer extracts the parameter and return types, checks arity and the types of the arguments, and produces a typed `CallExpr` annotated with the return type.

A method reference `obj.method` is represented at runtime as a two‑word thunk: a pointer to the function and a pointer to the receiver (`self`). This allows the method to be called later without knowing the receiver's concrete type.

#### 3.2.2 Comparison with other languages

**Table 4.** *First‑class functions across different languages*

| Language | Function type | Subtyping |
|---|---|---|
| Kotlin [19] | `(T) -> R` | Contravariant/covariant; function type vs. type |
| Rust [17] | `fn(T) -> R` / `Fn(T) -> R` | Traits; closures capture their environment |
| Haskell [22] | `T -> R` | Curried; fully first‑class |
| Java [11] | `Function<T,R>` | Functional interfaces; lambdas since Java 8 |
| TypeScript [26] | `(x: T) => R` | Structural; compatible by shape |
| HULK | `(T) -> R` | Contravariant/covariant; thunk for methods |

The decision to use a two‑word thunk for method references follows the model of Kotlin and C# [19], where `obj::method` creates a delegate that captures the receiver. In Rust [17], method references require explicit closures (`|x| obj.method(x)`), which is more verbose. The comparison with these languages can be examined in greater depth in Table 4.

### 3.3 Vectors with comprehensions

#### 3.3.1 Description

This extension adds the type **`Vector<T>`** as a fixed‑size homogeneous collection with support for:
- Literals: `[1, 2, 3]`
- Indexing: `v[0]`
- Methods: `v.size()`, `v.get(i)`, `v.set(i, x)`
- Comprehensions: `[expr | var in iterable]`
- Iteration with `for`

**Listing 18.** *BNF grammar of vectors*
```text
VectorExpr  ::= '[' VectorBody
VectorBody  ::= ']'
              | ExprNoTopOR VectorTail
VectorTail  ::= '|' id 'in' Expr ']'   -- comprehension
              | (',' Expr)* ']'          -- literal
TypeRef     ::= TypeRef '[]'            -- sugar: T[] = Vector<T>
              | TypeRef '*'             -- sugar: T* = Iterable<T>
```

**Listing 19.** *Vector literal and indexing*
```hulk
let v = [1, 2, 3, 4, 5] in {
    print(v[0]);      // 1
    print(v.size());  // 5
};
```

**Listing 20.** *Functional‑style vector comprehension*
```hulk
// Squares from 1 to 5
let squares = [x * x | x in range(1, 6)] in
    for (s in squares)
        print(s);
// Output: 1 4 9 16 25
```

A notable detail of the parser is its resolution of the ambiguity between the boolean operator `|` and the comprehension separator. The grammar uses a special production, `ExprNoTopOR`, for the left‑hand side of the comprehension, which does not consume `|` at the top level. This allows `[(x | y) | x in values]` to be valid, because the inner OR expression is parenthesized.

The vector inference implemented in `infer_vector` follows these rules:

1. **Literal**: the type of the vector is `Vector<T>`, where `T` is the LCA of the types of all the elements.
2. **Comprehension**: the type is `Vector<T>`, where `T` is the type of the element expression, after inferring the type of the iteration variable from the iterable.
3. **Indexing**: `v[i]` requires that `v` be `Vector<T>` and `i` be `Number`; it returns `T`.
4. **Covariance**: `Vector<T>` $\leq$ `Vector<U>` if `T` $\leq$ `U`.
5. **Iterability**: `Vector<T>` automatically implements `Iterable<T>`.

#### 3.3.2 Comparison with other languages

**Table 5.** *Collections and comprehensions across different languages*

| Language | Collection | Comprehension |
|---|---|---|
| Python [25] | `list[T]` | `[x*x for x in range(5)]` |
| Haskell [23] | `[T]` | `[x*x \| x <- [1..5]]` |
| Kotlin [19] | `List<T>` | `(1..5).map { it * it }` |
| Rust [17] | `Vec<T>` | `(1..6).map(\|x\| x*x).collect()` |
| Java [11] | `List<T>` | Streams: `.stream().map(...).collect(...)` |
| HULK | `Vector<T>` | `[x*x \| x in range(1,6)]` |

HULK's comprehension syntax (`[expr | var in iter]`) is deliberately similar to that of Haskell [23] and Python [25], which are the most concise and readable. The decision to use `|` as the separator (instead of `for`, as in Python) follows the mathematical notation of set‑builder notation: $\{x^2 \mid x \in \mathbb{N}\}$.

---

## 4. Design and Architecture Analysis

### 4.1 Overview of the pipeline

The implemented HULK compiler follows the classic multi‑phase compiler architecture, where each phase transforms one representation of the program into a more elaborate one. The complete pipeline is:

```
source.hulk → hulk-lexer → hulk-parser → hulk-semantic → hulk-codegen → hulk-rt → ./output
```

The Rust workspace is organized into seven crates with clearly separated responsibilities, following the single‑responsibility principle (SRP):

**Table 6.** *Workspace crates and their responsibilities*

| Crate | Responsibility |
|---|---|
| `hulk-ast` | Generic abstract syntax tree, parameterized by the annotation type |
| `hulk-lexer` | Lexical analysis: source to `Vec<Token>` |
| `hulk-parser` | LL(1) syntactic analysis: tokens to `Program<()>` |
| `hulk-semantic` | Five‑pass semantic analysis: `Program<()>` to `VerifiedProgram` |
| `hulk-codegen` | LLVM IR, object, and executable code generation |
| `hulk-rt` | Runtime library: GC, strings, vectors, builtins |
| `hulk-cli` | Entry point: orchestrates the pipeline, manages exit codes |

### 4.2 The generic, parameterized AST

A fundamental design decision is that the AST (`hulk-ast`) is parameterized by an annotation type `A`:

**Listing 21.** *Generic AST in Rust*
```rust
pub struct Expr<A = ()> {
    pub kind: ExprKind<A>,
    pub anno: A,         // () before semantic analysis; Type afterward
    pub span: SourceSpan,
}
```

This allows the same tree to represent two different states of the program:

- `Program<()>` — untyped tree, produced by the parser
- `Program<Type>` — typed tree, produced by the semantic analyzer

This technique, known as a *typed syntax tree* or *annotated AST*, is used by professional compilers such as GHC (Haskell) and rustc, which descend from the tradition of explicitly representing, at every stage of the compiler, the typing state of the program [21]. The main advantage is that the compiler cannot accidentally mix typed and untyped trees — Rust's type system guarantees this at compile time.

### 4.3 The lexical analyzer

The lexer implemented in `hulk-lexer` is a hand‑written deterministic finite automaton (DFA). It reads the source character by character and emits tokens with position information (`SourceSpan`). Some of the important design decisions taken include:

- **Table‑free lexer**: each state is a branch of Rust code, not a transition table. This is faster and easier to debug than lexer generators such as Flex.
- **Keywords as post‑processing**: the lexer emits every identifier as `Identifier`; a subsequent post‑processing step converts reserved words into their corresponding tokens.
- **`base` as a contextual keyword**: resolved in the parser, not in the lexer, following the same principle as Kotlin's soft keywords [19] rather than that of a word reserved in every position, as `base` is in C#.
- **Recoverable errors**: the lexer reports errors but attempts to continue, so that it can report multiple errors in a single pass.

The recognized tokens were presented previously in Table 2 of Section 2.

#### 4.3.1 Lexical error recovery and continuous operation mode

The lexer implements a *simplified panic‑mode* error‑recovery strategy: when it encounters an unrecognized character, it emits an `UnexpectedChar` error token with the exact `SourceSpan` and advances to the next character, continuing the analysis instead of aborting. This decision follows the multiple‑diagnostics principle described in the classic compiler literature [6]: a compiler is significantly more useful when it exposes the programmer to the full set of errors present in the program in a single invocation, rather than stopping at the first one. The three kinds of lexical error the compiler can produce are:

- **`UnterminatedString`**: a string literal opened with `"` but not closed before the end of the line or file. The lexer consumes up to the next line break and emits the error with the opening position, so that the parser can interpret the following token as if the string had ended.
- **`UnexpectedChar`**: any character that does not belong to the language's alphabet (e.g. `#`, `$`, `\`). A single‑character error token is emitted and lexical analysis unconditionally advances.
- **`InvalidEscape`**: an unknown escape sequence inside a string (e.g. `\q`). The two‑character sequence is consumed and analysis of the string continues, preventing the character following the escape from being misinterpreted as the closing delimiter.

In every case, the `SourceSpan` included in the error token contains the line number, the starting column, and the length of the problematic fragment — information that `hulk-cli` uses to format diagnostics with an underline at the exact position in the source code.

### 4.4 The LL(1) syntactic analyzer

The parser is a hand‑implemented LL(1) recursive‑descent analyzer. Each non‑terminal of the grammar corresponds to a Rust method:

**Listing 22.** *Recursive‑descent parser structure*
```rust
impl Parser {
    fn parse_expression(&mut self) -> Result<Expr, ParseError> {
        self.parse_assignment()
    }
    fn parse_assignment(&mut self) -> Result<Expr, ParseError> {
        let lhs = self.parse_or()?;
        if self.peek_kind() == Some(&TokenKind::ColonEq) {
            self.advance();
            let rhs = self.parse_assignment()?;
            // build AssignExpr...
        }
        Ok(lhs)
    }
}
```

The most important technical points of the grammar are:

1. **Left‑recursion elimination**: precedence rules ($E \to E \; op \; T \mid T$) are transformed into the iterative form ($E \to T \; E'$, $E' \to op \; T \; E' \mid \varepsilon$).
2. **Resolution of vector ambiguity**: the `|` operator appears both in boolean expressions and in comprehensions. This is resolved with a special production, `ExprNoTopOR`.
3. **Contextual `base`**: the `parse_name()` helper accepts `TokenKind::Base` in identifier positions, but promotes it to `ExprKind::BaseRef` only when it is the callee of a call.

#### 4.4.1 Error message quality and grammatical extensibility

A practical advantage of the hand‑written recursive‑descent parser is that, at every point of error, the parsing context is explicit in Rust's call stack. This allows messages such as *"expected `in` after the `for` variable"* instead of the generic *"syntax error"* produced by table‑driven LR parsers when the stack state is opaque to the user. All of the `parse_*` functions return `Result<Expr, ParseError>`, where `ParseError` carries the `SourceSpan` of the unexpected token and a description of the syntactic context in which the parser found itself.

The grammar's extensibility also benefits from this architecture. Adding the `match` expression (Section 3), for example, required only adding the `TokenKind::Match` branch to the `match` in `parse_primary`, implementing `finish_match_expression` and `parse_pattern` as new methods, and adding the corresponding nodes to the AST. No transition table was modified and no parser was regenerated: the change is purely additive and localized, which illustrates the open‑extensibility characteristic that this kind of design offers on the front of new primary‑expression forms [6].

The only trade‑off of this approach compared to automatic generators is that the LL(1) property was verified by inspection and through the test suite, not through a formal proof using FIRST/FOLLOW tables. In practice, the look‑ahead conflicts detected during development — the `|` operator shared between disjunction and the comprehension separator, and the common `id` prefix between a method and an attribute in `TypeMember` — were resolved through the factorizations documented in the Appendix.

### 4.5 The five‑pass semantic analyzer

Semantic analysis is organized into five ordered passes, implementing the *two‑pass binder* pattern described by Immo Landwerth in his video series on the construction of the Minsk compiler [27]:

**Table 7.** *The five passes of the semantic analyzer*

| Pass | Name | Responsibility |
|---|---|---|
| 0 | `collect` | Registers every global declaration in the `TypeRegistry`. Enables forward references. |
| 1 | `hierarchy` | Resolves the inheritance hierarchy and protocol conformances. Detects cycles and incompatibilities. |
| 1.5 | `resolve_constructor_params` | Infers constructor parameter types from attribute initializers. |
| 2 | `infer` | Bidirectional type inference. Produces `Program<Type>`. |
| 3 | `check` | Final verification: type conformance, attribute privacy, correct use of `self`. |

The separation between `infer` (pass 2) and `check` (pass 3) simplifies the code: `infer` only assigns types (it may produce `Type::Unknown`), while `check` operates on the already‑typed tree and assumes all types have been resolved.

#### 4.5.1 Bidirectional inference and `Type::Unknown` propagation

The `infer` pass implements bidirectional type inference [14]: it combines synthesis (bottom‑up, where the type of an expression is deduced from its subexpressions) with checking (top‑down, where the type expected at a position — the *push* type — is propagated into the subexpressions to refine inference). The most illustrative case is assigning a *lambda* to an annotated variable:

**Listing 23.** *Bidirectional inference in a lambda assignment*
```hulk
let f: (Number) -> Number = function(x: Number): Number -> x + 1;
```

The push type `(Number) -> Number`, coming from `f`'s annotation, is propagated into the *lambda* to check that its synthesized type is compatible before it is definitively annotated. This interaction lets inference converge without global unification [20], which simplifies the implementation at the cost of requiring explicit annotations at some entry points that cannot be inferred locally (mainly, the parameters of global functions).

When an expression cannot be typed in a single pass — for example, when a method calls another whose return type has not yet been inferred — the `infer` pass produces `Type::Unknown` on the corresponding node. The subsequent `check` pass treats any residual `Unknown` as an unresolved type error and reports a precise diagnostic. This discipline guarantees that `hulk-codegen` never receives a typed tree containing unknowns: the `VerifiedProgram` interface that separates the front end from the back end is, in effect, a static proof of the absence of `Unknown`.

#### 4.5.2 Computing the lowest common ancestor

Several language constructs require computing the result type of alternative branches: the arms of `if`/`elif`/`else`, the cases of `match`, and the elements of a vector literal must share a common type. In all of these contexts, the semantic analyzer computes the *lowest common ancestor* (LCA) of the involved types within HULK's nominal hierarchy. The LCA is resolved in the `hierarchy` pass — which has already built the complete inheritance tree with DFS intervals for O(1) type checking — and its result is used in `infer` to annotate the containing node with the most specific type that covers all alternatives. If the types are incompatible (they have no LCA other than `Object`), the system may produce `Type::Object` as a conservative fallback, or a type error if the position does not accept `Object` as a valid type.

### 4.6 The LLVM code generator

The code generator uses LLVM 17 through the `inkwell` library. The public entry function is:

**Listing 24.** *Public API of `hulk-codegen`*
```rust
pub fn compile(
    verified: &VerifiedProgram,
    opts: &CodegenOptions,
) -> Result<(), CodegenError>
```

#### 4.6.1 Object model

Each HULK type is represented in memory as a structure with an object header (`ObjHeader`) followed by its attributes:

**Listing 25.** *Object memory layout*
```c
struct ObjHeader {
    ref_count: i64,       // reference counter
    gc_mark:   i8,        // mark-sweep flag
    next:      *ObjHeader, // GC list
    vtable:    *VTable,   // virtual method table
};
```

This layout — a common header followed by the inherited fields and then the type's own fields, in declaration order — matches the single‑inheritance case of the Itanium ABI for C++ [8], without any of the complications that multiple inheritance introduces in that same specification (secondary virtual tables, `this`‑pointer adjustment); since HULK does not support multiple inheritance, the simplest form of that model is obtained directly. The `vtable` is an array of function pointers in a stable order (parent‑before‑child, declaration order). A virtual call consists of loading the vtable pointer, indexing by the method's slot, and then calling it indirectly. When the receiver is of a sealed type (`Number`, `String`, `Boolean`, `Vector`) or of a user type with no observed subtypes within the compilation unit, this indirect call is replaced with a direct call; this closed‑world devirtualization is viable because a HULK program is compiled as a single closed unit, with no separate compilation and no dynamic loading of additional types after compilation.

#### 4.6.2 Protocols and interface tables

Protocols — HULK's form of structural typing — are compiled using interface tables (*itables*): for every `(concrete type, protocol)` pair that the program materializes at some point (assignment, argument passing, or conversion with `as`), a constant table is built with one pointer per protocol method. A variable of a protocol type is represented as a wide pointer, `{ data, itable }`.

HULK's design — resolving the table address entirely at compile time — matches the exact form of Rust's *trait objects* [17], but differs from Go's interfaces, whose method table is built lazily at runtime the first time a type is converted to an interface, precisely because Go allows implicit implementation without the compiler generally having a static closure of the set of (type, interface) pairs used in the program [16]. Java's interfaces sit at an intermediate point: their method table is resolved at class‑loading time, not at compile time, because the virtual machine must tolerate a class that implements an interface being loaded at a different time from the interface itself [11]. HULK has a complete static closure of the pairs used — structural conformance is verified by the front end, and the back end itself enumerates the pairs in a single pass over the typed program — so building the table at compile time, with no runtime lookup cost whatsoever, is the strictly superior option available, without paying the indirection cost that Go or Java must pay for reasons unrelated to HULK itself (dynamic loading, separate compilation).

#### 4.6.3 Memory management

HULK does not prevent the construction of self‑referential structures, so a pure reference‑counting scheme would permanently leak any cycle that a program constructs. The adopted design is therefore hybrid: reference counting as the fast path for the acyclic case, combined with a mark‑and‑sweep collector reserved for breaking the cycles that reference counting never frees on its own. This point in the design space — combining both techniques rather than choosing a single extreme — corresponds to the category that Bacon, Cheng, and Rajan identify as the most effective in practice within their unified taxonomy of garbage‑collection algorithms [9], and is, in substance, the same strategy adopted by CPython since its earliest versions: reference counting for the common path, with an additional generational collector reserved for breaking cycles [10].

This point in the design space corresponds to the category that Bacon, Cheng, and Rajan identify as the most effective in practice within their unified taxonomy of garbage‑collection algorithms [9], and is, in substance, the same strategy adopted by CPython since its earliest versions: reference counting for the common path, with an additional collector reserved for breaking cycles [10]. Table 8 summarizes the runtime‑interface functions exposed by `hulk-rt` for the memory‑management system.

**Table 8.** *Memory‑management functions in `hulk-rt`*

| Function | Responsibility |
|---|---|
| `hulk_rt_alloc(size)` | Allocates `size` bytes, chains the block into the global allocation list, updates the `ALLOC_BYTES` counter, and automatically triggers `hulk_rt_gc_collect` if the configurable threshold is exceeded. |
| `hulk_rt_retain(ptr)` | Increments `ref_count`. Treats `null` and immortal objects (string literals) as a no‑op. |
| `hulk_rt_release(ptr)` | Decrements `ref_count`; when it reaches zero, calls the type's destructor (`gc_free_object`), which frees type‑specific sub‑allocations (string data, a vector's array, etc.) and the header. |
| `hulk_rt_shadow_push(slot)` | Registers the address of a local pointer‑typed slot on the shadow stack. The GC dereferences each slot during the mark phase to find the live roots. |
| `hulk_rt_shadow_pop()` | Removes the most recent entry from the shadow stack (LIFO, mirroring scope exit). |
| `hulk_rt_gc_collect()` | Runs a complete mark‑and‑sweep cycle: it walks the shadow stack to mark reachable objects and their descendants (using the field maps embedded in the vtables), then sweeps the allocation list, freeing unmarked objects. |

- **Reference counting**: every assignment increments the counter (`hulk_rt_retain`), and every release decrements it (`hulk_rt_release`). When it reaches zero, the object is freed. `hulk-rt` exposes `hulk_rt_alloc`, `hulk_rt_retain`, and `hulk_rt_release`, and `hulk-codegen` invokes them systematically when constructing objects, when assigning to members, and in particular when closing each lexical scope (`pop_scope` automatically releases every local variable of a dynamically allocated type).
- **Cyclic mark‑sweep**: to handle circular references, the GC maintains a *shadow stack* of roots, and, once memory exceeds a configurable threshold, `hulk_rt_gc_collect()` walks the list of allocations (threaded through the `next` field of `ObjHeader`) and frees unreachable objects. This half of the model is fully implemented: `hulk-rt` provides the shadow‑stack mechanism and `hulk_rt_gc_collect`, and `hulk-codegen` emits the field map that each traversal needs alongside every vtable, as detailed below.

#### 4.6.4 Cycle collector: shadow stack and field maps

The **mark** phase of the cycle collector needs to enumerate all live roots (pointers held in local variables and parameters of every function active on the call stack) without directly accessing the native C stack, whose layout is not portable. The adopted solution — a *shadow stack* maintained by the generated code itself, not by the operating system — is classic in the precise garbage‑collection literature [28]: the compiler instruments every pointer‑typed local variable with calls to `hulk_rt_shadow_push` on scope entry and to `hulk_rt_shadow_pop` on scope exit.

Concretely, `hulk-codegen` emits the following LLVM IR pattern in `declare_var` for every variable of a dynamically allocated type (user objects, strings, vectors, and protocol wide pointers):

**Listing 26.** *LLVM IR pattern for a pointer‑typed variable with a shadow stack*
```text
%x = alloca ptr                          ; the slot lives in memory
store ptr %init_val, ptr %x
call void @hulk_rt_shadow_push(ptr %x)  ; registers the slot's ADDRESS
; ... scope body ...
; on scope exit (pop_scope):
%x_val = load ptr, ptr %x
call void @hulk_rt_release(ptr %x_val)  ; reference counting
store ptr null, ptr %x                  ; double‑free protection
call void @hulk_rt_shadow_pop()         ; deregisters the root
```

The fact that the slot's address escapes into `hulk_rt_shadow_push` has a beneficial side effect: LLVM classifies that slot as *address‑taken* and automatically excludes it from the `mem2reg` pass, guaranteeing that the slot stays in memory and can be updated by the code generator without breaking the optimizer's invariants (see Section 4.7).

For the mark phase to be able to follow pointers inside heap objects without a hand‑written per‑type trace function, `hulk-codegen` emits a *field map* for every user type: a constant array of `i64` integers containing the byte offsets of the type's pointer‑typed fields, terminated by a `-1` sentinel. This array is stored in an LLVM global and referenced from slot 0 of each type's virtual table:

**Listing 27.** *Vtable layout with the field map in slot 0*
```text
@Node.field_map = constant [3 x i64] [i64 48, i64 56, i64 -1]
                          ;             ^val     ^next   ^sentinel

@Node.vtable = constant [5 x ptr] [
  ptr @Node.field_map,   ; slot 0 -> reserved for the GC
  ptr @Node.value,       ; slot 1 -> first method
  ptr @Node.next,        ; slot 2
  ...
]
```

The resulting mark algorithm is therefore:

1. For every slot registered on the shadow stack, dereference the address to obtain the pointer to the object.
2. From the object's header, load the `vtable` field.
3. From `vtable[0]`, load the field map.
4. For every offset in the map (up to the `-1` sentinel), compute `object_pointer + offset` and recursively mark the child object.

The sweep walks the global linked list of allocations (maintained through `ObjHeader.next`) and frees, via `gc_free_object`, every block whose mark (`ObjHeader.gc_mark`) was not set during the mark phase — that is, those that form cycles unreachable from any live root.

#### 4.6.5 Robustness and security

The two most significant mitigations implemented to guarantee safety when freeing memory are:

1. **Header poisoning on release**: immediately before returning a block to the allocator, `hulk_rt_release` writes `type_tag = TAG_FREED` (value `0xFF`, never used by a live object) and `ref_count = i64::MIN` (an impossible value for a real object). If any subsequent dangling pointer attempts to retain or release that block before the allocator recycles it, the `TAG_FREED` check at the start of `hulk_rt_retain`/`hulk_rt_release` detects it: in development (`debug_assertions`), a `panic!` is raised with the block's exact address; in production, a no‑op is performed instead of propagating undefined corruption.
2. **Post‑release slot nulling**: after emitting the call to `hulk_rt_release` for a local variable at scope close, `hulk-codegen` immediately emits a `store ptr null, ptr %alloca` instruction, writing `null` into the variable's slot. Any later use of that variable (due to a code‑generation bug, an incorrect merging of branches, etc.) will load `null`, and `hulk_rt_retain`/`hulk_rt_release` already treat `null` as a no‑op, turning a potential double free into an inert, attributable operation.

Together, these two defenses close the two most frequent failure cases in reference‑counted systems: freeing the same local variable twice (covered by slot nulling) and accessing a block that has been freed before the allocator recycles it (covered by header poisoning).

### 4.7 The LLVM optimization process

#### 4.7.1 Architecture and optimization levels

The compiler exposes a configurable optimization level through the `opt_level` field of `CodegenOptions`, with three variants that map to distinct pipelines of LLVM 17's pass manager:

**Table 9.** *Optimization levels of the HULK compiler*

| Variant | LLVM pipeline | Recommended use |
|---|---|---|
| `OptLevel::None` | (no IR passes) | Unit testing, IR debugging |
| `OptLevel::Less` | `"default<O1>"` | Benchmarking |
| `OptLevel::Default` | `"default<O2>"` | Production builds |
| `OptLevel::Aggressive` | `"default<O3>"` | Benchmarking |

The level also controls the code‑generation configuration of the `TargetMachine`. Instruction selection, register allocation, and instruction scheduling are all calibrated to the corresponding level consistently with the IR passes, avoiding the degenerate situation of optimized IR being fed to a `-O0` selector.

The sequence of operations inside `compile()` follows the order *verify → optimize → verify → emit*:

1. **Pre‑verification** (`module.verify()`): catches code‑generator bugs with precise messages from the LLVM verifier before they reach the optimizer. Invalid IR at the optimizer's input produces undefined behavior in the passes, not useful error messages.
2. **Optimization** (`run_passes`): applies the requested standard pipeline.
3. **Post‑verification**: some passes (`instcombine`, `GVN`) can reveal latent type inconsistencies while propagating constants or eliminating dead code. This second verification catches them before they produce incorrect machine code.
4. **Object emission** (`write_object_file`): generates the final `.o` file.

#### 4.7.2 Compatibility with the garbage collector

LLVM passes, such as `mem2reg`, which affects pointer locations, do not invalidate the GC's invariants. `mem2reg` promotes an `alloca` to an SSA register only if that `alloca`'s address never escapes — that is, if its only using instructions are `store` and `load`, with the address never appearing in any call argument or in any `getelementptr` stored elsewhere. Since every pointer‑typed `alloca` is registered with `hulk_rt_shadow_push(%alloca)`, its address escapes into that external function, and LLVM permanently classifies it as *address‑taken*, excluding it from `mem2reg`. `alloca`s of numeric type (`f64`) and boolean type (`i1`) are never registered on the shadow stack and are eligible for promotion, reducing the number of loads and stores in arithmetic.

The `instcombine` and `simplifycfg` passes operate on instruction patterns and the control‑flow graph respectively; neither can eliminate calls to external functions (which, under LLVM's memory model, carry implicit side effects). The shadow push/pop functions, retain/release, and `hulk_rt_alloc` are all external and therefore unreachable by these passes. The `inline` pass copies instructions from the callee into the caller; since push/pop pairs are emitted at the boundaries of HULK scopes — not of LLVM functions — the push/pop balance is preserved exactly after inlining [29].

### 4.8 Non‑trivial design decisions

The choice of Rust over C++ is justified by Rust's greater readability and user‑friendliness, which also comes with more up‑to‑date tooling and an ecosystem (`cargo fmt`, `cargo clippy`, and `cargo test`) that provides formatting, static analysis, and testing without configuration headaches.

An LL(1) recursive‑descent parser was chosen over an LR generator for three reasons: hand‑written LL(1) parsers produce more precise error messages because the parsing context is explicit; since it is defined through direct semantic actions, each method builds the AST node directly, with no indirection through tables; and, since it requires no external dependencies such as `flex`/`bison`, the module can be built entirely with `cargo`.

The design explicitly separates semantic analysis from the code generator. The interface between them is `VerifiedProgram`, which guarantees that `codegen` never receives a program with type errors. This separation facilitates independent testing of each phase and allows different backends (JVM, WASM, etc.) to be targeted without modifying the semantic analyzer.

The elements of a `Vector` are represented uniformly. Every element is stored as an eight‑byte pointer, boxing even the `Number` and `Boolean` values inserted into it, rather than specializing storage by element type. This is the same decision made both by Python — every list is, internally, an array of pointers to objects, with no special treatment for the integers it contains — and by Java's generic collections prior to the specialization of primitive types [11]; it deliberately departs from Go's approach, whose *slices* are densely specialized arrays by element type with no additional indirection [16]. HULK's choice prioritizes a single, simple, correct implementation of the vector runtime over the performance of cases that could, in principle, support inline storage — a decision that is explicitly revisable should future profiling evidence justify it.

As its type‑inference strategy for unannotated symbols, the analyzer synthesizes, from usage, the minimal structural protocol a parameter must satisfy, and refines it across successive passes, rather than pursuing the principal type in the sense of Hindley–Milner unification from the ML family [20]. This is closer in spirit to the structural object‑type inference employed by OCaml for its object types, or by TypeScript for its structural interfaces [13, 26], than to the search for a single most‑general type [14, 15]. This choice is consistent with the fact that HULK combines nominal typing for classes with structural typing for protocols — a combination for which, in general, no single principal type exists in the classical Hindley–Milner sense.

---

## 5. Testing

### 5.1 Adopted testing strategy

The project adopts two clearly differentiated levels of testing for `hulk-codegen`, following the same criterion already used by `hulk-semantic` for its own passes: manually constructing a typed tree fragment and asserting on the result, rather than relying exclusively on end‑to‑end tests.

The first level, located in `src/lower/mod.rs`, manually builds a minimal `Expr<Type>` — using helper functions such as `num`, `bin_op`, `if_expr`, or `let_expr` that fabricate nodes already annotated with their type — lowers it through `lower_expr`, and checks that the resulting LLVM module passes verification (`Module::verify()`), optionally inspecting the emitted IR text. This testing layer is deliberately independent of the rest of the pipeline (the lexer, parser, and semantic analyzer are not invoked) and does not require linking against `hulk-rt`; it validates that the lowering function corresponding to each AST form produces well‑formed LLVM IR.

The second level, located in `src/lib.rs`, runs the complete pipeline — lexical, syntactic, semantic, and code generation — from an actual HULK source‑code string, and checks that the resulting object file is a valid ELF file (by checking the four magic bytes `0x7f, 'E', 'L', 'F'`). A subset of eight of these tests goes one step further: it actually links the object against `libhulk_rt.a` using the system compiler (`clang` with a cross target on Windows, `cc` on Linux/WSL), runs the resulting binary, and compares its standard output — or its exit code, in the case of the trap test for a non‑exhaustive `match` — against the expected value. This is the *golden test* layer: the only one that certifies not only that the generated code is syntactically valid, but that its observable runtime behavior is correct.

### 5.2 Coverage achieved

At the time of writing this report, the complete workspace contains 237 test functions (annotated with `#[test]`), distributed according to the following table:

**Table 10.** *Distribution of automated tests by crate*

| Crate | Tests |
|---|---|
| `hulk-ast` | 4 |
| `hulk-lexer` | 16 |
| `hulk-parser` | 9 |
| `hulk-semantic` | 86 |
| `hulk-codegen` | 79 |
| `hulk-rt` | 43 |
| **Total** | **237** |

Within `hulk-codegen`, the 79 tests are split into 36 integration tests (valid ELF, in `lib.rs`) and 43 unit lowering tests (IR verification, in `lower/mod.rs`). The coverage of language constructs explicitly spans: literals and variables; `let` and variable shadowing; blocks; unary and binary operators (including concatenation with and without a space); `if`/`elif`/`else`; `while`; destructive assignment to variables and to members; calls to free functions with and without arguments, including recursion; object construction (`new`); attribute reads; method calls, including inheritance, overriding, and calls to `base`; method references without invocation (function types); `is`/`as`; `for` loops over literal vectors and over `range`; vector comprehensions; `match` with literal, type, and string patterns; dispatch through protocols; and the built‑in mathematical functions. The GC tests verify that pointer‑typed `alloca`s are not promoted by `mem2reg` after the shadow push (by inspecting the optimized IR); that field maps contain exactly the pointer offsets of the type; and that `hulk_rt_gc_collect` correctly collects binary and ternary cycles without freeing reachable objects. The `hulk-rt` module extends its tests with 25 additional cases: correctness of `retain`/`release` with the `TAG_FREED` poisoning value, absence of leaks in `hulk_rt_dynamic_vector_to_vector`, and correct cleanup by `reset_gc_state` for types with secondary sub‑allocations.

---

## 6. Discussion

It is important to be precise about the actual scope of the coverage described in Section 5. IR‑verification tests guarantee that lowering a given construct produces well‑formed LLVM IR — that is, that it passes the module verifier — but they do not, on their own, guarantee that runtime behavior is correct: an offset error in computing a struct field, for example, can produce IR that is perfectly valid as far as the verifier is concerned, yet read the wrong field at runtime. Similarly, ELF‑validity tests guarantee that the produced object has the correct binary structure, but say nothing about the behavior of the program once it is linked and executed.

The only tests that fully close that gap — the eight golden tests that link and run the resulting binary — are concentrated, understandably given the point in development at which they were written, on the most recently implemented features: vectors, ranges, protocols, `match`, and the built‑in mathematical functions. The older constructs of the language — arithmetic, control flow, object orientation with inheritance and `base`, type tests and downcasting, member assignment — are today validated only at the IR or ELF‑validity level, not through actual execution. This does not imply that these constructs are incorrect; the indirect coverage provided by more complex constructs that use them internally (for example, `for` loops depend on method calls and virtual dispatch working correctly) offers some additional confidence. But, strictly speaking, extending the golden‑test layer to the entirety of the feature corpus remains pending work, and is taken up as a concrete recommendation in Section 8.

However, the system was verified against the tests provided by the course evaluators at <https://github.com/matcom/compilers/>.

### 6.1 Alternatives considered and discarded

Two testing‑methodology alternatives were consciously discarded. The first was adopting golden tests (linking and actual execution) as the sole testing level from the very start of backend development. This was discarded because each golden test depends on a complete cross‑linking toolchain (a C compiler, the `hulk-rt` library already built as a *staticlib*), which would have made the development cycle — write a lowering function, run the test, fix — much slower during the early phases when most lowering functions did not yet exist; IR‑verification tests, since they require no linking, allow that short cycle. The second discarded alternative was using fuzzing or property‑based testing over the space of valid HULK programs to detect divergences between expected and generated behavior; this was discarded due to the implementation cost of a well‑typed HULK program generator within the timeframe of the course, and is left as a recommendation for future work in Section 8 rather than being attempted incompletely.

---

## 7. Conclusions

The complete development of a HULK compiler, from lexical analysis through native code generation for Linux x86_64, has demonstrated the feasibility of building an academic production‑quality tool that integrates the classic phases of compilation with a set of modern extensions. The implemented compiler covers the entirety of the base language, including the type system with single inheritance, structural protocols, type inference, and expression‑based control constructs. In addition, the three proposed extensions — pattern matching (`match`), first‑class functions, and vectors with comprehensions — have been integrated coherently, respecting the language's philosophy while contributing an expressiveness comparable to that of contemporary languages such as Rust [17], Kotlin [19], or Haskell [23].

The choice of Rust as the implementation language has proven especially well suited. Its type system and ownership model have made it possible to build a compiler that is safe and free of memory‑management errors, even in the most complex phases such as the five‑pass semantic analysis and LLVM code generation [7]. Parameterizing the AST with an annotation type has eased the transition between the untyped syntax tree and the fully typed tree, ensuring that each phase operates on the appropriate representation. Likewise, the use of `inkwell` as an interface to LLVM has provided fine‑grained control over code generation, making it possible to implement an efficient object model (vtables, itables) and a fully functional hybrid memory‑management system: reference counting handles the common acyclic path, while the mark‑and‑sweep collector — instrumented with a shadow stack maintained by the generated code itself and per‑type field maps embedded in the vtables — eliminates the reference cycles that pure counting cannot free [9, 28].

The addition of LLVM‑IR optimization (the `mem2reg`, `instcombine`, `simplifycfg`, and `inline` passes, applied through `run_passes`) allows high‑quality generated code to be expected. Numeric variables are promoted to SSA registers, eliminating redundant loads and stores; small methods are inlined into their devirtualized callers; and superfluous control flow is simplified. Compatibility with the GC is guaranteed structurally: pointer‑typed slots, by being registered with `hulk_rt_shadow_push`, are classified by LLVM as *address‑taken* and automatically excluded from `mem2reg`, with no need for any additional annotation or any modification to the GC [29].

The test suite, with more than two hundred automated tests, exhaustively covers the compiler's functionality and has been essential to validating the correctness of each phase. The two‑level testing strategy — IR verification and actual execution of binaries — has proven effective at catching errors both in code generation and in dynamic behavior. The "golden test" approach, which links and runs real programs, provides the highest confidence in the system's correctness, while IR‑verification tests allow for an agile development cycle.

The project has confirmed that it is possible to build a complete, extensible compiler within the framework of a university course, applying the theoretical principles of compilation [6] together with modern implementation techniques. The clear separation between the front end (analysis) and the back end (code generation) has facilitated teamwork and allowed each module to be developed and tested independently. Integration with LLVM, although complex, has provided a solid foundation for native code generation with competitive performance.

The developed HULK compiler meets the academic objectives of the Programming Languages and Compilation courses, and constitutes a solid foundation for future research and development in the field of didactic programming languages. The implemented extensions have enriched the language and demonstrated that HULK can evolve into a more expressive language without losing its pedagogical essence.

---

## 8. Limitations and Recommendations

Despite the project's success, there are limitations inherent to the current design and to its temporal scope that deserve to be pointed out — not as deficiencies, but as opportunities for future improvements and extensions.

### 8.1 Design and implementation limitations

From an architectural standpoint, the compiler has opted for pragmatic solutions that, while correct and efficient, could be refined in later versions:

1. **Limited type inference for mutual recursion.** The current inference algorithm correctly resolves simple recursion and recursion through `self`, but cannot infer types for mutually recursive functions without explicit annotations. This is a known limitation that could be addressed through a constraint‑based unification approach (Hindley–Milner style) [20] or through a more sophisticated fixed‑point analysis. Nevertheless, the current behavior is safe and predictable, since it reports an error rather than producing incorrect results.

2. **Cycle‑collector performance.** The mark‑and‑sweep collector is triggered globally every time the `ALLOC_BYTES` counter exceeds the configured threshold, which implies a pause proportional to the number of objects alive at that moment. For programs with many live objects, this pause can be noticeable. The natural evolution is to adopt a generational design [10]: most objects die young, and a generational collector would limit the set of objects examined in each cycle to the young generation, drastically reducing the average collection cost.

3. **Uniform vector storage.** The decision to store every vector element as a pointer, even for primitive types, simplifies the runtime implementation but introduces additional indirection and memory overhead. In a production context, one could consider specializing vectors for value types (similar to Java's specialized `List` types [11] or Go's `array`s [16]) in order to improve performance.

4. **Scope of the optimization process.** The current version enables LLVM's `default<O1>`, `default<O2>`, and `default<O3>` pipelines, which include method inlining and basic vectorization. However, the compiler does not perform interprocedural data‑flow analysis (e.g. dead‑code elimination across compilation units) or specialization of polymorphic functions based on concrete types (devirtualization beyond the local devirtualization the backend already performs). These techniques require the whole program to be present in memory along with a complete call graph, and constitute a natural direction for future work [29].

### 8.2 Recommendations for future work

From a research‑and‑development perspective, the HULK compiler opens up several lines of work that could be explored:

1. **Expansion of the type system.** Introducing algebraic (sum) types and richer destructuring patterns would let HULK move closer to modern functional languages. The `match` extension already takes the first steps in that direction; it could be completed with nested patterns, boolean guards, and a formal exhaustiveness checker [23].

2. **Generational collector.** The current mark‑and‑sweep collector examines every live object on each cycle. Adopting a generational strategy — reserving global collection for long‑lived objects and using a low‑latency *nursery* space for new objects — would drastically reduce pause times for programs with many short‑lived objects [10, 28].

3. **Support for incremental compilation and caching.** The current architecture compiles the entire program in a single pass. An incremental compilation system, which recompiles only the modified parts, would be beneficial for large projects and would significantly improve the interactive development experience.

4. **Code generation for other platforms.** Although the backend currently targets Linux x86_64, LLVM's portability would allow code to be generated for Windows, macOS, and ARM architectures with relatively little effort. This would make HULK usable in more diverse environments without changes to the front end or the semantic analyzer.

5. **Improving the user experience.** The current compiler produces clear, location‑aware error messages, but it could be enriched with automatic suggestions (for example, correcting typos in variable names via edit distance) and tighter integration with development environments through the Language Server Protocol (LSP).

The developed HULK compiler is a robust and extensible tool that more than fulfills the project's objectives. The limitations identified do not detract from its correctness or usefulness, and the proposed recommendations offer a clear path for its future evolution. This work constitutes a solid foundation both for teaching and for research in programming languages and compilation.

---

## 9. References

1. Wexelblat, R. L. (Ed.). (1981). *History of Programming Languages*. Academic Press / ACM.
2. Backus, J. W. (1957). The FORTRAN automatic coding system. In *Proceedings of the Western Joint Computer Conference*, 188–198.
3. McCarthy, J. (1960). Recursive functions of symbolic expressions and their computation by machine, Part I. *Communications of the ACM*, 3(4), 184–195.
4. Chomsky, N. (1956). Three models for the description of language. *IRE Transactions on Information Theory*, 2(3), 113–124.
5. Knuth, D. E. (1965). On the translation of languages from left to right. *Information and Control*, 8(6), 607–639.
6. Aho, A. V., Sethi, R., & Ullman, J. D. (1986). *Compilers: Principles, Techniques, and Tools*. Addison‑Wesley.
7. Lattner, C., & Adve, V. (2004). LLVM: A Compilation Framework for Lifelong Program Analysis & Transformation. In *Proceedings of the International Symposium on Code Generation and Optimization (CGO)*. IEEE.
8. Itanium C++ ABI Organization. *Itanium C++ ABI: Generic Application Binary Interface for the C++ Programming Language*. Collaboratively maintained reference specification; section on object layout for single inheritance.
9. Bacon, D. F., Cheng, P., & Rajan, V. T. (2004). A Unified Theory of Garbage Collection. In *Proceedings of the 19th ACM SIGPLAN Conference on Object‑Oriented Programming, Systems, Languages, and Applications (OOPSLA)*, 50–68. ACM.
10. Bacon, D. F., & Rajan, V. T. (2001). Concurrent Cycle Collection in Reference Counted Systems. In *Proceedings of the European Conference on Object‑Oriented Programming (ECOOP)*. Springer.
11. Gosling, J., Joy, B., Steele, G., Bracha, G., & Buckley, A. (2021). *The Java Language Specification, Java SE 17 Edition*. Oracle America, Inc.
12. Bierman, G. (2019). *JEP 361: Switch Expressions*. OpenJDK.
13. Leroy, X., Doligez, D., Frisch, A., Garrigue, J., Rémy, D., & Vouillon, J. (2023). *The OCaml System: Documentation and User's Manual*. Institut National de Recherche en Informatique et en Automatique (INRIA).
14. Pierce, B. C. (2002). *Types and Programming Languages*. MIT Press.
15. Cardelli, L., & Wegner, P. (1985). On Understanding Types, Data Abstraction, and Polymorphism. *ACM Computing Surveys*, 17(4), 471–523.
16. Donovan, A. A., & Kernighan, B. W. (2015). *The Go Programming Language*. Addison‑Wesley.
17. Klabnik, S., & Nichols, C. (2019). *The Rust Programming Language*. No Starch Press.
18. Odersky, M., Spoon, L., & Venners, B. (2021). *Programming in Scala* (5th ed.). Artima Press.
19. Jemerov, D., & Isakova, S. (2017). *Kotlin in Action*. Manning Publications.
20. Milner, R. (1978). A Theory of Type Polymorphism in Programming Languages. *Journal of Computer and System Sciences*, 17(3), 348–375.
21. Peyton Jones, S. L. (1987). *The Implementation of Functional Programming Languages*. Prentice Hall.
22. Hudak, P., Hughes, J., Peyton Jones, S., & Wadler, P. (2007). A History of Haskell: Being Lazy with Class. In *Proceedings of the Third ACM SIGPLAN Conference on History of Programming Languages (HOPL III)*. ACM.
23. Marlow, S. (Ed.). (2010). *Haskell 2010 Language Report*.
24. Bucher, B., & Van Rossum, G. (2020). *PEP 634 — Structural Pattern Matching: Specification*. Python Software Foundation.
25. Warsaw, B. (2000). *PEP 202 — List Comprehensions*. Python Software Foundation.
26. Bierman, G., Abadi, M., & Torgersen, M. (2014). Understanding TypeScript. In *Proceedings of the 28th European Conference on Object‑Oriented Programming (ECOOP 2014)*, 257–281. Springer.
27. Landwerth, I. (2019–2020). *Building a Compiler* [video series]. Reference repository: <https://github.com/terrajobst/minsk>.
28. Jones, R., Hosking, A., & Moss, E. (2011). *The Garbage Collection Handbook: The Art of Automatic Memory Management*. CRC Press / Chapman & Hall.
29. Allen, R., & Kennedy, K. (2001). *Optimizing Compilers for Modern Architectures: A Dependence‑based Approach*. Morgan Kaufmann.

---

## 10. Appendix: Complete Grammar

### 10.1 Conventions

The grammar that follows is the one implemented by `hulk-parser`, a hand‑written predictive descent analyzer in which every non‑terminal corresponds to a Rust method; it is not a generic table‑driven engine, which allows the semantic actions to build the abstract syntax tree directly during the descent. HULK's natural grammar is ambiguous and left‑recursive at its expression levels, so that — following the standard transformation described in Section 4 [6] — left recursion was eliminated through productions of the form `Tail -> op RHS Tail | epsilon`, common prefixes were factored out, and each production was selected using a single lookahead token (LL(1)). The *Program level* and *Expression precedence levels* sections reproduce, systematically and completely, the constructs already introduced illustratively in Sections 2 and 3; the *Extended primary expressions* sections have been reconstructed from the actual implementation of the `finish_*_expression` functions and `parse_pattern` in `hulk-parser/src/lib.rs`.

### 10.2 Program level and declarations

```text
Program      -> Declaration* Expr ';'* EOF
Declaration  -> FunctionDecl | TypeDecl | ProtocolDecl

FunctionDecl -> 'function' id FunctionTail
FunctionTail -> ParamList ReturnType? FunctionBody
ReturnType   -> ':' TypeRef
FunctionBody -> '=>' Expr ';'? | Block

TypeDecl     -> 'type' id ParamList? Parent? '{' TypeMember* '}'
Parent       -> 'inherits' id ConstructorArgs?
TypeMember   -> 'function' id FunctionTail
              | id TypeMemberAfterId
TypeMemberAfterId -> FunctionTail | TypeAnnotation? '=' Expr ';'

ProtocolDecl -> 'protocol' id ProtocolParents? '{' ProtocolMethod* '}'
```

`TypeMember` is left‑factored: after consuming an identifier, the next token decides between a method (if the next token is `(`) and an attribute (if the next token is `:` or `=`), thereby avoiding backtracking.

### 10.3 Expression precedence levels

The natural grammar of expressions is left‑recursive, so it is rewritten as a sequence of LL(1) levels, from lowest to highest precedence:

```text
Expr        -> Assignment
Assignment  -> Or AssignmentTail
AssignmentTail -> ':=' Assignment | epsilon

Or          -> And OrTail
OrTail      -> '|' And OrTail | epsilon

And         -> Equality AndTail
AndTail     -> '&' Equality AndTail | epsilon

Equality    -> Comparison EqualityTail
EqualityTail -> ('==' | '!=') Comparison EqualityTail | epsilon

Comparison  -> TypeTest ComparisonTail
ComparisonTail -> ('<' | '<=' | '>' | '>=') TypeTest ComparisonTail | epsilon

TypeTest    -> Concat TypeTestTail
TypeTestTail -> ('is' | 'as') TypeRef TypeTestTail | epsilon

Concat      -> Term ConcatTail
ConcatTail  -> ('@' | '@@') Term ConcatTail | epsilon

Term        -> Factor TermTail
TermTail    -> ('+' | '-') Factor TermTail | epsilon

Factor      -> Unary FactorTail
FactorTail  -> ('*' | '/' | '%') Unary FactorTail | epsilon

Unary       -> '-' Unary | '!' Unary | Power
Power       -> Postfix PowerTail
PowerTail   -> '^' Unary | epsilon

Postfix     -> Primary PostfixTail
PostfixTail -> '(' ArgList? ')' PostfixTail
             | '.' id PostfixTail
             | '[' Expr ']' PostfixTail
             | epsilon
```

### 10.4 Primary expressions

```text
Primary -> number
         | string
         | 'true'
         | 'false'
         | id
         | 'self'
         | 'base'
         | '(' Expr ')'
         | Block
         | Vector
         | Let
         | If
         | While
         | For
         | New
         | Match
```

Note that `base` is not, in the lexer, a reserved word in every position but rather a soft keyword (§2.4): the lexical analyzer produces it as a distinguished token, but only `Postfix` reinterprets it as a reference to the parent's method when it is immediately followed by `(`, which means a program can, in principle, use `base` as a variable name when it is not invoked as method delegation.

### 10.5 Extended primary expressions

The following productions, presented illustratively alongside each feature in Sections 2 and 3, are reconstructed here completely and systematically from the `finish_*_expression` functions and `parse_pattern` in `hulk-parser/src/lib.rs`:

```text
Block -> '{' BlockItem* '}'
BlockItem -> ';' | Expr ';'?

Let  -> 'let' LetBinding (',' LetBinding)* 'in' Expr
LetBinding -> id (':' TypeRef)? '=' Expr

If   -> 'if' '(' Expr ')' Expr ElifTail* 'else' Expr
ElifTail -> 'elif' '(' Expr ')' Expr

While -> 'while' '(' Expr ')' Expr

For  -> 'for' '(' id 'in' Expr ')' Expr

New  -> 'new' TypeRef ( '(' ArgList? ')' )?

Match -> 'match' Expr '{' MatchCase+ '}'
MatchCase -> 'case' Pattern '=>' Expr ';'?
Pattern -> '_'
         | number | string | 'true' | 'false'
         | id (':' TypeRef)?
```

In `Pattern`, an identifier not followed by `:` is interpreted as a variable pattern that captures the entire value; an identifier followed by `:` and a type reference is interpreted as a type pattern with an alias, equivalent at runtime to an `is` test followed by a local binding of the value under the given name, valid only within the body of that case.

### 10.6 Vector ambiguity

HULK uses the symbol `|` both for logical disjunction and as the separator for vector comprehensions. To preserve the LL(1) property, the parser factors the vector production and uses a special head expression that does not consume the top‑level disjunction:

```text
Vector      -> '[' VectorBody
VectorBody  -> ']'
            | ExprNoTopLevelOr VectorTail
VectorTail  -> '|' id 'in' Expr ']'
            | VectorItemsTail ']'
VectorItemsTail -> (',' Expr)*
```

This allows `[x^2 | x in range(1, 10)]` to be parsed as a comprehension, while `[(x | y) | x in values]` remains valid because the disjunction expression inside the head is explicitly delimited by parentheses. In the implementation, `ExprNoTopLevelOr` corresponds to `parse_assignment_without_or`, the same precedence level as `Assignment` but without descending through `Or`.

### 10.7 Grammar–implementation correspondence table

**Table 11.** *Grammar–implementation correspondence table*

| Non‑terminal | Rust method |
|---|---|
| `Program` | `parse_program` |
| `Declaration` | `parse_declaration` |
| `FunctionDecl` | `parse_function_declaration_after_keyword` |
| `TypeDecl` | `parse_type_declaration_after_keyword` |
| `ProtocolDecl` | `parse_protocol_declaration_after_keyword` |
| `Expr` | `parse_expression` |
| `Assignment` | `parse_assignment` |
| `Or` | `parse_or` |
| `And` | `parse_and` |
| `Equality` | `parse_equality` |
| `Comparison` | `parse_comparison` |
| `TypeTest` | `parse_type_test` |
| `Concat` | `parse_concat` |
| `Term` | `parse_term` |
| `Factor` | `parse_factor` |
| `Unary` | `parse_unary` |
| `Power` | `parse_power` |
| `Postfix` | `parse_postfix` |
| `Primary` | `parse_primary` |
| `Block` | `finish_block_expression` |
| `Vector` | `finish_vector_expression` |
| `Let` | `finish_let_expression` |
| `If` | `finish_if_expression` |
| `While` | `finish_while_expression` |
| `For` | `finish_for_expression` |
| `New` | `finish_new_expression` |
| `Match` | `finish_match_expression` |
| `Pattern` | `parse_pattern` |
