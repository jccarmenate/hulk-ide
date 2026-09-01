# HULK parser: LL(1) grammar used by `hulk-parser`

This crate implements a hand-written predictive LL(1) parser. The code follows
the same transformation taught in the compiler course: remove ambiguity, remove
left recursion, factor common prefixes, and then choose each production with a
single token of lookahead.

The parser is not a generic table engine; each non-terminal is represented by a
Rust method. This is the usual practical implementation of a hand-written LL(1)
parser because semantic actions can construct the AST directly.

## Program level

```ebnf
Program      -> Declaration* Expr ';'* EOF
Declaration  -> FunctionDecl | MacroDecl | TypeDecl | ProtocolDecl

FunctionDecl -> 'function' id FunctionTail
MacroDecl    -> 'def' id '(' MacroParamList ')' (':' TypeRef)?
                (FunctionBody | Block)
FunctionTail -> ParamList ReturnType? FunctionBody
ReturnType   -> ':' TypeRef
TypeRef      -> FunctionType | IterablePrefix? NamedTypeRef TypeSuffix*
FunctionType -> '(' (TypeRef (',' TypeRef)*)? ')' '->' TypeRef
IterablePrefix -> '*'
TypeSuffix   -> '[]' | '*'
FunctionBody -> ('=>' | '->') Expr ';'? | Block

TypeDecl     -> 'type' id ParamList? Parent? '{' TypeMember* '}'
Parent       -> 'inherits' id ConstructorArgs?
TypeMember   -> 'function' id FunctionTail
              | id TypeMemberAfterId
TypeMemberAfterId -> FunctionTail | TypeAnnotation? '=' Expr ';'

ProtocolDecl -> 'protocol' id ProtocolParents? '{' ProtocolMethod* '}'
```

Notice that `TypeMember` is left-factored: after seeing an identifier, the next
token chooses between a method (`(`) and an attribute (`:`, `=`).

## Expression levels

The natural expression grammar is left-recursive, so it is rewritten into a
sequence of LL(1) precedence levels. Loops in the implementation correspond to
`Tail -> op RHS Tail | epsilon` productions.

```ebnf
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
             | '(' MacroArgList ')' ('{' Block '}')? PostfixTail
             | '.' id PostfixTail
             | '[' Expr ']' PostfixTail
             | epsilon
```

`AssignmentTail`'s left-hand `Or` is reinterpreted as an `AssignTarget`
(variable, member, or index) once `:=` is seen; indexing assignment targets
are produced by the same `'[' Expr ']'` postfix used for reads, so
`matrix[i][j] := v` and `matrix[i] := new Number[3]` fall out of the existing
`Postfix`/`AssignmentTail` productions with no extra grammar.

## Primary expressions

```ebnf
Primary -> number
         | string
         | 'true'
         | 'false'
         | id
         | 'self'
         | 'base'
         | Lambda
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

## Lambda, vector and type sugar

```ebnf
Lambda      -> '(' ParamListBody ')' ReturnType? '=>' Expr
```

The parser peeks through a parenthesized prefix to distinguish `(x) => ...`
from a normal parenthesized expression. Type annotations also support function
types, vector sugar and iterable sugar:

```hulk
(Number[]) -> Boolean   // Function<Vector<Number>, Boolean>
Number[]                // Vector<Number>
*Number[]               // Iterable<Number>
Number*                 // Iterable<Number>, kept for reference compatibility
```

## Vector ambiguity resolution

HULK uses `|` both as Boolean OR and as the separator in vector comprehensions.
To keep the grammar LL(1), the parser uses a factored vector production and a
special head expression that does not consume top-level OR:

```ebnf
Vector      -> '[' VectorBody
VectorBody  -> ']'
            | ExprNoTopLevelOr VectorTail
VectorTail  -> '|' id 'in' Expr ']'
            | VectorItemsTail ']'
