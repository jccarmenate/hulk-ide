//! Abstract Syntax Tree (AST) for the HULK programming language.
//!
//! The parser produces these nodes and the following compiler phases
//! (semantic analysis, type checking and code generation) consume them.
//!
//! Design rule: the AST keeps semantic information. Parentheses, separators
//! and grammar-only helper productions are intentionally omitted.

use std::fmt;

/// Source location used by AST nodes for diagnostics.
///
/// `line` and `col` are 1-based, matching the lexer spans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourceSpan {
    pub line: usize,
    pub col: usize,
}

impl SourceSpan {
    pub const fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

/// Complete HULK program.
///
/// HULK programs are a sequence of declarations followed by one global
/// expression that acts as the entry point.
#[derive(Debug, Clone, PartialEq)]
pub struct Program<A = ()> {
    pub declarations: Vec<Declaration<A>>,
    pub entry: Expr<A>,
}

impl<A> Program<A> {
    pub fn new(declarations: Vec<Declaration<A>>, entry: Expr<A>) -> Self {
        Self {
            declarations,
            entry,
        }
    }
}

/// A top-level declaration with its source span.
#[derive(Debug, Clone, PartialEq)]
pub struct Declaration<A = ()> {
    pub kind: DeclarationKind<A>,
    pub span: SourceSpan,
}

impl<A> Declaration<A> {
    pub fn new(kind: DeclarationKind<A>, span: SourceSpan) -> Self {
        Self { kind, span }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeclarationKind<A = ()> {
    Function(FunctionDecl<A>),
    Type(TypeDecl<A>),
    Protocol(ProtocolDecl), // protocols have no expression bodies
    Macro(MacroDecl<A>),
}

/// Function declaration, either inline (`=> expr`) or full-form (`{ ... }`).
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionDecl<A = ()> {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Option<TypeRef>,
    pub body: Expr<A>,
}

impl<A> FunctionDecl<A> {
    pub fn new(
        name: impl Into<String>,
        params: Vec<Param>,
        return_type: Option<TypeRef>,
        body: Expr<A>,
    ) -> Self {
        Self {
            name: name.into(),
            params,
            return_type,
            body,
        }
    }
}

/// Type declaration: `type T(args) inherits Base(args) { ... }`.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeDecl<A = ()> {
    pub name: String,
    pub params: Vec<Param>,
    pub parent: Option<TypeParent<A>>,
    pub members: Vec<TypeMember<A>>,
}

impl<A> TypeDecl<A> {
    pub fn new(
        name: impl Into<String>,
        params: Vec<Param>,
        parent: Option<TypeParent<A>>,
        members: Vec<TypeMember<A>>,
    ) -> Self {
        Self {
            name: name.into(),
            params,
            parent,
            members,
        }
    }
}

/// Parent type and constructor arguments used by inheritance.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeParent<A = ()> {
    pub name: String,
    pub args: Vec<Expr<A>>,
}

impl<A> TypeParent<A> {
    pub fn new(name: impl Into<String>, args: Vec<Expr<A>>) -> Self {
        Self {
            name: name.into(),
            args,
        }
    }
}

/// Member of a HULK type.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeMember<A = ()> {
    pub kind: TypeMemberKind<A>,
    pub span: SourceSpan,
}

