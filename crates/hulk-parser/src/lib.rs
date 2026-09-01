//! LL(1) predictive parser for HULK.
//!
//! This crate consumes the token stream produced by `hulk-lexer` and builds the
//! semantic Abstract Syntax Tree defined in `hulk-ast`.
//!
//! The implementation is a hand-written LL(1) parser: it reads the input from
//! left to right, builds a leftmost derivation, and every grammar decision is
//! made with one token of lookahead. The usual expression grammar is transformed
//! by eliminating left recursion and encoding precedence as LL(1) levels
//! (`or`, `and`, `equality`, `comparison`, `term`, `factor`, ...). Repetition
//! tails such as `E' -> + T E' | epsilon` are implemented as loops.
//!
//! The resulting AST intentionally omits grammar-only helper nodes and keeps only
//! semantic structure.

use std::fmt;

use hulk_ast::{
    AssignExpr, AssignTarget, AttributeDecl, BinaryOp, BlockExpr, Declaration, DeclarationKind,
    DowncastExpr, ElifBranch, Expr, ExprKind, ForExpr, FunctionDecl, IfExpr, IndexExpr, LambdaExpr,
    LetBinding, LetExpr, Literal, MatchCase, MatchExpr, MemberExpr, NewExpr, Param, Pattern,
    Program, ProtocolDecl, ProtocolMethod, SourceSpan, TypeDecl, TypeMember, TypeMemberKind,
    TypeParent, TypeRef, TypeTestExpr, UnaryOp, VectorComprehension, VectorExpr, VectorGenerator, 
    WhileExpr, MacroArg, MacroCallExpr, MacroDecl, MacroParam, MacroParamKind, MacroCase, MacroMatchExpr,
    MacroPattern, MacroPatternBind,
};
use hulk_lexer::{Span, Token, TokenKind};

/// Parses a complete HULK program from a token stream using the LL(1) parser.
pub fn parse(tokens: Vec<Token>) -> Result<Program, ParseError> {
    Ll1Parser::new(tokens).parse_program()
}

/// Public name that makes the chosen parsing strategy explicit.
///
/// `Parser` is kept as a type alias for compatibility with older code that was
/// already importing `hulk_parser::Parser`.
pub type Parser = Ll1Parser;

/// Error produced by the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub kind: ParseErrorKind,
    pub span: SourceSpan,
}

impl ParseError {
    fn new(kind: ParseErrorKind, span: SourceSpan) -> Self {
        Self { kind, span }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ParseErrorKind::UnexpectedToken { expected, found } => write!(
                f,
                "parse error at line {}, col {}: expected {}, found {}",
                self.span.line, self.span.col, expected, found
            ),
            ParseErrorKind::ExpectedExpression { found } => write!(
                f,
                "parse error at line {}, col {}: expected expression, found {}",
                self.span.line, self.span.col, found
            ),
            ParseErrorKind::ExpectedIdentifier { found } => write!(
                f,
                "parse error at line {}, col {}: expected identifier, found {}",
                self.span.line, self.span.col, found
            ),
            ParseErrorKind::InvalidAssignmentTarget => write!(
                f,
                "parse error at line {}, col {}: invalid assignment target",
                self.span.line, self.span.col
            ),
            ParseErrorKind::Message(message) => write!(
                f,
                "parse error at line {}, col {}: {}",
                self.span.line, self.span.col, message
            ),
        }
    }
}

impl std::error::Error for ParseError {}

/// Specific parser error kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseErrorKind {
    UnexpectedToken { expected: String, found: String },
    ExpectedExpression { found: String },
    ExpectedIdentifier { found: String },
    InvalidAssignmentTarget,
    Message(String),
}

/// Predictive LL(1) recursive-descent parser.
///
/// The code mirrors a table-driven LL(1) parser, but instead of storing semantic
/// actions in a separate parsing table, every non-terminal has a Rust method.
/// The `check_*` helper methods below are the FIRST/FOLLOW predicates used to
/// choose the single valid production for the current lookahead token.
pub struct Ll1Parser {
    tokens: Vec<Token>,
    current: usize,
    /// Whether a macro definition body is currently being paarsed.
    /// This affects how `match` expressions are parsed.
    in_macro_body: bool,
}

impl Ll1Parser {
    /// Creates a parser. If the caller forgot to append EOF, the parser adds it.
    pub fn new(mut tokens: Vec<Token>) -> Self {
        let needs_eof = tokens
            .last()
            .map(|token| !same_variant(&token.kind, &TokenKind::Eof))
            .unwrap_or(true);

        if needs_eof {
            tokens.push(Token {
                kind: TokenKind::Eof,
                span: Span { line: 0, col: 0 },
            });
        }

        Self { tokens, current: 0, in_macro_body: false }
    }

    /// Parses a full HULK program: zero or more declarations followed by the
    /// mandatory global expression that works as entry point.
    pub fn parse_program(&mut self) -> Result<Program, ParseError> {
        let mut declarations = Vec::new();

        while self.lookahead_starts_declaration() {
            declarations.push(self.parse_declaration()?);
        }

        if !self.lookahead_starts_expression() {
            return Err(ParseError::new(
                ParseErrorKind::ExpectedExpression {
                    found: token_kind_name(&self.peek().kind),
                },
                self.peek_span(),
            ));
        }

        let entry = self.parse_expression()?;
        self.consume_optional_semicolons();
        self.consume(&TokenKind::Eof, "end of file")?;

        Ok(Program::new(declarations, entry))
    }

    fn parse_declaration(&mut self) -> Result<Declaration, ParseError> {
        let span = self.peek_span();

        match &self.peek().kind {
            TokenKind::Function => {
                self.advance();
                let function = self.parse_function_declaration_after_keyword()?;
                Ok(Declaration::new(DeclarationKind::Function(function), span))
            }
            TokenKind::Def => {
                self.advance();
                let macro_decl = self.parse_macro_declaration()?;
                Ok(Declaration::new(DeclarationKind::Macro(macro_decl), span))
            }
            TokenKind::Type => {
                self.advance();
                let type_decl = self.parse_type_declaration_after_keyword()?;
                Ok(Declaration::new(DeclarationKind::Type(type_decl), span))
            }
            TokenKind::Protocol => {
                self.advance();
                let protocol_decl = self.parse_protocol_declaration_after_keyword()?;
                Ok(Declaration::new(
                    DeclarationKind::Protocol(protocol_decl),
                    span,
                ))
            }
            _ => Err(self.error_unexpected("declaration")),
        }
    }

    fn parse_function_declaration_after_keyword(&mut self) -> Result<FunctionDecl, ParseError> {
        let name = self.consume_identifier()?;
        self.parse_function_tail(name)
    }

    fn parse_function_tail(&mut self, name: String) -> Result<FunctionDecl, ParseError> {
        let params = self.parse_param_list()?;
        let return_type = if self.match_kind(&TokenKind::Colon) {
            Some(self.parse_type_ref()?)
        } else {
            None
        };

        // WHY: HULK spec uses `=>` for function bodies, but the 'define' macro
        // extension conventionally uses `->` (same token as function-type arrows).
        // Accept both so `def f(x): T -> expr` and `function f(x): T => expr`
        // work identically.
        let body = if self.match_kind(&TokenKind::FatArrow) || self.match_kind(&TokenKind::Arrow) {
            let expression = self.parse_expression()?;
            self.match_kind(&TokenKind::Semicolon);
            expression
        } else if self.check(&TokenKind::LBrace) {
            self.parse_block_expression()?
        } else {
            return Err(self.error_unexpected("`=>`, `->`, or function block"));
        };

        Ok(FunctionDecl::new(name, params, return_type, body))
    }

    /// Parses a macro declaration after the `def` keyword has been consumed.
    fn parse_macro_declaration(&mut self) -> Result<MacroDecl, ParseError> {
        let name = self.parse_name()?;
        self.consume(&TokenKind::LParen, "`(` after macro name")?;
        let params = self.parse_macro_param_list()?;
        self.consume(&TokenKind::RParen, "`)` after macro parameters")?;

        let return_type = if self.match_kind(&TokenKind::Colon) {
            Some(self.parse_type_ref()?)
        } else {
            None
        };

        // ── Enter macro body context ──
        let old_in_macro = self.in_macro_body;
        self.in_macro_body = true;

        let body = if self.match_kind(&TokenKind::FatArrow) || self.match_kind(&TokenKind::Arrow) {
            let expr = self.parse_expression()?;
            self.match_kind(&TokenKind::Semicolon);
            expr
        } else if self.check(&TokenKind::LBrace) {
            self.parse_block_expression()?
        } else {
            return Err(ParseError::new(
                ParseErrorKind::Message(
                    "expected `=>`, `->`, or `{` for macro body".to_string(),
                ),
                self.peek_span(),
            ));
        };

        // ── Restore context ──
        self.in_macro_body = old_in_macro;

        Ok(MacroDecl::new(name, params, return_type, body))
    }

    /// Parses the parameter list of a macro declaration.
    ///
    /// Symbolic and Placeholder params may appear in any position but by convention
    /// are placed first (placeholders) or last (symbolic) for readability.
    fn parse_macro_param_list(&mut self) -> Result<Vec<MacroParam>, ParseError> {
        let mut params = Vec::new();

        if self.check(&TokenKind::RParen) {
            return Ok(params);
        }

        loop {
            let param = self.parse_macro_param()?;
            params.push(param);
            if !self.match_kind(&TokenKind::Comma) {
                break;
            }
            // Trailing comma before `)` is allowed.
            if self.check(&TokenKind::RParen) {
                break;
            }
        }
        Ok(params)
    }