VectorItemsTail -> (',' Expr)*
```

This allows `[x^2 | x in range(1, 10)]` to be parsed as a comprehension, while
`[(x | y) | x in values]` remains valid because the OR expression is explicitly
parenthesized inside the head.

## `New` expressions and sized vector allocation

`new` is overloaded between plain object construction (`new Type(args)`) and
fixed-size vector allocation (`new Type[size]`, optionally with a generator
initializer). Both share the same `'new' NamedTypeRef` prefix, so they are
left-factored into one production, distinguished by whether a `[` follows the
type name:

```ebnf
New              -> 'new' NamedTypeRef NewTail
NewTail          -> NewVectorSuffix Generator?
                  | ConstructorArgs?
NewVectorSuffix  -> ('[' ']')* '[' Expr ']'
Generator        -> '{' id '->' Expr '}'
ConstructorArgs  -> '(' ArgList? ')'
```

`NewVectorSuffix` only commits to the vector-allocation branch once it sees a
`[` immediately after the type name; every empty `'[' ']'` pair wraps the
element type in one more level of `Vector<_>` (mirroring `TypeSuffix -> '[]'`
above), and the final, mandatory `'[' Expr ']'` supplies the runtime length
and terminates the suffix — a sized bracket can't be followed by another
dimension. If no `[` follows the type name at all, parsing falls through to
the pre-existing `ConstructorArgs?` branch unchanged, so `new Foo`,
`new Foo()`, and `new Foo(a, b)` are unaffected.

Examples:

```hulk
new Number[5]                  // NewVectorSuffix = '[5]', no Generator
new Number[5]{ i -> i * 2 }     // NewVectorSuffix = '[5]', Generator binds i
new Number[][3]                 // NewVectorSuffix = '[]' '[3]' -> Vector<Number>, len 3
new Point(1, 2)                  // ConstructorArgs branch, unchanged
```

Because `NewVectorSuffix`'s trailing `'[' Expr ']'` and `Postfix`'s indexing
production `'[' Expr ']'` are lexically identical, no new tokens are needed —
`NewTail` is simply consulted only in the `New` production, right after a bare
type name, where indexing postfixes cannot otherwise appear.

## Block / curly vector literal ambiguity resolution

Besides the square-bracket `Vector` production above, HULK also accepts a
curly-brace vector literal `{ e1, e2, ... }`, which is written with the same
opening token as a `Block`. Both productions parse an opening `'{'` and then
the first sub-expression before anything distinguishes them, so — exactly
like the `Vector`/`|` ambiguity above — the grammar is factored on a common
prefix and disambiguated by a single token of lookahead *after* that first
expression: `;` or `'}'` continues as a `Block`, while `,` commits to a vector
literal.

```ebnf
Block           -> '{' BlockBody
BlockBody       -> '}'
                 | Expr BlockBodyTail
BlockBodyTail   -> ',' VectorItemsTail '}'          // curly vector literal
                 | BlockStmtTail                     // ordinary block
BlockStmtTail   -> ';' BlockStmtTail
                 | Expr BlockStmtTail
                 | '}'
```

`VectorItemsTail` is the same non-terminal used by the square-bracket
`Vector` production, so `{10, 20, 30}` and `[10, 20, 30]` build the identical
`VectorExpr::Literal` AST node — the curly form is purely a surface-syntax
alternative, not a new semantic construct.

Examples:

```hulk
{10, 20, 30}          // curly vector literal (Vector::Literal), same AST as [10, 20, 30]
{ a := 1; a + 1 }      // ordinary block: first expr followed by ';'
{}                     // empty block (unchanged); use [] for an empty vector literal
```

This disambiguation is safe because a bare `,` cannot otherwise appear
directly inside a `{}` block: the only other places `,` is meaningful
(argument lists, parameter lists, `let` binding lists, `match` case lists)
are parsed by their own dedicated non-terminals, not by `Block`.

## Macros

```ebnf
MacroParamList -> MacroParam (',' MacroParam)* ','? | ε
MacroParam     -> ('*' | '@' | '$')? id (':' TypeRef)?

MacroArgList -> MacroArg (',' MacroArg)* | ε
MacroArg     -> '@' id | Expr
```

### Macro pattern matching (inside macro bodies)

Inside a macro body, the `match` keyword is parsed as a compile‑time pattern‑matching
expression, not as a runtime `MatchExpr`. The syntax is:

```ebnf
MacroMatch     -> 'match' '(' Expr ')' '{' MacroCase* '}'
MacroCase      -> 'case' '(' MacroPattern ')' '=>' Expr ';'
                | 'default' '=>' Expr ';'

The default arm is represented by the identifier default (not a keyword) and is
only recognised in the arm position. It is parsed as a wildcard pattern.

Macro patterns have a precedence structure that mirrors expression precedence, but
patterns are not expressions — they describe the shape of an AST node. The pattern 
grammar is parsed only when in_macro_body is true. In normal code, match continues 
to be parsed as a runtime MatchExpr.

MacroPattern   -> MacroOr
MacroOr        -> MacroAnd ('|' MacroAnd)*
MacroAnd       -> MacroEquality ('&' MacroEquality)*
MacroEquality  -> MacroComparison (('==' | '!=') MacroComparison)*
MacroComparison -> MacroConcat (('<' | '<=' | '>' | '>=') MacroConcat)*
MacroConcat    -> MacroTerm (('@' | '@@') MacroTerm)*
MacroTerm      -> MacroFactor (('+' | '-') MacroFactor)*
MacroFactor    -> MacroUnary (('*' | '/' | '%') MacroUnary)*
MacroUnary     -> ('-' | '!') MacroUnary | MacroPower
MacroPower     -> MacroPrimary ('^' MacroUnary)?
MacroPrimary   -> number
                | string
                | 'true'
                | 'false'
                | '_'                     // wildcard
                | id MacroPrimaryId       // binding or simple identifier
                | '(' MacroPattern ')'

MacroPrimaryId -> ':' TypeRef             // type‑annotated wildcard binding
                | ':' '(' MacroPattern ')' // compound binding: name: (pattern)
                | ε                        // simple variable binding (no type)

A MacroPrimary that starts with an identifier may be:

- x → binds the matched expression to x (wildcard shape).

- x: Number → binds to x with type constraint.

- x: (pattern) → binds the whole matched expression to x, where the inner
pattern is the shape that must be satisfied.

The wildcard _ matches any expression without binding it.



## Implementation map

| Grammar non-terminal | Rust method |
| --- | --- |
| `Program` | `parse_program` |
| `Declaration` | `parse_declaration` |
| `FunctionDecl` / `MacroDecl` | `parse_function_declaration_after_keyword` |
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
| `Lambda` | `parse_lambda_expression` |
| `TypeRef` | `parse_type_ref` |
| `Block` / `BlockBody` / curly `Vector` literal | `finish_block_expression` |
| `Vector` (square-bracket literal / comprehension) | `finish_vector_expression` |
| `New` / `NewTail` / `NewVectorSuffix` | `finish_new_expression` |
| `Generator` | `parse_new_vector_generator` |