impl<A> TypeMember<A> {
    pub fn new(kind: TypeMemberKind<A>, span: SourceSpan) -> Self {
        Self { kind, span }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeMemberKind<A = ()> {
    Attribute(AttributeDecl<A>),
    Method(FunctionDecl<A>),
}

/// Attribute declaration inside a type body.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributeDecl<A = ()> {
    pub name: String,
    pub type_annotation: Option<TypeRef>,
    pub initializer: Expr<A>,
}

impl<A> AttributeDecl<A> {
    pub fn new(
        name: impl Into<String>,
        type_annotation: Option<TypeRef>,
        initializer: Expr<A>,
    ) -> Self {
        Self {
            name: name.into(),
            type_annotation,
            initializer,
        }
    }
}

/// Protocol declaration, useful for the natural next HULK layer.
/// Keeping it in the AST avoids a redesign when protocols are implemented.
#[derive(Debug, Clone, PartialEq)]
pub struct ProtocolDecl {
    pub name: String,
    pub parents: Vec<TypeRef>,
    pub methods: Vec<ProtocolMethod>,
}

impl ProtocolDecl {
    pub fn new(
        name: impl Into<String>,
        parents: Vec<TypeRef>,
        methods: Vec<ProtocolMethod>,
    ) -> Self {
        Self {
            name: name.into(),
            parents,
            methods,
        }
    }
}

/// Method signature inside a protocol.
#[derive(Debug, Clone, PartialEq)]
pub struct ProtocolMethod {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: TypeRef,
}

impl ProtocolMethod {
    pub fn new(name: impl Into<String>, params: Vec<Param>, return_type: TypeRef) -> Self {
        Self {
            name: name.into(),
            params,
            return_type,
        }
    }
}

/// Function, method, type, or protocol parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub type_annotation: Option<TypeRef>,
}

impl Param {
    pub fn new(name: impl Into<String>, type_annotation: Option<TypeRef>) -> Self {
        Self {
            name: name.into(),
            type_annotation,
        }
    }
}

/// A reference to a type name as written in source code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeRef {
    pub name: String,
    pub args: Vec<TypeRef>,
}

impl TypeRef {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            args: Vec::new(),
        }
    }

    pub fn with_args(name: impl Into<String>, args: Vec<TypeRef>) -> Self {
        Self {
            name: name.into(),
            args,
        }
    }
}

impl fmt::Display for TypeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.name == "Function" && !self.args.is_empty() {
            let return_type = self.args.last().expect("function type has return type");
            let params = self.args[..self.args.len() - 1]
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            write!(f, "({}) -> {}", params, return_type)
        } else if self.args.is_empty() {
            write!(f, "{}", self.name)
        } else {
            let args = self
                .args
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            write!(f, "{}<{}>", self.name, args)
        }
    }
}

/// Expression node.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr<A = ()> {
    pub kind: ExprKind<A>,
    pub anno: A,
    pub span: SourceSpan,
}

impl Expr {
    /// Creates a new expression node with `anno = ()`.
    pub fn new(kind: ExprKind, span: SourceSpan) -> Self {
        Self {
            kind,
            anno: (),
            span,
        }
    }

    pub fn literal(literal: Literal, span: SourceSpan) -> Self {
        Self::new(ExprKind::Literal(literal), span)
    }

    pub fn number(value: f64, span: SourceSpan) -> Self {
        Self::literal(Literal::Number(value), span)
    }

    pub fn string(value: impl Into<String>, span: SourceSpan) -> Self {
        Self::literal(Literal::String(value.into()), span)
    }

    pub fn boolean(value: bool, span: SourceSpan) -> Self {
        Self::literal(Literal::Boolean(value), span)
    }

    pub fn variable(name: impl Into<String>, span: SourceSpan) -> Self {
        Self::new(ExprKind::Variable(name.into()), span)
    }

    pub fn unary(op: UnaryOp, expr: Self, span: SourceSpan) -> Self {
        Self::new(
            ExprKind::Unary(UnaryExpr {
                op,
                expr: Box::new(expr),
            }),
            span,
        )
    }

    pub fn binary(op: BinaryOp, left: Self, right: Self, span: SourceSpan) -> Self {
        Self::new(
            ExprKind::Binary(BinaryExpr {
                op,
                left: Box::new(left),
                right: Box::new(right),
            }),
            span,
        )
    }

    pub fn call(callee: Self, args: Vec<Self>, span: SourceSpan) -> Self {
        Self::new(
            ExprKind::Call(CallExpr {
                callee: Box::new(callee),
                args,
            }),
            span,
        )
    }
}