    fn parse_macro_param(&mut self) -> Result<MacroParam, ParseError> {
        // Detect sigil prefix: `*`, `@`, or `$`
        let kind_prefix = if self.match_kind(&TokenKind::Star) {
            MacroParamKind::BodyExpr
        } else if self.match_kind(&TokenKind::At) {
            // `@` is the string-concat operator in expressions, but inside a macro
            // parameter list it is unambiguous as the symbolic sigil because
            // parameter positions never contain binary expressions.
            MacroParamKind::Symbolic
        } else if self.match_kind(&TokenKind::Dollar) {
            MacroParamKind::Placeholder
        } else {
            MacroParamKind::Regular
        };

        let name = self.parse_name()?;
        let type_annotation = if self.match_kind(&TokenKind::Colon) {
            Some(self.parse_type_ref()?)
        } else {
            None
        };

        Ok(MacroParam { kind: kind_prefix, name, type_annotation })
    }

    fn parse_type_declaration_after_keyword(&mut self) -> Result<TypeDecl, ParseError> {
        let name = self.consume_identifier()?;
        let params = if self.check(&TokenKind::LParen) {
            self.parse_param_list()?
        } else {
            Vec::new()
        };

        let parent = if self.match_kind(&TokenKind::Inherits) {
            let parent_name = self.consume_identifier()?;
            let args = if self.match_kind(&TokenKind::LParen) {
                self.parse_argument_list_after_lparen()?
            } else {
                Vec::new()
            };
            Some(TypeParent::new(parent_name, args))
        } else {
            None
        };

        self.consume(&TokenKind::LBrace, "`{` before type body")?;
        let mut members = Vec::new();

        while !self.check(&TokenKind::RBrace) && !self.is_at_end() {
            if self.match_kind(&TokenKind::Semicolon) {
                continue;
            }
            members.push(self.parse_type_member()?);
        }

        self.consume(&TokenKind::RBrace, "`}` after type body")?;

        Ok(TypeDecl::new(name, params, parent, members))
    }

    fn parse_type_member(&mut self) -> Result<TypeMember, ParseError> {
        let span = self.peek_span();

        if self.match_kind(&TokenKind::Function) {
            let method = self.parse_function_declaration_after_keyword()?;
            return Ok(TypeMember::new(TypeMemberKind::Method(method), span));
        }

        let name = self.parse_name()?;

        if self.check(&TokenKind::LParen) {
            let method = self.parse_function_tail(name)?;
            return Ok(TypeMember::new(TypeMemberKind::Method(method), span));
        }

        let type_annotation = if self.match_kind(&TokenKind::Colon) {
            Some(self.parse_type_ref()?)
        } else {
            None
        };

        self.consume(&TokenKind::Assign, "`=` in attribute declaration")?;
        let initializer = self.parse_expression()?;
        self.consume(&TokenKind::Semicolon, "`;` after attribute declaration")?;

        Ok(TypeMember::new(
            TypeMemberKind::Attribute(AttributeDecl::new(name, type_annotation, initializer)),
            span,
        ))
    }

    fn parse_protocol_declaration_after_keyword(&mut self) -> Result<ProtocolDecl, ParseError> {
        let name = self.consume_identifier()?;
        let mut parents = Vec::new();

        if self.match_kind(&TokenKind::Extends) {
            loop {
                parents.push(self.parse_type_ref()?);
                if !self.match_kind(&TokenKind::Comma) {
                    break;
                }
            }
        }

        self.consume(&TokenKind::LBrace, "`{` before protocol body")?;
        let mut methods = Vec::new();

        while !self.check(&TokenKind::RBrace) && !self.is_at_end() {
            if self.match_kind(&TokenKind::Semicolon) {
                continue;
            }

            let method_name = self.parse_name()?;
            let params = self.parse_param_list()?;
            self.consume(&TokenKind::Colon, "return type in protocol method")?;
            let return_type = self.parse_type_ref()?;
            self.consume(&TokenKind::Semicolon, "`;` after protocol method")?;
            methods.push(ProtocolMethod::new(method_name, params, return_type));
        }

        self.consume(&TokenKind::RBrace, "`}` after protocol body")?;

        Ok(ProtocolDecl::new(name, parents, methods))
    }

    fn parse_param_list(&mut self) -> Result<Vec<Param>, ParseError> {
        self.consume(&TokenKind::LParen, "`(` before parameter list")?;
        self.parse_param_list_after_lparen()
    }

    fn parse_param_list_after_lparen(&mut self) -> Result<Vec<Param>, ParseError> {
        let mut params = Vec::new();

        if !self.check(&TokenKind::RParen) {
            loop {
                let name = self.parse_name()?;
                let type_annotation = if self.match_kind(&TokenKind::Colon) {
                    Some(self.parse_type_ref()?)
                } else {
                    None
                };
                params.push(Param::new(name, type_annotation));

                if !self.match_kind(&TokenKind::Comma) {
                    break;
                }
            }
        }

        self.consume(&TokenKind::RParen, "`)` after parameter list")?;
        Ok(params)
    }

    fn parse_argument_list_after_lparen(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut args = Vec::new();

        if !self.check(&TokenKind::RParen) {
            loop {
                args.push(self.parse_expression()?);
                if !self.match_kind(&TokenKind::Comma) {
                    break;
                }
            }
        }

        self.consume(&TokenKind::RParen, "`)` after argument list")?;
        Ok(args)
    }

    fn parse_type_ref(&mut self) -> Result<TypeRef, ParseError> {
        if self.check(&TokenKind::LParen) {
            return self.parse_function_type_ref();
        }

        let iterable_prefix = self.match_kind(&TokenKind::Star);
        let mut ty = self.parse_named_type_ref()?;

        loop {
            if self.match_kind(&TokenKind::Star) {
                // Backwards-compatible spelling from the reference docs: `T*`.
                ty = TypeRef::with_args("Iterable", vec![ty]);
            } else if self.match_kind(&TokenKind::LBracket) {
                self.consume(&TokenKind::RBracket, "`]` after vector type suffix")?;
                ty = TypeRef::with_args("Vector", vec![ty]);
            } else {
                break;
            }
        }

        if iterable_prefix {
            // Project sugar requested by the user: `*T[]` means an iterable of T.
            // If the immediate type has just been built as `Vector<T>`, unwrap it
            // so the semantic type becomes `Iterable<T>`, not `Iterable<Vector<T>>`.
            ty = match ty {
                TypeRef { name, mut args } if name == "Vector" && args.len() == 1 => {
                    TypeRef::with_args("Iterable", vec![args.remove(0)])
                }
                other => TypeRef::with_args("Iterable", vec![other]),
            };
        }

        Ok(ty)
    }

    fn parse_named_type_ref(&mut self) -> Result<TypeRef, ParseError> {
        let name = self.consume_identifier()?;
        if self.match_kind(&TokenKind::Lt) {
            let mut args = Vec::new();
            loop {
                args.push(self.parse_type_ref()?);
                if !self.match_kind(&TokenKind::Comma) {
                    break;
                }
            }
            self.consume(&TokenKind::Gt, "`>` after type arguments")?;
            Ok(TypeRef::with_args(name, args))
        } else {
            Ok(TypeRef::named(name))
        }
    }

    fn parse_function_type_ref(&mut self) -> Result<TypeRef, ParseError> {
        self.consume(&TokenKind::LParen, "`(` before function type parameters")?;
        let mut args = Vec::new();

        if !self.check(&TokenKind::RParen) {
            loop {
                args.push(self.parse_type_ref()?);
                if !self.match_kind(&TokenKind::Comma) {
                    break;
                }
            }
        }

        self.consume(&TokenKind::RParen, "`)` after function type parameters")?;
        self.consume(&TokenKind::Arrow, "`->` in function type")?;
        args.push(self.parse_type_ref()?);

        Ok(TypeRef::with_args("Function", args))
    }

    /// Lowest-precedence expression entry point.
    fn parse_expression(&mut self) -> Result<Expr, ParseError> {
        self.parse_assignment()
    }

    fn parse_assignment(&mut self) -> Result<Expr, ParseError> {
        let expr = self.parse_or()?;

        if self.match_kind(&TokenKind::ColonEq) {
            let span = expr.span;
            let value = self.parse_assignment()?;
            let target = Self::assignment_target_from_expr(expr)?;
            return Ok(Expr::new(
                ExprKind::Assign(AssignExpr::new(target, value)),
                span,
            ));
        }

        Ok(expr)
    }

    fn parse_assignment_without_or(&mut self) -> Result<Expr, ParseError> {
        let expr = self.parse_and()?;

        if self.match_kind(&TokenKind::ColonEq) {
            let span = expr.span;
            let value = self.parse_assignment_without_or()?;
            let target = Self::assignment_target_from_expr(expr)?;
            return Ok(Expr::new(
                ExprKind::Assign(AssignExpr::new(target, value)),
                span,
            ));
        }

        Ok(expr)
    }

    fn assignment_target_from_expr(expr: Expr) -> Result<AssignTarget, ParseError> {
        let span = expr.span;
        match expr.kind {
            ExprKind::Variable(name) => Ok(AssignTarget::Variable(name)),
            ExprKind::Member(member) => Ok(AssignTarget::Member {
                object: member.object,
                field: member.member,
            }),
            ExprKind::Index(index) => Ok(AssignTarget::Index {
                object: index.object,
                index: index.index,
            }),
            _ => Err(ParseError::new(
                ParseErrorKind::InvalidAssignmentTarget,
                span,
            )),
        }
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_and()?;

        while self.match_kind(&TokenKind::Or) {
            let span = expr.span;
            let right = self.parse_and()?;
            expr = Expr::binary(BinaryOp::Or, expr, right, span);
        }

        Ok(expr)
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_equality()?;

        while self.match_kind(&TokenKind::And) {
            let span = expr.span;
            let right = self.parse_equality()?;
            expr = Expr::binary(BinaryOp::And, expr, right, span);
        }

        Ok(expr)
    }

