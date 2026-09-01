# hulk-parser

`hulk-parser` builds a `hulk_ast::Program` from the token stream produced by
`hulk-lexer`.

The parser is implemented as a hand-written LL(1) predictive parser. It uses one
token of lookahead, encodes expression precedence through left-recursion-free
levels, and constructs the AST directly during parsing.

See [`GRAMMAR_LL1.md`](GRAMMAR_LL1.md) for the grammar shape and the mapping
between grammar non-terminals and Rust methods.