/// Every expression form supported by the AST.
///
/// HULK is expression-based, so assignment, loops and blocks also appear here.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind<A = ()> {
    Literal(Literal),
    Variable(String),
    SelfRef,
    BaseRef,
    Unary(UnaryExpr<A>),
    Binary(BinaryExpr<A>),
    Let(LetExpr<A>),
    Assign(AssignExpr<A>),
    Block(BlockExpr<A>),
    If(IfExpr<A>),
    While(WhileExpr<A>),
    For(ForExpr<A>),
    Call(CallExpr<A>),
    Lambda(LambdaExpr<A>),
    Member(MemberExpr<A>),
    New(NewExpr<A>),
    TypeTest(TypeTestExpr<A>),
    Downcast(DowncastExpr<A>),
    Vector(VectorExpr<A>),
    Index(IndexExpr<A>),
    /// Extra-feature friendly node for `match` expressions.
    Match(MatchExpr<A>),
    /// Macro invocation site — replaced by the expander before semantic analysis.
    MacroCall(MacroCallExpr<A>),
    /// Compile‑time pattern matching, used only inside macro definitions.
    MacroMatch(MacroMatchExpr<A>),
}

/// Literal values.
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Number(f64),
    String(String),
    Boolean(bool),
}

/// Unary expression.
#[derive(Debug, Clone, PartialEq)]
pub struct UnaryExpr<A = ()> {
    pub op: UnaryOp,
    pub expr: Box<Expr<A>>,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Negate,
    Not,
}

/// Binary expression.
#[derive(Debug, Clone, PartialEq)]
pub struct BinaryExpr<A = ()> {
    pub op: BinaryOp,
    pub left: Box<Expr<A>>,
    pub right: Box<Expr<A>>,
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Or,
    And,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
    Power,
    Concat,
    ConcatSpace,
}

/// `let a = expr, b: T = expr in body`.
#[derive(Debug, Clone, PartialEq)]
pub struct LetExpr<A = ()> {
    pub bindings: Vec<LetBinding<A>>,
    pub body: Box<Expr<A>>,
}