    fn parse_equality(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_comparison()?;

        loop {
            let op = if self.match_kind(&TokenKind::EqEq) {
                Some(BinaryOp::Equal)
            } else if self.match_kind(&TokenKind::Neq) {
                Some(BinaryOp::NotEqual)
            } else {
                None
            };

            match op {
                Some(op) => {
                    let span = expr.span;
                    let right = self.parse_comparison()?;
                    expr = Expr::binary(op, expr, right, span);
                }
                None => break,
            }
        }

        Ok(expr)
    }

    fn parse_comparison(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_type_test()?;

        loop {
            let op = if self.match_kind(&TokenKind::Lt) {
                Some(BinaryOp::Less)
            } else if self.match_kind(&TokenKind::Leq) {
                Some(BinaryOp::LessEqual)
            } else if self.match_kind(&TokenKind::Gt) {
                Some(BinaryOp::Greater)
            } else if self.match_kind(&TokenKind::Geq) {
                Some(BinaryOp::GreaterEqual)
            } else {
                None
            };

            match op {
                Some(op) => {
                    let span = expr.span;
                    let right = self.parse_type_test()?;
                    expr = Expr::binary(op, expr, right, span);
                }
                None => break,
            }
        }

        Ok(expr)
    }

    fn parse_type_test(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_concat()?;

        loop {
            if self.match_kind(&TokenKind::Is) {
                let span = expr.span;
                let type_name = self.parse_type_ref()?;
                expr = Expr::new(ExprKind::TypeTest(TypeTestExpr::new(expr, type_name)), span);
            } else if self.match_kind(&TokenKind::As) {
                let span = expr.span;
                let type_name = self.parse_type_ref()?;
                expr = Expr::new(ExprKind::Downcast(DowncastExpr::new(expr, type_name)), span);
            } else {
                break;
            }
        }

        Ok(expr)
    }

    fn parse_concat(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_term()?;

        loop {
            let op = if self.match_kind(&TokenKind::At) {
                Some(BinaryOp::Concat)
            } else if self.match_kind(&TokenKind::AtAt) {
                Some(BinaryOp::ConcatSpace)
            } else {
                None
            };

            match op {
                Some(op) => {
                    let span = expr.span;
                    let right = self.parse_term()?;
                    expr = Expr::binary(op, expr, right, span);
                }
                None => break,
            }
        }

        Ok(expr)
    }

    fn parse_term(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_factor()?;

        loop {
            let op = if self.match_kind(&TokenKind::Plus) {
                Some(BinaryOp::Add)
            } else if self.match_kind(&TokenKind::Minus) {
                Some(BinaryOp::Subtract)
            } else {
                None
            };

            match op {
                Some(op) => {
                    let span = expr.span;
                    let right = self.parse_factor()?;
                    expr = Expr::binary(op, expr, right, span);
                }
                None => break,
            }
        }

        Ok(expr)
    }

    fn parse_factor(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_unary()?;

        loop {
            let op = if self.match_kind(&TokenKind::Star) {
                Some(BinaryOp::Multiply)
            } else if self.match_kind(&TokenKind::Slash) {
                Some(BinaryOp::Divide)
            } else if self.match_kind(&TokenKind::Percent) {
                Some(BinaryOp::Modulo)
            } else {
                None
            };

            match op {
                Some(op) => {
                    let span = expr.span;
                    let right = self.parse_unary()?;
                    expr = Expr::binary(op, expr, right, span);
                }
                None => break,
            }
        }

        Ok(expr)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        if self.match_kind(&TokenKind::Minus) {
            let span = self.previous_span();
            let expr = self.parse_unary()?;
            return Ok(Expr::unary(UnaryOp::Negate, expr, span));
        }

        if self.match_kind(&TokenKind::Not) {
            let span = self.previous_span();
            let expr = self.parse_unary()?;
            return Ok(Expr::unary(UnaryOp::Not, expr, span));
        }

        self.parse_power()
    }

    fn parse_power(&mut self) -> Result<Expr, ParseError> {
        let expr = self.parse_postfix()?;

        if self.match_kind(&TokenKind::Caret) {
            let span = expr.span;
            let right = self.parse_unary()?;
            return Ok(Expr::binary(BinaryOp::Power, expr, right, span));
        }

        Ok(expr)
    }

    /// Parse a postfix expression: primary, then repetitions of call, member, index.
    ///
    /// Special handling for macro invocations:
    /// - A call with a trailing `{ block }` directly after the `)` is treated as a
    ///   macro call only if the callee is a plain variable name (e.g., `foo(args) {…}`).
    /// - Any call that contains `@ident` symbolic arguments is also treated as a
    ///   macro call, regardless of a trailing block.
    ///
    /// In both macro cases we produce a `MacroCallExpr`. Otherwise we produce a normal
    /// `Call` expression, rejecting `@ident` arguments that are illegal in a function call.
    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_primary()?;

        loop {
            let span = expr.span; // span of the entire compound expression so far
            if self.match_kind(&TokenKind::LParen) {
                // Parse the argument list using the macro‑aware helper.
                let (macro_args, is_macro_style) = self.parse_call_arg_list()?;
                self.consume(&TokenKind::RParen, "`)` after call arguments")?;

                // Peek for a trailing body block `{ … }`.
                // This is only allowed when the callee is a plain variable name.
                let trailing_body = if self.check(&TokenKind::LBrace) {
                    if let ExprKind::Variable(_) = &expr.kind {
                        Some(self.parse_block_expression()?)
                    } else {
                        None
                    }
                } else {
                    None
                };

                if trailing_body.is_some() || is_macro_style {
                    // This is a macro invocation – callee must be a plain name.
                    let name = match expr.kind {
                        ExprKind::Variable(ref n) => n.clone(),
                        _ => {
                            return Err(ParseError::new(
                                ParseErrorKind::Message(
                                    "macro invocation requires a plain name as callee".to_string(),
                                ),
                                span,
                            ));
                        }
                    };
                    expr = Expr::new(
                        ExprKind::MacroCall(MacroCallExpr::new(name, macro_args, trailing_body)),
                        span,
                    );
                } else {
                    // Normal function call: convert MacroArg back to plain Expr list,
                    // rejecting any `@ident` that doesn't belong here.
                    let args: Vec<Expr> = macro_args
                        .into_iter()
                        .map(|a| match a {
                            MacroArg::Expr(e) => Ok(e),
                            _ => Err(ParseError::new(
                                ParseErrorKind::Message(format!(
                                    "Macro specific argument used outside a macro call"
                                )),
                                span,
                            )),
                        })
                        .collect::<Result<_, _>>()?;
                    expr = Expr::call(expr, args, span);
                }
            } else if self.match_kind(&TokenKind::Dot) {
                let member = self.parse_name()?;
                expr = Expr::new(ExprKind::Member(MemberExpr::new(expr, member)), span);
            } else if self.match_kind(&TokenKind::LBracket) {
                let index = self.parse_expression()?;
                self.consume(&TokenKind::RBracket, "`]` after index expression")?;
                expr = Expr::new(ExprKind::Index(IndexExpr::new(expr, index)), span);
            } else {
                break;
            }
        }