impl<A> LetExpr<A> {
    pub fn new(bindings: Vec<LetBinding<A>>, body: Expr<A>) -> Self {
        Self {
            bindings,
            body: Box::new(body),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LetBinding<A = ()> {
    pub name: String,
    pub type_annotation: Option<TypeRef>,
    pub initializer: Expr<A>,
}

impl<A> LetBinding<A> {
    pub fn new(
        name: impl Into<String>,
        type_annotation: Option<TypeRef>,
        initializer: Expr<A>,
    ) -> Self {
        Self {
            name: name.into(),
            type_annotation,
            initializer,
        }
    }
}

/// Destructive assignment: `target := value`.
#[derive(Debug, Clone, PartialEq)]
pub struct AssignExpr<A = ()> {
    pub target: AssignTarget<A>,
    pub value: Box<Expr<A>>,
}

impl<A> AssignExpr<A> {
    pub fn new(target: AssignTarget<A>, value: Expr<A>) -> Self {
        Self {
            target,
            value: Box::new(value),
        }
    }
}

/// Valid left-hand sides for destructive assignment.
#[derive(Debug, Clone, PartialEq)]
pub enum AssignTarget<A = ()> {
    Variable(String),
    Member {
        object: Box<Expr<A>>,
        field: String,
    },
    Index {
        object: Box<Expr<A>>,
        index: Box<Expr<A>>,
    },
}

/// Expression block: `{ expr1; expr2; ... }`.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockExpr<A = ()> {
    pub expressions: Vec<Expr<A>>,
}

impl<A> BlockExpr<A> {
    pub fn new(expressions: Vec<Expr<A>>) -> Self {
        Self { expressions }
    }
}

/// Conditional expression with optional `elif` branches and final `else`.
#[derive(Debug, Clone, PartialEq)]
pub struct IfExpr<A = ()> {
    pub condition: Box<Expr<A>>,
    pub then_branch: Box<Expr<A>>,
    pub elif_branches: Vec<ElifBranch<A>>,
    pub else_branch: Box<Expr<A>>,
}

impl<A> IfExpr<A> {
    pub fn new(
        condition: Expr<A>,
        then_branch: Expr<A>,
        elif_branches: Vec<ElifBranch<A>>,
        else_branch: Expr<A>,
    ) -> Self {
        Self {
            condition: Box::new(condition),
            then_branch: Box::new(then_branch),
            elif_branches,
            else_branch: Box::new(else_branch),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ElifBranch<A = ()> {
    pub condition: Expr<A>,
    pub body: Expr<A>,
}

impl<A> ElifBranch<A> {
    pub fn new(condition: Expr<A>, body: Expr<A>) -> Self {
        Self { condition, body }
    }
}

/// `while (condition) body`.
#[derive(Debug, Clone, PartialEq)]
pub struct WhileExpr<A = ()> {
    pub condition: Box<Expr<A>>,
    pub body: Box<Expr<A>>,
}

impl<A> WhileExpr<A> {
    pub fn new(condition: Expr<A>, body: Expr<A>) -> Self {
        Self {
            condition: Box::new(condition),
            body: Box::new(body),
        }
    }
}

/// `for (var in iterable) body`.
#[derive(Debug, Clone, PartialEq)]
pub struct ForExpr<A = ()> {
    pub var: String,
    pub iterable: Box<Expr<A>>,
    pub body: Box<Expr<A>>,
}

impl<A> ForExpr<A> {
    pub fn new(var: impl Into<String>, iterable: Expr<A>, body: Expr<A>) -> Self {
        Self {
            var: var.into(),
            iterable: Box::new(iterable),
            body: Box::new(body),
        }
    }
}

/// Function call or method call.
///
/// The callee is an expression so the same node supports both `f(x)` and
/// `obj.f(x)` after dot access has been parsed as a [`MemberExpr`].
#[derive(Debug, Clone, PartialEq)]
pub struct CallExpr<A = ()> {
    pub callee: Box<Expr<A>>,
    pub args: Vec<Expr<A>>,
}

/// Anonymous function expression: `(x: T, y) => body`.
///
/// The body is still a HULK expression, so both inline expression bodies and
/// block bodies are represented without needing a separate return statement.
#[derive(Debug, Clone, PartialEq)]
pub struct LambdaExpr<A = ()> {
    pub params: Vec<Param>,
    pub return_type: Option<TypeRef>,
    pub body: Box<Expr<A>>,
}

impl<A> LambdaExpr<A> {
    pub fn new(params: Vec<Param>, return_type: Option<TypeRef>, body: Expr<A>) -> Self {
        Self {
            params,
            return_type,
            body: Box::new(body),
        }
    }
}

/// Dot access: `object.member`.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberExpr<A = ()> {
    pub object: Box<Expr<A>>,
    pub member: String,
}

impl<A> MemberExpr<A> {
    pub fn new(object: Expr<A>, member: impl Into<String>) -> Self {
        Self {
            object: Box::new(object),
            member: member.into(),
        }
    }
}


/// Object construction (`new Type(args)`) or fixed-size vector allocation
/// (`new Type[size]` / `new Type[][size]` / `new Type[size]{ i -> expr }`).
#[derive(Debug, Clone, PartialEq)]
pub struct NewExpr<A = ()> {
    pub type_name: TypeRef,
    pub args: Vec<Expr<A>>,
    /// Present only for vector-allocation form: the requested length.
    pub size: Option<Box<Expr<A>>>,
    /// Present only when a `{ i -> expr }` generator follows a sized `new`.
    pub generator: Option<VectorGenerator<A>>,
}

impl<A> NewExpr<A> {
    /// Plain object construction: `new Type(args)`.
    pub fn new(type_name: TypeRef, args: Vec<Expr<A>>) -> Self {
        Self { type_name, args, size: None, generator: None }
    }

    /// Vector allocation: `new ElemType[size]` with an optional generator.
    pub fn new_vector(
        elem_type: TypeRef,
        size: Expr<A>,
        generator: Option<VectorGenerator<A>>,
    ) -> Self {
        Self {
            type_name: elem_type,
            args: Vec::new(),
            size: Some(Box::new(size)),
            generator,
        }
    }
}

/// The `{ i -> expr }` initializer attached to a sized `new T[n]`.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorGenerator<A = ()> {
    pub var: String,
    pub body: Box<Expr<A>>,
}

impl<A> VectorGenerator<A> {
    pub fn new(var: impl Into<String>, body: Expr<A>) -> Self {
        Self { var: var.into(), body: Box::new(body) }
    }
}

/// Dynamic type test: `expr is Type`.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeTestExpr<A = ()> {
    pub expr: Box<Expr<A>>,
    pub type_name: TypeRef,
}

impl<A> TypeTestExpr<A> {
    pub fn new(expr: Expr<A>, type_name: TypeRef) -> Self {
        Self {
            expr: Box::new(expr),
            type_name,
        }
    }
}

/// Downcast: `expr as Type`.
#[derive(Debug, Clone, PartialEq)]
pub struct DowncastExpr<A = ()> {
    pub expr: Box<Expr<A>>,
    pub type_name: TypeRef,
}

impl<A> DowncastExpr<A> {
    pub fn new(expr: Expr<A>, type_name: TypeRef) -> Self {
        Self {
            expr: Box::new(expr),
            type_name,
        }
    }
}

/// Vector literal or vector comprehension.
#[derive(Debug, Clone, PartialEq)]
pub enum VectorExpr<A = ()> {
    Literal(Vec<Expr<A>>),
    Comprehension(VectorComprehension<A>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct VectorComprehension<A = ()> {
    pub expr: Box<Expr<A>>,
    pub var: String,
    pub iterable: Box<Expr<A>>,
}

impl<A> VectorComprehension<A> {
    pub fn new(expr: Expr<A>, var: impl Into<String>, iterable: Expr<A>) -> Self {
        Self {
            expr: Box::new(expr),
            var: var.into(),
            iterable: Box::new(iterable),
        }
    }
}

/// Indexing expression: `object[index]`.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexExpr<A = ()> {
    pub object: Box<Expr<A>>,
    pub index: Box<Expr<A>>,
}

impl<A> IndexExpr<A> {
    pub fn new(object: Expr<A>, index: Expr<A>) -> Self {
        Self {
            object: Box::new(object),
            index: Box::new(index),
        }
    }
}

/// Pattern matching expression for the planned extension.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchExpr<A = ()> {
    pub value: Box<Expr<A>>,
    pub cases: Vec<MatchCase<A>>,
}

impl<A> MatchExpr<A> {
    pub fn new(value: Expr<A>, cases: Vec<MatchCase<A>>) -> Self {
        Self {
            value: Box::new(value),
            cases,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchCase<A = ()> {
    pub pattern: Pattern,
    pub body: Expr<A>,
}

impl<A> MatchCase<A> {
    pub fn new(pattern: Pattern, body: Expr<A>) -> Self {
        Self { pattern, body }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Wildcard,
    Literal(Literal),
    Variable(String),
    Type(TypeRef, Option<String>),
}


// =============================================================================
// Macro declarations
// =============================================================================

/// Discriminates the four kinds of macro parameter.
///
/// Each kind maps to a different syntactic sigil in the source:
///
/// | Kind          | Syntax      | Semantics                                     |
/// |---------------|-------------|-----------------------------------------------|
/// | Regular       | `n: T`      | Expression argument — evaluated, substituted. |
/// | BodyExpr      | `*expr: T`  | Trailing `{ block }` after the invocation.    |
/// | Symbolic      | `@x: T`     | An lvalue passed by name, not by value.       |
/// | Placeholder   | `$iter: T`  | A new variable name introduced at call site.  |
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MacroParamKind {
    Regular,
    BodyExpr,
    Symbolic,
    Placeholder,
}

/// A single parameter of a macro declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct MacroParam {
    pub kind: MacroParamKind,
    pub name: String,
    pub type_annotation: Option<TypeRef>,
}

impl MacroParam {
    pub fn regular(name: impl Into<String>, type_annotation: Option<TypeRef>) -> Self {
        Self { kind: MacroParamKind::Regular, name: name.into(), type_annotation }
    }
    pub fn body_expr(name: impl Into<String>, type_annotation: Option<TypeRef>) -> Self {
        Self { kind: MacroParamKind::BodyExpr, name: name.into(), type_annotation }
    }
    pub fn symbolic(name: impl Into<String>, type_annotation: Option<TypeRef>) -> Self {
        Self { kind: MacroParamKind::Symbolic, name: name.into(), type_annotation }
    }
    pub fn placeholder(name: impl Into<String>, type_annotation: Option<TypeRef>) -> Self {
        Self { kind: MacroParamKind::Placeholder, name: name.into(), type_annotation }
    }
}

/// Top-level macro declaration: `def name(params...) { body }`.
///
/// Template that will be textually expanded at every call site.
#[derive(Debug, Clone, PartialEq)]
pub struct MacroDecl<A = ()> {
    pub name: String,
    pub params: Vec<MacroParam>,
    pub return_type: Option<TypeRef>,
    pub body: Expr<A>,
}

impl<A> MacroDecl<A> {
    pub fn new(
        name: impl Into<String>,
        params: Vec<MacroParam>,
        return_type: Option<TypeRef>,
        body: Expr<A>,
    ) -> Self {
        Self { name: name.into(), params, return_type, body }
    }
}

// =============================================================================
// Macro call sites
// =============================================================================

/// Argument at a macro call site.
///
/// | Variant     | Syntax   | Meaning                                              |
/// |-------------|----------|------------------------------------------------------|
/// | Expr        | `expr`   | Normal expression, bound to a Regular param.         |
/// | Symbolic    | `@x`     | Variable name passed to a Symbolic (`@`) param.      |
/// | Placeholder | `name`   | Identifier bound to a Placeholder (`$`) param.       |
#[derive(Debug, Clone, PartialEq)]
pub enum MacroArg<A = ()> {
    /// A regular expression argument.
    Expr(Expr<A>),
    /// A symbolic argument: `@varname` — passes the variable by name.
    Symbolic(String),
    /// A placeholder binding name: names the `$param` variable for this call.
    Placeholder(String),
}

/// A macro invocation: `name(args...) { body_block }`.
///
/// `body` is `Some` when the macro has a `*expr` (BodyExpr) parameter and
/// the caller supplies a trailing `{ ... }` block. It is `None` if the macro
/// has no such parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct MacroCallExpr<A = ()> {
    pub name: String,
    pub args: Vec<MacroArg<A>>,
    pub body: Option<Box<Expr<A>>>,
}

impl<A> MacroCallExpr<A> {
    pub fn new(name: impl Into<String>, args: Vec<MacroArg<A>>, body: Option<Expr<A>>) -> Self {
        Self { name: name.into(), args, body: body.map(Box::new) }
    }
}

// =============================================================================
// Macro pattern matching
// =============================================================================

/// A pattern that matches the shape of an AST node during macro expansion.
#[derive(Debug, Clone, PartialEq)]
pub enum MacroPattern {
    /// Matches any expression (wildcard).
    Wildcard,
    /// Matches a specific literal.
    Literal(Literal),
    /// Binds the matched sub‑expression to a name, optionally with a type constraint.
    Bind {
        name: Option<String>,
        ty: Option<TypeRef>,
        pattern: Box<MacroPattern>,
    },
    /// Matches a binary expression with a specific operator and recursive sub‑patterns.
    BinaryExpr {
        op: BinaryOp,
        left: Box<MacroPatternBind>,
        right: Box<MacroPatternBind>,
    },
    /// Matches a unary expression with a specific operator and a recursive sub‑pattern.
    UnaryExpr {
        op: UnaryOp,
        operand: Box<MacroPatternBind>,
    },
    // Future extensions: Call, Member, Let, etc.
    // Relatively hard for the initial implementation, but could be easily extended later.
    // The main difficulty would be in addapting the parser grammar without conflicts.
}

/// A binding site in a macro pattern: captures the matched sub‑expression
/// under an optional name, optionally with a type constraint.
#[derive(Debug, Clone, PartialEq)]
pub struct MacroPatternBind {
    /// If `Some`, the matched sub‑expression is stored in the substitution map
    /// under this name, so it can be used in the case body.
    pub name: Option<String>,
    /// Optional type annotation (used for consistency checking during expansion).
    pub ty: Option<TypeRef>,
    /// The shape this binding must match.
    pub pattern: MacroPattern,
}

/// A single case in a `macro match` expression.
#[derive(Debug, Clone, PartialEq)]
pub struct MacroCase<A = ()> {
    pub pattern: MacroPattern,
    pub body: Expr<A>,
}

/// A compile‑time `match` expression used inside macro bodies.
/// The scrutinee is a concrete AST node (already expanded), and the cases
/// are tested in order; the first matching case provides the substitution map
/// for expanding its body.
#[derive(Debug, Clone, PartialEq)]
pub struct MacroMatchExpr<A = ()> {
    /// The expression to match against (must be a fully‑expanded AST node).
    pub scrutinee: Box<Expr<A>>,
    /// List of cases evaluated in order.
    pub cases: Vec<MacroCase<A>>,
}

/// Generic AST visitor.
///
/// This trait is intentionally small. Specific compiler passes can implement
/// traversal manually or use [`walk_expr`] for the recursive part.
pub trait AstVisitor<A> {
    type Output;

    fn visit_program(&mut self, program: &Program<A>) -> Self::Output;
    fn visit_declaration(&mut self, declaration: &Declaration<A>) -> Self::Output;
    fn visit_expr(&mut self, expr: &Expr<A>) -> Self::Output;
}

/// Walks an expression in pre-order and calls `visit` for each node.
///
/// Useful for tests and simple analyses. Full semantic passes will usually
/// implement their own visitor because they need scopes and synthesized values.
pub fn walk_expr<A, F>(expr: &Expr<A>, visit: &mut F)
where
    F: FnMut(&Expr<A>),
{
    visit(expr);

    match &expr.kind {
        ExprKind::Literal(_) | ExprKind::Variable(_) | ExprKind::SelfRef | ExprKind::BaseRef => {}
        ExprKind::Unary(unary) => walk_expr(&unary.expr, visit),
        ExprKind::Binary(binary) => {
            walk_expr(&binary.left, visit);
            walk_expr(&binary.right, visit);
        }
        ExprKind::Let(let_expr) => {
            for binding in &let_expr.bindings {
                walk_expr(&binding.initializer, visit);
            }
            walk_expr(&let_expr.body, visit);
        }
        ExprKind::Assign(assign) => {
            walk_assign_target(&assign.target, visit);
            walk_expr(&assign.value, visit);
        }
        ExprKind::Block(block) => {
            for expression in &block.expressions {
                walk_expr(expression, visit);
            }
        }
        ExprKind::If(if_expr) => {
            walk_expr(&if_expr.condition, visit);
            walk_expr(&if_expr.then_branch, visit);
            for elif in &if_expr.elif_branches {
                walk_expr(&elif.condition, visit);
                walk_expr(&elif.body, visit);
            }
            walk_expr(&if_expr.else_branch, visit);
        }
        ExprKind::While(while_expr) => {
            walk_expr(&while_expr.condition, visit);
            walk_expr(&while_expr.body, visit);
        }
        ExprKind::For(for_expr) => {
            walk_expr(&for_expr.iterable, visit);
            walk_expr(&for_expr.body, visit);
        }
        ExprKind::Call(call) => {
            walk_expr(&call.callee, visit);
            for arg in &call.args {
                walk_expr(arg, visit);
            }
        }
        ExprKind::Lambda(lambda) => walk_expr(&lambda.body, visit),
        ExprKind::Member(member) => walk_expr(&member.object, visit),
        ExprKind::New(new_expr) => {
            for arg in &new_expr.args {
                walk_expr(arg, visit);
            }
        }
        ExprKind::TypeTest(type_test) => walk_expr(&type_test.expr, visit),
        ExprKind::Downcast(downcast) => walk_expr(&downcast.expr, visit),
        ExprKind::Vector(vector) => match vector {
            VectorExpr::Literal(items) => {
                for item in items {
                    walk_expr(item, visit);
                }
            }
            VectorExpr::Comprehension(comprehension) => {
                walk_expr(&comprehension.expr, visit);
                walk_expr(&comprehension.iterable, visit);
            }
        },
        ExprKind::Index(index) => {
            walk_expr(&index.object, visit);
            walk_expr(&index.index, visit);
        }
        ExprKind::Match(match_expr) => {
            walk_expr(&match_expr.value, visit);
            for case in &match_expr.cases {
                walk_expr(&case.body, visit);
            }
        }
        ExprKind::MacroCall(mc) => {
            for arg in &mc.args {
                if let MacroArg::Expr(e) = arg {
                    walk_expr(e, visit);
                }
            }
            if let Some(body) = &mc.body {
                walk_expr(body, visit);
            }
        }
        ExprKind::MacroMatch(mm) => {
            // Dereference the box to get &Expr<A>
            walk_expr(&*mm.scrutinee, visit);
            for case in &mm.cases {
                walk_expr(&case.body, visit);
            }
        }
    }
}

fn walk_assign_target<A, F>(target: &AssignTarget<A>, visit: &mut F)
where
    F: FnMut(&Expr<A>),
{
    match target {
        AssignTarget::Variable(_) => {}
        AssignTarget::Member { object, .. } => walk_expr(object, visit),
        AssignTarget::Index { object, index } => {
            walk_expr(object, visit);
            walk_expr(index, visit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s() -> SourceSpan {
        SourceSpan::new(1, 1)
    }

    #[test]
    fn builds_let_expression() {
        let expr = Expr::new(
            ExprKind::Let(LetExpr::new(
                vec![LetBinding::new("x", None, Expr::number(5.0, s()))],
                Expr::binary(
                    BinaryOp::Add,
                    Expr::variable("x", s()),
                    Expr::number(1.0, s()),
                    s(),
                ),
            )),
            s(),
        );

        match expr.kind {
            ExprKind::Let(let_expr) => {
                assert_eq!(let_expr.bindings[0].name, "x");
                assert!(matches!(let_expr.body.kind, ExprKind::Binary(_)));
            }
            other => panic!("expected let expression, got {other:?}"),
        }
    }

    #[test]
    fn type_ref_display_without_args() {
        assert_eq!(TypeRef::named("Number").to_string(), "Number");
    }

    #[test]
    fn type_ref_display_with_args() {
        let t = TypeRef::with_args("Iterable", vec![TypeRef::named("Number")]);
        assert_eq!(t.to_string(), "Iterable<Number>");
    }

    #[test]
    fn type_ref_display_function_type() {
        let t = TypeRef::with_args(
            "Function",
            vec![TypeRef::named("Number"), TypeRef::named("Boolean")],
        );
        assert_eq!(t.to_string(), "(Number) -> Boolean");
    }

    #[test]
    fn walk_expr_visits_children() {
        let expr = Expr::binary(
            BinaryOp::Multiply,
            Expr::binary(
                BinaryOp::Add,
                Expr::number(3.0, s()),
                Expr::number(2.0, s()),
                s(),
            ),
            Expr::number(5.0, s()),
            s(),
        );

        let mut count = 0;
        walk_expr(&expr, &mut |_| count += 1);

        assert_eq!(count, 5);
    }
}