        Ok(expr)
    }

    /// Parses the argument list of a call, detecting `@ident` (symbolic argument).
    ///
    /// Returns `(args, is_macro_style)` where `is_macro_style` is `true` if any
    /// `@ident` argument was encountered. A regular `(expr, expr)` list returns
    /// `is_macro_style = false`.
    ///
    /// Note: Variable placeholders (`$ident` in the macro definition) appear at the
    /// call site as plain identifiers, so the parser treats them as ordinary expressions.
    /// The expansion pass later resolves them to placeholder parameters.
    fn parse_call_arg_list(&mut self) -> Result<(Vec<MacroArg>, bool), ParseError> {
        let mut args = Vec::new();
        let mut is_macro_style = false;

        if self.check(&TokenKind::RParen) {
            return Ok((args, false));
        }

        loop {
            let arg = if self.match_kind(&TokenKind::At) {
                // `@ident` — symbolic argument
                is_macro_style = true;
                let name = self.parse_name()?;
                MacroArg::Symbolic(name)
            } else {
                let expr = self.parse_expression()?;
                MacroArg::Expr(expr)
            };
            args.push(arg);
            if !self.match_kind(&TokenKind::Comma) {
                break;
            }
            if self.check(&TokenKind::RParen) {
                break;
            }
        }
        Ok((args, is_macro_style))
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        if !self.lookahead_starts_primary() {
            return Err(ParseError::new(
                ParseErrorKind::ExpectedExpression {
                    found: token_kind_name(&self.peek().kind),
                },
                self.peek_span(),
            ));
        }

        if self.check(&TokenKind::LParen) && self.lookahead_starts_lambda_expression() {
            return self.parse_lambda_expression();
        }

        let token = self.advance();
        let span = token_span(token.span);

        match token.kind {
            TokenKind::Number(value) => Ok(Expr::number(value, span)),
            TokenKind::StringLit(value) => Ok(Expr::string(value, span)),
            TokenKind::True => Ok(Expr::boolean(true, span)),
            TokenKind::False => Ok(Expr::boolean(false, span)),
            TokenKind::Ident(name) => Ok(Expr::variable(name, span)),
            TokenKind::SelfKw => Ok(Expr::new(ExprKind::SelfRef, span)),
            // `base` is a symbol, not a keyword — it can be shadowed by a variable 
            // (like `let base: Printer = ...`). Emit Variable("base") here;
            // parse_postfix promotes it to BaseRef only when immediately followed by `(`
            // (the method-delegation call site).
            TokenKind::Base => Ok(Expr::variable("base".to_string(), span)),
            TokenKind::LParen => {
                let expr = self.parse_expression()?;
                self.consume(&TokenKind::RParen, "`)` after expression")?;
                Ok(expr)
            }
            TokenKind::LBrace => self.finish_block_expression(span),
            TokenKind::LBracket => self.finish_vector_expression(span),
            TokenKind::Let => self.finish_let_expression(span),
            TokenKind::If => self.finish_if_expression(span),
            TokenKind::While => self.finish_while_expression(span),
            TokenKind::For => self.finish_for_expression(span),
            TokenKind::New => self.finish_new_expression(span),
            TokenKind::Match => {
                if self.in_macro_body {
                    self.parse_macro_match_expr(span)
                } else {
                    self.finish_match_expression(span)
                }
            }
            TokenKind::Function => self.parse_anon_function_after_keyword(span),
            other => Err(ParseError::new(
                ParseErrorKind::ExpectedExpression {
                    found: token_kind_name(&other),
                },
                span,
            )),
        }
    }

    fn parse_anon_function_after_keyword(&mut self, span: SourceSpan) -> Result<Expr, ParseError> {
        let params = self.parse_param_list()?;
        let return_type = if self.match_kind(&TokenKind::Colon) {
            Some(self.parse_type_ref()?)
        } else {
            None
        };
        let body = if self.match_kind(&TokenKind::FatArrow) || self.match_kind(&TokenKind::Arrow) {
            let expression = self.parse_expression()?;
            self.match_kind(&TokenKind::Semicolon);
            expression
        } else if self.check(&TokenKind::LBrace) {
            self.parse_block_expression()?
        } else {
            return Err(self.error_unexpected("`=>`, `->`, or function block"));
        };
        Ok(Expr::new(
            ExprKind::Lambda(LambdaExpr::new(params, return_type, body)),
            span,
        ))
    }

    fn parse_lambda_expression(&mut self) -> Result<Expr, ParseError> {
        let paren = self.consume(&TokenKind::LParen, "`(` before lambda parameter list")?;
        let span = token_span(paren.span);
        let params = self.parse_param_list_after_lparen()?;
        let return_type = if self.match_kind(&TokenKind::Colon) {
            Some(self.parse_type_ref()?)
        } else {
            None
        };
        self.consume(&TokenKind::FatArrow, "`=>` after lambda parameters")?;
        let body = self.parse_expression()?;

        Ok(Expr::new(
            ExprKind::Lambda(LambdaExpr::new(params, return_type, body)),
            span,
        ))
    }

    fn parse_block_expression(&mut self) -> Result<Expr, ParseError> {
        let brace = self.consume(&TokenKind::LBrace, "`{` before expression block")?;
        self.finish_block_expression(token_span(brace.span))
    }

    fn finish_block_expression(&mut self, span: SourceSpan) -> Result<Expr, ParseError> {
        // ── Empty block `{}` ──────────────────────────────────────────────
        if self.check(&TokenKind::RBrace) {
            self.advance();
            return Ok(Expr::new(
                ExprKind::Block(BlockExpr::new(Vec::new())),
                span,
            ));
        }

        // ── Parse the first expression (mandatory) ──────────────────────
        let first = self.parse_expression()?;

        // ── Disambiguate: `{ expr, ... }` -> vector literal ─────────────
        if self.check(&TokenKind::Comma) {
            let mut items = vec![first];
            // Parse comma‑separated expressions, allowing a trailing comma.
            while self.match_kind(&TokenKind::Comma) {
                if self.check(&TokenKind::RBrace) {
                    break; // trailing comma is allowed
                }
                items.push(self.parse_expression()?);
            }
            self.consume(&TokenKind::RBrace, "`}` after vector literal")?;
            return Ok(Expr::new(
                ExprKind::Vector(VectorExpr::Literal(items)),
                span,
            ));
        }

        // ── Otherwise: normal `;`‑separated block ──────────────────────
        let mut expressions = vec![first];

        // If there is a semicolon after the first expression, parse the rest.
        if self.match_kind(&TokenKind::Semicolon) {
            while !self.check(&TokenKind::RBrace) && !self.is_at_end() {
                // Skip redundant semicolons (allow empty statements).
                if self.match_kind(&TokenKind::Semicolon) {
                    continue;
                }
                expressions.push(self.parse_expression()?);
                // After an expression, we expect either a semicolon or the closing brace.
                if self.match_kind(&TokenKind::Semicolon) {
                    continue;
                }
                if !self.check(&TokenKind::RBrace) {
                    self.consume(&TokenKind::Semicolon, "`;` between block expressions")?;
                }
            }
        }
        // If there was no semicolon after the first expression, the block
        // contains only that single expression. In either case, consume the `}`.
        self.consume(&TokenKind::RBrace, "`}` after expression block")?;
        Ok(Expr::new(
            ExprKind::Block(BlockExpr::new(expressions)),
            span,
        ))
    }

    fn finish_vector_expression(&mut self, span: SourceSpan) -> Result<Expr, ParseError> {
        if self.match_kind(&TokenKind::RBracket) {
            return Ok(Expr::new(
                ExprKind::Vector(VectorExpr::Literal(Vec::new())),
                span,
            ));
        }

        let first = self.parse_assignment_without_or()?;

        if self.match_kind(&TokenKind::Or) {
            let var = self.parse_name()?;
            self.consume(&TokenKind::In, "`in` in vector comprehension")?;
            let iterable = self.parse_expression()?;
            self.consume(&TokenKind::RBracket, "`]` after vector comprehension")?;

            return Ok(Expr::new(
                ExprKind::Vector(VectorExpr::Comprehension(VectorComprehension::new(
                    first, var, iterable,
                ))),
                span,
            ));
        }

        let mut items = vec![first];
        while self.match_kind(&TokenKind::Comma) {
            if self.check(&TokenKind::RBracket) {
                break;
            }
            items.push(self.parse_expression()?);
        }

        self.consume(&TokenKind::RBracket, "`]` after vector literal")?;
        Ok(Expr::new(
            ExprKind::Vector(VectorExpr::Literal(items)),
            span,
        ))
    }

    fn finish_let_expression(&mut self, span: SourceSpan) -> Result<Expr, ParseError> {
        let mut bindings = Vec::new();

        loop {
            let name = self.parse_name()?;
            let type_annotation = if self.match_kind(&TokenKind::Colon) {
                Some(self.parse_type_ref()?)
            } else {
                None
            };

            self.consume(&TokenKind::Assign, "`=` in let binding")?;
            let initializer = self.parse_expression()?;
            bindings.push(LetBinding::new(name, type_annotation, initializer));

            if !self.match_kind(&TokenKind::Comma) {
                break;
            }
        }

        self.consume(&TokenKind::In, "`in` after let bindings")?;
        let body = self.parse_expression()?;

        Ok(Expr::new(ExprKind::Let(LetExpr::new(bindings, body)), span))
    }

    fn finish_if_expression(&mut self, span: SourceSpan) -> Result<Expr, ParseError> {
        let condition = self.parse_parenthesized_expression("if condition")?;
        let then_branch = self.parse_expression()?;
        let mut elif_branches = Vec::new();

        while self.match_kind(&TokenKind::Elif) {
            let elif_condition = self.parse_parenthesized_expression("elif condition")?;
            let body = self.parse_expression()?;
            elif_branches.push(ElifBranch::new(elif_condition, body));
        }

        self.consume(&TokenKind::Else, "`else` branch in if expression")?;
        let else_branch = self.parse_expression()?;

        Ok(Expr::new(
            ExprKind::If(IfExpr::new(
                condition,
                then_branch,
                elif_branches,
                else_branch,
            )),
            span,
        ))
    }

    fn finish_while_expression(&mut self, span: SourceSpan) -> Result<Expr, ParseError> {
        let condition = self.parse_parenthesized_expression("while condition")?;
        let body = self.parse_expression()?;

        Ok(Expr::new(
            ExprKind::While(WhileExpr::new(condition, body)),
            span,
        ))
    }

    fn finish_for_expression(&mut self, span: SourceSpan) -> Result<Expr, ParseError> {
        self.consume(&TokenKind::LParen, "`(` before for binding")?;
        let var = self.parse_name()?;
        self.consume(&TokenKind::In, "`in` inside for binding")?;
        let iterable = self.parse_expression()?;
        self.consume(&TokenKind::RParen, "`)` after for binding")?;
        let body = self.parse_expression()?;

        Ok(Expr::new(
            ExprKind::For(ForExpr::new(var, iterable, body)),
            span,
        ))
    }

    fn finish_new_expression(&mut self, span: SourceSpan) -> Result<Expr, ParseError> {
        // Parse the bare element/type name
        let mut type_name = self.parse_named_type_ref()?;

        // Look for a `new`-only vector-allocation suffix: zero or more empty
        // `[]` pairs (each adds one level of Vector<_> nesting), terminated by
        // exactly one `[<size-expr>]`.
        let mut size_expr: Option<Expr> = None;
        while self.check(&TokenKind::LBracket) {
            self.advance(); // consume '['

            if self.match_kind(&TokenKind::RBracket) {
                // `[]` — one more layer of nesting, keep scanning.
                type_name = TypeRef::with_args("Vector", vec![type_name]);
                continue;
            }

            // `[<expr>` — this is the sized bracket; it must be the last one.
            let sz = self.parse_expression()?;
            self.consume(&TokenKind::RBracket, "`]` after vector size")?;
            size_expr = Some(sz);
            break;
        }

        if let Some(size) = size_expr {
            // Vector allocation: `new ElemType[size]` (+ optional generator).
            let generator = if self.check(&TokenKind::LBrace) {
                Some(self.parse_new_vector_generator()?)
            } else {
                None
            };
            return Ok(Expr::new(
                ExprKind::New(NewExpr::new_vector(type_name, size, generator)),
                span,
            ));
        }

        // No `[...]` suffix at all -> plain-object-construction path.
        let args = if self.match_kind(&TokenKind::LParen) {
            self.parse_argument_list_after_lparen()?
        } else {
            Vec::new()
        };

        Ok(Expr::new(
            ExprKind::New(NewExpr::new(type_name, args)),
            span,
        ))
    }

    /// Parses the `{ ident -> expr }` generator attached to a sized `new`.
    fn parse_new_vector_generator(&mut self) -> Result<VectorGenerator, ParseError> {
        self.consume(&TokenKind::LBrace, "`{` before vector generator")?;
        let var = self.parse_name()?;
        self.consume(&TokenKind::Arrow, "`->` in vector generator")?;
        let body = self.parse_expression()?;
        self.consume(&TokenKind::RBrace, "`}` after vector generator")?;
        Ok(VectorGenerator::new(var, body))
    }

    fn finish_match_expression(&mut self, span: SourceSpan) -> Result<Expr, ParseError> {
        let value = self.parse_expression()?;
        self.consume(&TokenKind::LBrace, "`{` before match cases")?;
        let mut cases = Vec::new();

        while !self.check(&TokenKind::RBrace) && !self.is_at_end() {
            self.consume(&TokenKind::Case, "`case` in match expression")?;
            let pattern = self.parse_pattern()?;
            self.consume(&TokenKind::FatArrow, "`=>` after match pattern")?;
            let body = self.parse_expression()?;
            self.match_kind(&TokenKind::Semicolon);
            cases.push(MatchCase::new(pattern, body));
        }

        self.consume(&TokenKind::RBrace, "`}` after match cases")?;
        Ok(Expr::new(
            ExprKind::Match(MatchExpr::new(value, cases)),
            span,
        ))
    }

    fn parse_pattern(&mut self) -> Result<Pattern, ParseError> {
        let token = self.advance();

        match token.kind {
            TokenKind::Underscore => Ok(Pattern::Wildcard),
            TokenKind::Number(value) => Ok(Pattern::Literal(Literal::Number(value))),
            TokenKind::StringLit(value) => Ok(Pattern::Literal(Literal::String(value))),
            TokenKind::True => Ok(Pattern::Literal(Literal::Boolean(true))),
            TokenKind::False => Ok(Pattern::Literal(Literal::Boolean(false))),
            TokenKind::Ident(name) => {
                if self.match_kind(&TokenKind::Colon) {
                    let alias = Some(name);
                    let type_name = self.parse_type_ref()?;
                    Ok(Pattern::Type(type_name, alias))
                } else {
                    Ok(Pattern::Variable(name))
                }
            }
            other => Err(ParseError::new(
                ParseErrorKind::Message(format!(
                    "invalid match pattern: {}",
                    token_kind_name(&other)
                )),
                token_span(token.span),
            )),
        }
    }

    /// Parses a compile‑time `match` expression inside a macro body.
    /// Syntax: `match ( <expr> ) { case ( <pattern> ) => <expr> ; ... [default => <expr> ;] }`
    fn parse_macro_match_expr(&mut self, span: SourceSpan) -> Result<Expr, ParseError> {
        self.consume(&TokenKind::LParen, "`(` after `match`")?;
        let scrutinee = self.parse_expression()?;
        self.consume(&TokenKind::RParen, "`)` after scrutinee")?;

        self.consume(&TokenKind::LBrace, "`{` before macro match cases")?;

        let mut cases = Vec::new();
        let mut has_default = false;

        while !self.check(&TokenKind::RBrace) && !self.is_at_end() {
            // Contextual `default` arm: an identifier named "default" at arm level.
            if let TokenKind::Ident(ref name) = &self.peek().kind {
                if name == "default" && !has_default {
                    self.advance(); // consume `default`
                    has_default = true;
                    self.consume(&TokenKind::FatArrow, "`=>` after `default`")?;
                    let body = self.parse_expression()?;
                    self.match_kind(&TokenKind::Semicolon);
                    cases.push(MacroCase {
                        pattern: MacroPattern::Wildcard,
                        body,
                    });
                    continue;
                }
            }

            // Regular case arm: `case ( <pattern> ) => <expr> ;`
            self.consume(&TokenKind::Case, "`case` in macro match")?;
            self.consume(&TokenKind::LParen, "`(` before macro pattern")?;
            let pattern = self.parse_macro_pattern_or()?; // Parses a compile‑time pattern with precedence.
            self.consume(&TokenKind::RParen, "`)` after macro pattern")?;
            self.consume(&TokenKind::FatArrow, "`=>` after macro pattern")?;
            let body = self.parse_expression()?;
            self.match_kind(&TokenKind::Semicolon);

            cases.push(MacroCase { pattern, body });
        }

        self.consume(&TokenKind::RBrace, "`}` after macro match cases")?;

        let macro_match = MacroMatchExpr {
            scrutinee: Box::new(scrutinee),
            cases,
        };
        Ok(Expr::new(ExprKind::MacroMatch(macro_match), span))
    }

    // ─── Precedence levels ──────────────────────────────────────────────

    /// Parses `|` (logical or) with left associativity.
    fn parse_macro_pattern_or(&mut self) -> Result<MacroPattern, ParseError> {
        let mut left = self.parse_macro_pattern_and()?;
        while self.match_kind(&TokenKind::Or) {
            let right = self.parse_macro_pattern_and()?;
            left = MacroPattern::BinaryExpr {
                op: BinaryOp::Or,
                left: Box::new(MacroPatternBind {
                    name: None,
                    ty: None,
                    pattern: left,
                }),
                right: Box::new(MacroPatternBind {
                    name: None,
                    ty: None,
                    pattern: right,
                }),
            };
        }
        Ok(left)
    }

    /// Parses `&` (logical and) with left associativity.
    fn parse_macro_pattern_and(&mut self) -> Result<MacroPattern, ParseError> {
        let mut left = self.parse_macro_pattern_equality()?;
        while self.match_kind(&TokenKind::And) {
            let right = self.parse_macro_pattern_equality()?;
            left = MacroPattern::BinaryExpr {
                op: BinaryOp::And,
                left: Box::new(MacroPatternBind {
                    name: None,
                    ty: None,
                    pattern: left,
                }),
                right: Box::new(MacroPatternBind {
                    name: None,
                    ty: None,
                    pattern: right,
                }),
            };
        }
        Ok(left)
    }

    /// Parses `==` and `!=` with left associativity.
    fn parse_macro_pattern_equality(&mut self) -> Result<MacroPattern, ParseError> {
        let mut left = self.parse_macro_pattern_comparison()?;
        loop {
            let op = if self.match_kind(&TokenKind::EqEq) {
                Some(BinaryOp::Equal)
            } else if self.match_kind(&TokenKind::Neq) {
                Some(BinaryOp::NotEqual)
            } else {
                None
            };
            if let Some(op) = op {
                let right = self.parse_macro_pattern_comparison()?;
                left = MacroPattern::BinaryExpr {
                    op,
                    left: Box::new(MacroPatternBind {
                        name: None,
                        ty: None,
                        pattern: left,
                    }),
                    right: Box::new(MacroPatternBind {
                        name: None,
                        ty: None,
                        pattern: right,
                    }),
                };
            } else {
                break;
            }
        }
        Ok(left)
    }

    /// Parses `<`, `<=`, `>`, `>=` with left associativity.
    fn parse_macro_pattern_comparison(&mut self) -> Result<MacroPattern, ParseError> {
        let mut left = self.parse_macro_pattern_concat()?;
        loop {
            let op = if self.match_kind(&TokenKind::Lt) {
                Some(BinaryOp::Less)
            } else if self.match_kind(&TokenKind::Leq) {
                Some(BinaryOp::LessEqual)
            } else if self.match_kind(&TokenKind::Gt) {
                Some(BinaryOp::Greater)
            } else if self.match_kind(&TokenKind::Geq) {
                Some(BinaryOp::GreaterEqual)
            } else {
                None
            };
            if let Some(op) = op {
                let right = self.parse_macro_pattern_concat()?;
                left = MacroPattern::BinaryExpr {
                    op,
                    left: Box::new(MacroPatternBind {
                        name: None,
                        ty: None,
                        pattern: left,
                    }),
                    right: Box::new(MacroPatternBind {
                        name: None,
                        ty: None,
                        pattern: right,
                    }),
                };
            } else {
                break;
            }
        }
        Ok(left)
    }

    /// Parses `@` and `@@` (string concatenation) with left associativity.
    fn parse_macro_pattern_concat(&mut self) -> Result<MacroPattern, ParseError> {
        let mut left = self.parse_macro_pattern_term()?;
        loop {
            let op = if self.match_kind(&TokenKind::At) {
                Some(BinaryOp::Concat)
            } else if self.match_kind(&TokenKind::AtAt) {
                Some(BinaryOp::ConcatSpace)
            } else {
                None
            };
            if let Some(op) = op {
                let right = self.parse_macro_pattern_term()?;
                left = MacroPattern::BinaryExpr {
                    op,
                    left: Box::new(MacroPatternBind {
                        name: None,
                        ty: None,
                        pattern: left,
                    }),
                    right: Box::new(MacroPatternBind {
                        name: None,
                        ty: None,
                        pattern: right,
                    }),
                };
            } else {
                break;
            }
        }
        Ok(left)
    }

    /// Parses `+` and `-` with left associativity.
    fn parse_macro_pattern_term(&mut self) -> Result<MacroPattern, ParseError> {
        let mut left = self.parse_macro_pattern_factor()?;
        loop {
            let op = if self.match_kind(&TokenKind::Plus) {
                BinaryOp::Add
            } else if self.match_kind(&TokenKind::Minus) {
                BinaryOp::Subtract
            } else {
                break;
            };
            let right = self.parse_macro_pattern_factor()?;
            left = MacroPattern::BinaryExpr {
                op,
                left: Box::new(MacroPatternBind { name: None, ty: None, pattern: left }),
                right: Box::new(MacroPatternBind { name: None, ty: None, pattern: right }),
            };
        }
        Ok(left)
    }

    /// Parses `*`, `/`, and `%` with left associativity.
    fn parse_macro_pattern_factor(&mut self) -> Result<MacroPattern, ParseError> {
        let mut left = self.parse_macro_pattern_unary()?;
        loop {
            let op = if self.match_kind(&TokenKind::Star) {
                Some(BinaryOp::Multiply)
            } else if self.match_kind(&TokenKind::Slash) {
                Some(BinaryOp::Divide)
            } else if self.match_kind(&TokenKind::Percent) {
                Some(BinaryOp::Modulo)
            } else {
                None
            };
            if let Some(op) = op {
                let right = self.parse_macro_pattern_unary()?;
                left = MacroPattern::BinaryExpr {
                    op,
                    left: Box::new(MacroPatternBind {
                        name: None,
                        ty: None,
                        pattern: left,
                    }),
                    right: Box::new(MacroPatternBind {
                        name: None,
                        ty: None,
                        pattern: right,
                    }),
                };
            } else {
                break;
            }
        }
        Ok(left)
    }

    /// Parses unary `-` and `!` with right associativity.
    fn parse_macro_pattern_unary(&mut self) -> Result<MacroPattern, ParseError> {
        if self.match_kind(&TokenKind::Minus) {
            let operand = self.parse_macro_pattern_unary()?;
            return Ok(MacroPattern::UnaryExpr {
                op: UnaryOp::Negate,
                operand: Box::new(MacroPatternBind {
                    name: None,
                    ty: None,
                    pattern: operand,
                }),
            });
        }
        if self.match_kind(&TokenKind::Not) {
            let operand = self.parse_macro_pattern_unary()?;
            return Ok(MacroPattern::UnaryExpr {
                op: UnaryOp::Not,
                operand: Box::new(MacroPatternBind {
                    name: None,
                    ty: None,
                    pattern: operand,
                }),
            });
        }
        self.parse_macro_pattern_power()
    }

    /// Parses `^` (power) with right associativity.
    fn parse_macro_pattern_power(&mut self) -> Result<MacroPattern, ParseError> {
        let left = self.parse_macro_pattern_primary()?;
        if self.match_kind(&TokenKind::Caret) {
            let right = self.parse_macro_pattern_unary()?;
            Ok(MacroPattern::BinaryExpr {
                op: BinaryOp::Power,
                left: Box::new(MacroPatternBind {
                    name: None,
                    ty: None,
                    pattern: left,
                }),
                right: Box::new(MacroPatternBind {
                    name: None,
                    ty: None,
                    pattern: right,
                }),
            })
        } else {
            Ok(left)
        }
    }

    /// Primary pattern: literal, identifier (with optional type), or parenthesised.
    fn parse_macro_pattern_primary(&mut self) -> Result<MacroPattern, ParseError> {
        let token = self.advance();
        let span = token_span(token.span);

        match token.kind {
            TokenKind::Number(value) => Ok(MacroPattern::Literal(Literal::Number(value))),
            TokenKind::StringLit(value) => Ok(MacroPattern::Literal(Literal::String(value))),
            TokenKind::True => Ok(MacroPattern::Literal(Literal::Boolean(true))),
            TokenKind::False => Ok(MacroPattern::Literal(Literal::Boolean(false))),
            TokenKind::Underscore => Ok(MacroPattern::Wildcard),
            
            TokenKind::Ident(name) => {
                // Check for a colon after the identifier
                if self.match_kind(&TokenKind::Colon) {
                    // Distinguish between type annotation and compound binding.
                    if self.check(&TokenKind::LParen) {
                        // `name: (pattern)` – bind the compound pattern
                        let inner = self.parse_macro_pattern_or()?;
                        Ok(MacroPattern::Bind {
                            name: Some(name),
                            ty: None,
                            pattern: Box::new(inner),
                        })
                    } else {
                        // `name: Type` – type-annotated wildcard binding
                        let ty = self.parse_named_type_ref()?;
                        Ok(MacroPattern::Bind {
                            name: Some(name),
                            ty: Some(ty),
                            pattern: Box::new(MacroPattern::Wildcard),
                        })
                    }
                } else {
                    // Simple variable binding (no type annotation)
                    Ok(MacroPattern::Bind {
                        name: Some(name),
                        ty: None,
                        pattern: Box::new(MacroPattern::Wildcard),
                    })
                }
            }

            TokenKind::LParen => {
                let inner = self.parse_macro_pattern_or()?;
                self.consume(&TokenKind::RParen, "`)` after parenthesised pattern")?;
                Ok(inner)
            }

            other => Err(ParseError::new(
                ParseErrorKind::Message(format!("unexpected token in macro pattern: {}", token_kind_name(&other))),
                span,
            )),
        }
    }

    fn parse_parenthesized_expression(&mut self, context: &str) -> Result<Expr, ParseError> {
        self.consume(&TokenKind::LParen, &format!("`(` before {context}"))?;
        let expr = self.parse_expression()?;
        self.consume(&TokenKind::RParen, &format!("`)` after {context}"))?;
        Ok(expr)
    }

    fn lookahead_starts_lambda_expression(&self) -> bool {
        if !matches!(&self.peek().kind, TokenKind::LParen) {
            return false;
        }

        let Some(close_paren) = self.find_matching_paren(self.current) else {
            return false;
        };

        let mut cursor = close_paren + 1;
        if self
            .token_at(cursor)
            .map(|k| same_variant(k, &TokenKind::Colon))
            .unwrap_or(false)
        {
            cursor += 1;
            cursor = self.skip_type_ref_tokens(cursor);
        }

        self.token_at(cursor)
            .map(|kind| same_variant(kind, &TokenKind::FatArrow))
            .unwrap_or(false)
    }

    fn find_matching_paren(&self, start: usize) -> Option<usize> {
        let mut depth = 0usize;
        for index in start..self.tokens.len() {
            match &self.tokens[index].kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(index);
                    }
                }
                TokenKind::Eof => return None,
                _ => {}
            }
        }
        None
    }

    fn skip_type_ref_tokens(&self, start: usize) -> usize {
        let mut index = start;
        let mut angle_depth = 0usize;
        let mut paren_depth = 0usize;
        let mut bracket_depth = 0usize;

        while let Some(kind) = self.token_at(index) {
            match kind {
                TokenKind::FatArrow | TokenKind::Eof
                    if angle_depth == 0 && paren_depth == 0 && bracket_depth == 0 =>
                {
                    break
                }
                TokenKind::LParen => paren_depth += 1,
                TokenKind::RParen => {
                    if paren_depth == 0 && angle_depth == 0 && bracket_depth == 0 {
                        break;
                    }
                    paren_depth = paren_depth.saturating_sub(1);
                }
                TokenKind::Lt => angle_depth += 1,
                TokenKind::Gt => {
                    if angle_depth == 0 && paren_depth == 0 && bracket_depth == 0 {
                        break;
                    }
                    angle_depth = angle_depth.saturating_sub(1);
                }
                TokenKind::LBracket => bracket_depth += 1,
                TokenKind::RBracket => {
                    if bracket_depth == 0 && angle_depth == 0 && paren_depth == 0 {
                        break;
                    }
                    bracket_depth = bracket_depth.saturating_sub(1);
                }
                _ => {}
            }
            index += 1;
        }

        index
    }

    fn token_at(&self, index: usize) -> Option<&TokenKind> {
        self.tokens.get(index).map(|token| &token.kind)
    }

    /// FIRST(Declaration) = { function, def, type, protocol }.
    fn lookahead_starts_declaration(&self) -> bool {
        matches!(
            &self.peek().kind,
            TokenKind::Function | TokenKind::Def | TokenKind::Type | TokenKind::Protocol
        )
    }

    /// FIRST(Expr) for HULK's expression grammar.
    fn lookahead_starts_expression(&self) -> bool {
        self.lookahead_starts_primary()
            || matches!(&self.peek().kind, TokenKind::Minus | TokenKind::Not)
    }

    /// FIRST(Primary), after expression precedence has been factored out.
    fn lookahead_starts_primary(&self) -> bool {
        matches!(
            &self.peek().kind,
            TokenKind::Number(_)
                | TokenKind::StringLit(_)
                | TokenKind::True
                | TokenKind::False
                | TokenKind::Ident(_)
                | TokenKind::SelfKw
                | TokenKind::Base
                | TokenKind::LParen
                | TokenKind::LBrace
                | TokenKind::LBracket
                | TokenKind::Let
                | TokenKind::If
                | TokenKind::While
                | TokenKind::For
                | TokenKind::New
                | TokenKind::Match
                | TokenKind::Function
        )
    }

    fn consume_identifier(&mut self) -> Result<String, ParseError> {
        let token = self.advance();
        match token.kind {
            TokenKind::Ident(name) => Ok(name),
            other => Err(ParseError::new(
                ParseErrorKind::ExpectedIdentifier {
                    found: token_kind_name(&other),
                },
                token_span(token.span),
            )),
        }
    }

    /// Parses an identifier name, also accepting `base` as a valid identifier.
    ///
    /// WHY: HULK spec §A.7.1 describes `self` as "not a keyword, which means it
    /// can be hidden by a let expression or method argument." The spec similarly
    /// describes `base` as a "symbol", not a keyword (§A.7.4). Following the
    /// SoulNG / C# contextual keyword pattern: the lexer emits TokenKind::Base
    /// unconditionally, but the parser accepts it as a plain identifier name in
    /// all positions except method-delegation calls (`base(args)`).
    fn parse_name(&mut self) -> Result<String, ParseError> {
        let token = self.advance();
        match token.kind {
            TokenKind::Ident(name) => Ok(name),
            TokenKind::Base => Ok("base".to_string()),
            other => Err(ParseError::new(
                ParseErrorKind::ExpectedIdentifier {
                    found: token_kind_name(&other),
                },
                token_span(token.span),
            )),
        }
    }

    fn consume(&mut self, kind: &TokenKind, expected: &str) -> Result<Token, ParseError> {
        if self.check(kind) {
            Ok(self.advance())
        } else {
            Err(self.error_unexpected(expected))
        }
    }

    fn consume_optional_semicolons(&mut self) {
        while self.match_kind(&TokenKind::Semicolon) {}
    }

    fn match_kind(&mut self, kind: &TokenKind) -> bool {
        if self.check(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn check(&self, kind: &TokenKind) -> bool {
        same_variant(&self.peek().kind, kind)
    }

    fn advance(&mut self) -> Token {
        let token = self.peek().clone();
        if !self.is_at_end() {
            self.current += 1;
        }
        token
    }

    fn is_at_end(&self) -> bool {
        self.check(&TokenKind::Eof)
    }

    fn peek(&self) -> &Token {
        let last_index = self.tokens.len().saturating_sub(1);
        let index = self.current.min(last_index);
        &self.tokens[index]
    }

    fn previous_span(&self) -> SourceSpan {
        let index = self.current.saturating_sub(1);
        token_span(self.tokens[index].span)
    }

    fn peek_span(&self) -> SourceSpan {
        token_span(self.peek().span)
    }

    fn error_unexpected(&self, expected: &str) -> ParseError {
        let token = self.peek();
        ParseError::new(
            ParseErrorKind::UnexpectedToken {
                expected: expected.to_string(),
                found: token_kind_name(&token.kind),
            },
            token_span(token.span),
        )
    }
}

fn same_variant(a: &TokenKind, b: &TokenKind) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

fn token_span(span: Span) -> SourceSpan {
    SourceSpan::new(span.line, span.col)
}

fn token_kind_name(kind: &TokenKind) -> String {
    match kind {
        TokenKind::Number(value) => format!("number literal `{value}`"),
        TokenKind::StringLit(value) => format!("string literal {value:?}"),
        TokenKind::True => "`true`".to_string(),
        TokenKind::False => "`false`".to_string(),
        TokenKind::Plus => "`+`".to_string(),
        TokenKind::Minus => "`-`".to_string(),
        TokenKind::Star => "`*`".to_string(),
        TokenKind::Slash => "`/`".to_string(),
        TokenKind::Caret => "`^`".to_string(),
        TokenKind::Percent => "`%`".to_string(),
        TokenKind::At => "`@`".to_string(),
        TokenKind::AtAt => "`@@`".to_string(),
        TokenKind::EqEq => "`==`".to_string(),
        TokenKind::Neq => "`!=`".to_string(),
        TokenKind::Lt => "`<`".to_string(),
        TokenKind::Gt => "`>`".to_string(),
        TokenKind::Leq => "`<=`".to_string(),
        TokenKind::Geq => "`>=`".to_string(),
        TokenKind::And => "`&`".to_string(),
        TokenKind::Or => "`|`".to_string(),
        TokenKind::Not => "`!`".to_string(),
        TokenKind::Assign => "`=`".to_string(),
        TokenKind::ColonEq => "`:=`".to_string(),
        TokenKind::LParen => "`(`".to_string(),
        TokenKind::RParen => "`)`".to_string(),
        TokenKind::LBrace => "`{`".to_string(),
        TokenKind::RBrace => "`}`".to_string(),
        TokenKind::LBracket => "`[`".to_string(),
        TokenKind::RBracket => "`]`".to_string(),
        TokenKind::Semicolon => "`;`".to_string(),
        TokenKind::Comma => "`,`".to_string(),
        TokenKind::Colon => "`:`".to_string(),
        TokenKind::Dot => "`.`".to_string(),
        TokenKind::Arrow => "`->`".to_string(),
        TokenKind::FatArrow => "`=>`".to_string(),
        TokenKind::Let => "`let`".to_string(),
        TokenKind::In => "`in`".to_string(),
        TokenKind::If => "`if`".to_string(),
        TokenKind::Elif => "`elif`".to_string(),
        TokenKind::Else => "`else`".to_string(),
        TokenKind::While => "`while`".to_string(),
        TokenKind::For => "`for`".to_string(),
        TokenKind::Function => "`function`".to_string(),
        TokenKind::Type => "`type`".to_string(),
        TokenKind::Inherits => "`inherits`".to_string(),
        TokenKind::New => "`new`".to_string(),
        TokenKind::SelfKw => "`self`".to_string(),
        TokenKind::Base => "`base`".to_string(),
        TokenKind::Is => "`is`".to_string(),
        TokenKind::As => "`as`".to_string(),
        TokenKind::Protocol => "`protocol`".to_string(),
        TokenKind::Extends => "`extends`".to_string(),
        TokenKind::Def => "`def`".to_string(),
        TokenKind::Dollar => "`$`".to_string(),
        TokenKind::Match => "`match`".to_string(),
        TokenKind::Case => "`case`".to_string(),
        TokenKind::Underscore => "`_`".to_string(),
        TokenKind::Ident(name) => format!("identifier `{name}`"),
        TokenKind::Eof => "end of file".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hulk_lexer::Lexer;

    fn parse_source(source: &str) -> Program {
        let tokens = Lexer::new(source).tokenize().expect("valid tokens");
        parse(tokens).expect("valid parse")
    }

    fn parse_source_with_ll1_parser(source: &str) -> Program {
        let tokens = Lexer::new(source).tokenize().expect("valid tokens");
        Ll1Parser::new(tokens).parse_program().expect("valid parse")
    }

    #[test]
    fn parses_arithmetic_precedence_inside_call() {
        let program = parse_source("print(1 + 2 * 3);");

        match program.entry.kind {
            ExprKind::Call(call) => match &call.args[0].kind {
                ExprKind::Binary(binary) => {
                    assert_eq!(binary.op, BinaryOp::Add);
                    assert!(matches!(binary.right.kind, ExprKind::Binary(_)));
                }
                other => panic!("expected binary argument, got {other:?}"),
            },
            other => panic!("expected call entry, got {other:?}"),
        }
    }

    #[test]
    fn parses_function_declaration_and_entry_expression() {
        let program =
            parse_source("function tan(x: Number): Number => sin(x) / cos(x); print(tan(PI));");

        assert_eq!(program.declarations.len(), 1);
        match &program.declarations[0].kind {
            DeclarationKind::Function(function) => {
                assert_eq!(function.name, "tan");
                assert_eq!(function.params.len(), 1);
                assert_eq!(
                    function.return_type.as_ref().map(ToString::to_string),
                    Some("Number".to_string())
                );
            }
            other => panic!("expected function declaration, got {other:?}"),
        }
    }

    #[test]
    fn parses_let_expression_with_type_annotation() {
        let program = parse_source("let x: Number = 5 in x + 1;");

        match program.entry.kind {
            ExprKind::Let(let_expr) => {
                assert_eq!(let_expr.bindings[0].name, "x");
                assert_eq!(
                    let_expr.bindings[0]
                        .type_annotation
                        .as_ref()
                        .map(ToString::to_string),
                    Some("Number".to_string())
                );
                assert!(matches!(let_expr.body.kind, ExprKind::Binary(_)));
            }
            other => panic!("expected let entry, got {other:?}"),
        }
    }

    #[test]
    fn parses_type_with_attribute_and_methods() {
        let program = parse_source(
            r#"
            type A {
                value: Number = 42;
                f() => "Hello";
                g(): String => "World";
            }
            print(new A().f());
            "#,
        );

        assert_eq!(program.declarations.len(), 1);
        match &program.declarations[0].kind {
            DeclarationKind::Type(type_decl) => {
                assert_eq!(type_decl.name, "A");
                assert_eq!(type_decl.members.len(), 3);
            }
            other => panic!("expected type declaration, got {other:?}"),
        }
    }

    #[test]
    fn parses_control_flow_expressions() {
        let program = parse_source(
            r#"
            let a = 10 in while (a >= 0) {
                print(a);
                a := a - 1;
            };
            "#,
        );

        match program.entry.kind {
            ExprKind::Let(let_expr) => assert!(matches!(let_expr.body.kind, ExprKind::While(_))),
            other => panic!("expected let entry, got {other:?}"),
        }
    }

    #[test]
    fn public_ll1_parser_type_parses_programs() {
        let program = parse_source_with_ll1_parser("print(42);");
        assert!(matches!(program.entry.kind, ExprKind::Call(_)));
    }

    #[test]
    fn parses_vector_comprehension() {
        let program = parse_source("let xs = [x^2 | x in range(1, 10)] in xs[0];");

        match program.entry.kind {
            ExprKind::Let(let_expr) => {
                assert!(matches!(
                    let_expr.bindings[0].initializer.kind,
                    ExprKind::Vector(VectorExpr::Comprehension(_))
                ));
                assert!(matches!(let_expr.body.kind, ExprKind::Index(_)));
            }
            other => panic!("expected let entry, got {other:?}"),
        }
    }

    #[test]
    fn parses_or_inside_parenthesized_vector_comprehension_head() {
        let program = parse_source("let xs = [(x | y) | x in values] in xs[0];");

        match program.entry.kind {
            ExprKind::Let(let_expr) => {
                assert!(matches!(
                    let_expr.bindings[0].initializer.kind,
                    ExprKind::Vector(VectorExpr::Comprehension(_))
                ));
            }
            other => panic!("expected let entry, got {other:?}"),
        }
    }

    #[test]
    fn parses_lambda_expression() {
        let program = parse_source("let f = (x: Number): Boolean => { x % 2 == 0; } in f(4);");

        match program.entry.kind {
            ExprKind::Let(let_expr) => {
                assert!(matches!(
                    let_expr.bindings[0].initializer.kind,
                    ExprKind::Lambda(_)
                ));
            }
            other => panic!("expected let entry, got {other:?}"),
        }
    }

    #[test]
    fn parses_function_type_vector_sugar() {
        let program = parse_source("function apply(xs: Number[], f: (Number[]) -> Boolean): Boolean => f(xs); print(true);");

        match &program.declarations[0].kind {
            DeclarationKind::Function(function) => {
                assert_eq!(
                    function.params[0]
                        .type_annotation
                        .as_ref()
                        .map(ToString::to_string),
                    Some("Vector<Number>".to_string())
                );
                assert_eq!(
                    function.params[1]
                        .type_annotation
                        .as_ref()
                        .map(ToString::to_string),
                    Some("(Vector<Number>) -> Boolean".to_string())
                );
            }
            other => panic!("expected function declaration, got {other:?}"),
        }
    }

    #[test]
    fn parses_iterable_prefix_vector_sugar() {
        let program = parse_source("function sum(xs: *Number[]): Number => 0; print(sum([1]));");

        match &program.declarations[0].kind {
            DeclarationKind::Function(function) => {
                assert_eq!(
                    function.params[0]
                        .type_annotation
                        .as_ref()
                        .map(ToString::to_string),
                    Some("Iterable<Number>".to_string())
                );
            }
            other => panic!("expected function declaration, got {other:?}"),
        }
    }

    #[test]
    fn parses_def_as_global_macro_like_function() {
        // Same as before, but now tests MacroDecl.
        let program = parse_source("def twice(x: Number): Number => x * 2; print(twice(21));");
        match &program.declarations[0].kind {
            DeclarationKind::Macro(macro_decl) => {
                assert_eq!(macro_decl.name, "twice");
                assert_eq!(macro_decl.params.len(), 1);
                assert_eq!(macro_decl.params[0].name, "x");
                assert_eq!(
                    macro_decl.params[0].type_annotation.as_ref().map(ToString::to_string),
                    Some("Number".to_string())
                );
                assert_eq!(
                    macro_decl.return_type.as_ref().map(ToString::to_string),
                    Some("Number".to_string())
                );
                match &macro_decl.body.kind {
                    ExprKind::Binary(bin) => {
                        assert_eq!(bin.op, BinaryOp::Multiply);
                        assert!(matches!(&bin.left.kind, ExprKind::Variable(name) if name == "x"));
                        assert!(matches!(&bin.right.kind, ExprKind::Literal(Literal::Number(2.0))));
                    }
                    _ => panic!("macro body is not a binary multiplication"),
                }
            }
            other => panic!("expected macro declaration, got {:?}", other),
        }
        // Entry expression is print(twice(21))
        match &program.entry.kind {
            ExprKind::Call(call) => {
                assert!(matches!(&call.callee.kind, ExprKind::Variable(name) if name == "print"));
                assert_eq!(call.args.len(), 1);
                match &call.args[0].kind {
                    ExprKind::Call(inner) => {
                        assert!(matches!(&inner.callee.kind, ExprKind::Variable(name) if name == "twice"));
                        assert_eq!(inner.args.len(), 1);
                        assert!(matches!(&inner.args[0].kind, ExprKind::Literal(Literal::Number(21.0))));
                    }
                    _ => panic!("argument is not a call"),
                }
            }
            _ => panic!("entry expression is not a call"),
        }
    }

    #[test]
    fn parses_macro_declaration_with_body_param() {
        // Include a dummy entry expression to satisfy the program grammar.
        let src = "def repeat(n: Number, *expr: Object): Object => expr; print(0);";
        let program = parse_source(src);
        match &program.declarations[0].kind {
            DeclarationKind::Macro(m) => {
                assert_eq!(m.name, "repeat");
                assert_eq!(m.params.len(), 2);
                // first param: regular
                assert_eq!(m.params[0].kind, MacroParamKind::Regular);
                assert_eq!(m.params[0].name, "n");
                // second param: body expr
                assert_eq!(m.params[1].kind, MacroParamKind::BodyExpr);
                assert_eq!(m.params[1].name, "expr");
                assert_eq!(
                    m.params[1].type_annotation.as_ref().map(ToString::to_string),
                    Some("Object".to_string())
                );
                assert_eq!(
                    m.return_type.as_ref().map(ToString::to_string),
                    Some("Object".to_string())
                );
                match &m.body.kind {
                    ExprKind::Variable(name) if name == "expr" => {}
                    _ => panic!("macro body should be variable `expr`"),
                }
            }
            _ => panic!("expected macro declaration"),
        }
    }

    #[test]
    fn parses_macro_declaration_with_symbolic_and_placeholder() {
        let src = "def swap(@a: Object, @b: Object): Object => { let temp: Object = a in { a := b; b := temp; } }; print(0);";
        let program = parse_source(src);
        match &program.declarations[0].kind {
            DeclarationKind::Macro(m) => {
                assert_eq!(m.name, "swap");
                assert_eq!(m.params.len(), 2);
                // both symbolic
                for param in &m.params {
                    assert_eq!(param.kind, MacroParamKind::Symbolic);
                    assert!(param.name == "a" || param.name == "b");
                }
                // body is a block with a let expression
                match &m.body.kind {
                    ExprKind::Block(block) => {
                        assert_eq!(block.expressions.len(), 1);
                        match &block.expressions[0].kind {
                            ExprKind::Let(let_expr) => {
                                // Check that the let has one binding: temp: Object = a
                                assert_eq!(let_expr.bindings.len(), 1);
                                assert_eq!(let_expr.bindings[0].name, "temp");
                                // The type annotation should be Object
                                assert_eq!(
                                    let_expr.bindings[0].type_annotation.as_ref().map(ToString::to_string),
                                    Some("Object".to_string())
                                );
                                // The initializer is a variable "a"
                                assert!(matches!(&let_expr.bindings[0].initializer.kind, ExprKind::Variable(name) if name == "a"));
                                // The body is a block with two assignments
                                match &let_expr.body.kind {
                                    ExprKind::Block(inner) => {
                                        assert_eq!(inner.expressions.len(), 2);
                                    }
                                    _ => panic!("let body is not a block"),
                                }
                            }
                            _ => panic!("macro body's first expression is not a let"),
                        }
                    }
                    _ => panic!("macro body is not a block"),
                }
            }
            _ => panic!("expected macro declaration"),
        }
    }

   #[test]
    fn parses_macro_call_with_trailing_block() {
        let src = "{ repeat(3) { print(\"hi\"); }; print(0); }";
        let program = parse_source(src);
        match &program.entry.kind {
            ExprKind::Block(block) => {
                assert!(!block.expressions.is_empty());
                match &block.expressions[0].kind {
                    ExprKind::MacroCall(mc) => {
                        assert_eq!(mc.name, "repeat");
                        assert_eq!(mc.args.len(), 1);
                        match &mc.args[0] {
                            MacroArg::Expr(e) => {
                                assert!(matches!(&e.kind, ExprKind::Literal(Literal::Number(3.0))));
                            }
                            _ => panic!("first argument is not an expression"),
                        }
                        assert!(mc.body.is_some());
                        let body = mc.body.as_ref().unwrap();
                        match &body.kind {
                            ExprKind::Block(inner_block) => {
                                assert_eq!(inner_block.expressions.len(), 1);
                                match &inner_block.expressions[0].kind {
                                    ExprKind::Call(call) => {
                                        assert!(matches!(&call.callee.kind, ExprKind::Variable(name) if name == "print"));
                                        assert_eq!(call.args.len(), 1);
                                        assert!(matches!(&call.args[0].kind, ExprKind::Literal(Literal::String(s)) if s == "hi"));
                                    }
                                    _ => panic!("block body is not a call"),
                                }
                            }
                            _ => panic!("macro body is not a block"),
                        }
                    }
                    _ => panic!("first expression is not a macro call"),
                }
            }
            _ => panic!("entry is not a block"),
        }
    }

    #[test]
    fn parses_macro_call_with_symbolic_args() {
        let src = "{ swap(@x, @y); print(0); }";
        let program = parse_source(src);
        match &program.entry.kind {
            ExprKind::Block(block) => {
                assert!(!block.expressions.is_empty());
                match &block.expressions[0].kind {
                    ExprKind::MacroCall(mc) => {
                        assert_eq!(mc.name, "swap");
                        assert_eq!(mc.args.len(), 2);
                        match &mc.args[0] {
                            MacroArg::Symbolic(name) => assert_eq!(name, "x"),
                            _ => panic!("first arg is not symbolic"),
                        }
                        match &mc.args[1] {
                            MacroArg::Symbolic(name) => assert_eq!(name, "y"),
                            _ => panic!("second arg is not symbolic"),
                        }
                        assert!(mc.body.is_none());
                    }
                    _ => panic!("first expression is not a macro call"),
                }
            }
            _ => panic!("entry is not a block"),
        }
    }

    #[test]
    fn parses_macro_with_block_body_without_arrow() {
        // Macros can have a block body without `=>`
        let src = "def greet(name: String): String { \"Hello, \" @ name; } print(greet(\"World\"));";
        let program = parse_source(src);
        match &program.declarations[0].kind {
            DeclarationKind::Macro(m) => {
                assert_eq!(m.name, "greet");
                match &m.body.kind {
                    ExprKind::Block(block) => {
                        assert_eq!(block.expressions.len(), 1);
                    }
                    _ => panic!("macro body should be a block"),
                }
            }
            _ => panic!("expected macro declaration"),
        }
    }

    #[test]
    fn parses_macro_with_arrow_and_block_body() {
        // Syntax: def name(params): Type => { ... }
        let src = "def greet(name: String): String => { \"Hello, \" @ name; }; print(greet(\"World\"));";
        let program = parse_source(src);
        match &program.declarations[0].kind {
            DeclarationKind::Macro(m) => {
                assert_eq!(m.name, "greet");
                match &m.body.kind {
                    ExprKind::Block(block) => {
                        assert_eq!(block.expressions.len(), 1);
                    }
                    _ => panic!("macro body should be a block"),
                }
            }
            _ => panic!("expected macro declaration"),
        }
    }

    #[test]
    fn rejects_invalid_assignment_target() {
        let tokens = Lexer::new("(1 + 2) := 3;")
            .tokenize()
            .expect("valid tokens");
        let error = parse(tokens).expect_err("invalid assignment target");
        assert_eq!(error.kind, ParseErrorKind::InvalidAssignmentTarget);
    }
}
