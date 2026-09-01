//! Semantic diagnostics for HULK.
//!
//! This module defines the error types used by the semantic analyzer.
//! Each pass appends `SemanticError` instances to a shared vector.
//! Errors carry a source span and a severity (error or warning) so that
//! the CLI can report them appropriately.

use std::fmt;

use hulk_ast::SourceSpan;

use crate::types::Type;

// -----------------------------------------------------------------------------
// Severity
// -----------------------------------------------------------------------------

/// The severity of a semantic diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// A hard error that prevents compilation.
    Error,
    /// A non-blocking warning that does not stop compilation.
    Warning,
}

// -----------------------------------------------------------------------------
// SemanticError
// -----------------------------------------------------------------------------

/// A semantic diagnostic: a kind, a source location, and a severity.
///
/// Modeled directly on `hulk_parser::ParseError` so that the CLI can render
/// both phases uniformly.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticError {
    /// The specific kind of diagnostic (error or warning).
    pub kind: SemanticErrorKind,
    /// The source location where the diagnostic originated.
    pub span: SourceSpan,
    /// The severity level (error or warning).
    pub severity: Severity,
}

impl SemanticError {
    /// Creates a new error with `Severity::Error`.
    pub fn error(kind: SemanticErrorKind, span: SourceSpan) -> Self {
        Self {
            kind,
            span,
            severity: Severity::Error,
        }
    }

    /// Creates a new warning with `Severity::Warning`.
    pub fn warning(kind: SemanticErrorKind, span: SourceSpan) -> Self {
        Self {
            kind,
            span,
            severity: Severity::Warning,
        }
    }
}

// -----------------------------------------------------------------------------
// SemanticErrorKind
// -----------------------------------------------------------------------------

/// The specific kind of semantic diagnostic.
///
/// These variants correspond to the checks performed in each pass:
/// - Name resolution and redefinition (Pass 0/2)
/// - Inheritance and protocol conformance (Pass 1)
/// - Type inference and checking (Pass 2/3)
/// - Optional quality-of-life warnings (Step 10)
#[derive(Debug, Clone, PartialEq)]
pub enum SemanticErrorKind {
    // ─── Name resolution ──────────────────────────────────────────────────
    /// An undefined variable name was used.
    UndefinedVariable(String),
    /// An undefined function (or builtin) was called.
    UndefinedFunction {
        /// The name of the undefined function.
        name: String,
        /// The number of arguments supplied in the call.
        arity: usize,
    },
    /// An undefined type name was referenced.
    UndefinedType(String),
    /// A member (attribute or method) could not be found on a type.
    UnknownMember {
        /// The type on which the member was accessed.
        ty: Type,
        /// The name of the missing member.
        member: String,
    },

    // ─── Redeclaration ──────────────────────────────────────────────────
    /// A function was defined more than once (global namespace).
    DuplicateFunction(String),
    /// A method was defined more than once within the same type.
    DuplicateMethod {
        /// The name of the type containing the method.
        ty: String,
        /// The name of the duplicate method.
        method: String,
    },
    /// A type was defined more than once.
    DuplicateType(String),
    /// An attribute was defined more than once within the same type.
    DuplicateAttribute {
        /// The name of the type containing the attribute.
        ty: String,
        /// The name of the duplicate attribute.
        attribute: String,
    },
    /// A parameter name was duplicated within a function/method parameter list.
    DuplicateParameter(String),

    // ─── Inheritance ──────────────────────────────────────────────────────
    /// Attempt to inherit from a builtin value type (`Number`, `String`, `Boolean`).
    InheritFromBuiltinValueType(String),
    /// Attempt to inherit from a type that does not exist.
    InheritFromUndefinedType(String),
    /// A cycle was detected in the inheritance graph.
    InheritanceCycle(
        /// The sequence of type names forming the cycle.
        Vec<String>,
    ),
    /// An overriding method does not match the parent's signature exactly.
    InvalidOverride {
        /// The name of the method being overridden.
        method: String,
        /// The type where the override occurs.
        in_type: String,
        /// The expected signature (from the parent).
        expected: String,
        /// The actual signature found.
        found: String,
    },

    // ─── Protocols & Annotations ──────────────────────────────────────
    /// A type annotation is required but missing (e.g., protocol method parameter).
    MissingTypeAnnotation {
        /// The name of the symbol that lacks a type.
        symbol: String,
        /// The context where the annotation is missing (e.g., "protocol method `foo`").
        context: String,
    },
    /// A type does not implement all methods required by a protocol.
    ProtocolNotImplemented {
        /// The type that fails to implement the protocol.
        ty: Type,
        /// The name of the protocol.
        protocol: String,
        /// The list of missing method names.
        missing: Vec<String>,
    },
    /// A protocol extension violates contravariant/covariant variance rules.
    InvalidProtocolVariance {
        /// The method name where the variance error occurs.
        method: String,
        /// A description of the variance violation.
        reason: String,
    },

    // ─── Typing ──────────────────────────────────────────────────────────
    /// The inferred type does not match the declared annotation.
    TypeMismatch {
        /// The type expected by the annotation.
        expected: Type,
        /// The type inferred from the expression.
        found: Type,
    },
    /// A value of one type cannot be used where another type is expected.
    NotConforming {
        /// The type of the value provided.
        found: Type,
        /// The type that was required.
        expected: Type,
    },
    /// A call supplies the wrong number of arguments.
    ArityMismatch {
        /// The number of arguments expected.
        expected: usize,
        /// The number of arguments supplied.
        found: usize,
    },
    /// An operator is applied to incompatible operand types.
    InvalidOperator {
        /// The operator symbol.
        op: String,
        /// The types of the operands.
        operand_types: Vec<Type>,
    },
    /// A non-boolean value was used as a condition.
    NonBooleanCondition(
        /// The actual type of the condition expression.
        Type,
    ),
    /// A value that does not implement `Iterable` was used in a `for` loop.
    NotIterable(
        /// The type that is not iterable.
        Type,
    ),
    /// Indexing was attempted on a non-vector type.
    IndexOnNonVector(
        /// The type being indexed.
        Type,
    ),
    /// The left-hand side of an assignment is not assignable.
    InvalidAssignTarget,
    /// `self` was used as an assignment target.
    SelfIsNotAssignable,
    /// `base` was used outside an overriding method.
    BaseOutsideOverridingMethod,
    /// A call was made on a value that is not a function.
    CallOnNonFunction {
        /// The type of the value being called.
        ty: Type,
    },
    /// An assignment was made to a method (e.g., `obj.method = ...`).
    AssignToMethod {
        /// The name of the method being assigned to.
        method: String,
    },

    // ─── Inference ──────────────────────────────────────────────────────
    /// A symbol's type could not be inferred (needs an explicit annotation).
    CannotInferType {
        /// The name of the symbol that could not be inferred.
        symbol: String,
    },
    /// Multiple incompatible types were possible for the same symbol.
    AmbiguousInference {
        /// The name of the symbol.
        symbol: String,
        /// The set of candidate types that were equally possible.
        candidates: Vec<Type>,
    },
    /// The semantic pass should never receive macro nodes after expansion
    MacroReferenceFound
    {
        /// Name of the macro expression in which the error originated
        macro_expr: String,
    },

    // ─── Non-blocking warnings (quality-of-life) ──────────────────────
    /// A downcast (`as`) can never succeed because the types are unrelated.
    UnreachableDowncast {
        /// The source type of the downcast.
        from: Type,
        /// The target type of the downcast.
        to: Type,
    },
    /// A `match` expression does not cover all possible cases.
    NonExhaustiveMatch,

    // ─── Internal errors ──────────────────────────────────────────────────────
    /// Generic internal compiler error message.
    /// Used as a fallback when a more specific kind is not available.
    Message {
        /// Message to display
        error_message: String,
    },
}

// -----------------------------------------------------------------------------
// Display implementations
// -----------------------------------------------------------------------------

impl fmt::Display for SemanticError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix = match self.severity {
            Severity::Error => "semantic error",
            Severity::Warning => "semantic warning",
        };
        write!(
            f,
            "{} at line {}, col {}: {}",
            prefix, self.span.line, self.span.col, self.kind
        )
    }
}

impl fmt::Display for SemanticErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Name resolution
            Self::UndefinedVariable(name) => {
                write!(f, "undefined variable `{}`", name)
            }
            Self::UndefinedFunction { name, arity } => {
                write!(
                    f,
                    "undefined function `{}` with {} argument(s)",
                    name, arity
                )
            }
            Self::UndefinedType(name) => {
                write!(f, "undefined type `{}`", name)
            }
            Self::UnknownMember { ty, member } => {
                write!(f, "type `{}` has no member `{}`", ty, member)
            }

            // Redeclaration
            Self::DuplicateFunction(name) => {
                write!(f, "duplicate function `{}`", name)
            }
            Self::DuplicateMethod { ty, method } => {
                write!(f, "duplicate method `{}` in type `{}`", method, ty)
            }
            Self::DuplicateType(name) => {
                write!(f, "duplicate type `{}`", name)
            }
            Self::DuplicateAttribute { ty, attribute } => {
                write!(f, "duplicate attribute `{}` in type `{}`", attribute, ty)
            }
            Self::DuplicateParameter(name) => {
                write!(f, "duplicate parameter `{}`", name)
            }

            // Inheritance
            Self::InheritFromBuiltinValueType(name) => {
                write!(f, "cannot inherit from builtin value type `{}`", name)
            }
            Self::InheritFromUndefinedType(name) => {
                write!(f, "cannot inherit from undefined type `{}`", name)
            }
            Self::InheritanceCycle(cycle) => {
                write!(f, "inheritance cycle detected: {}", cycle.join(" -> "))
            }
            Self::InvalidOverride {
                method,
                in_type,
                expected,
                found,
            } => {
                write!(
                    f,
                    "invalid override of method `{}` in type `{}`: expected `{}`, found `{}`",
                    method, in_type, expected, found
                )
            }

            // Protocols and Annotations
            Self::MissingTypeAnnotation { symbol, context } => {
                write!(f, "missing type annotation for `{}` in {}", symbol, context)
            }
            Self::ProtocolNotImplemented {
                ty,
                protocol,
                missing,
            } => {
                let missing_list = missing.join(", ");
                write!(
                    f,
                    "type `{}` does not implement protocol `{}`; missing methods: {}",
                    ty, protocol, missing_list
                )
            }
            Self::InvalidProtocolVariance { method, reason } => {
                write!(f, "invalid protocol variance for `{}`: {}", method, reason)
            }

            // Typing
            Self::TypeMismatch { expected, found } => {
                write!(
                    f,
                    "type mismatch: expected `{}`, found `{}`",
                    expected, found
                )
            }
            Self::NotConforming { found, expected } => {
                write!(f, "type `{}` does not conform to `{}`", found, expected)
            }
            Self::ArityMismatch { expected, found } => {
                write!(f, "arity mismatch: expected {}, found {}", expected, found)
            }
            Self::InvalidOperator { op, operand_types } => {
                let types: Vec<String> = operand_types.iter().map(|t| t.to_string()).collect();
                write!(
                    f,
                    "invalid operator `{}` for types {}",
                    op,
                    types.join(", ")
                )
            }
            Self::NonBooleanCondition(ty) => {
                write!(f, "non-boolean condition of type `{}`", ty)
            }
            Self::NotIterable(ty) => {
                write!(f, "type `{}` is not iterable", ty)
            }
            Self::IndexOnNonVector(ty) => {
                write!(f, "indexing applied to non-vector type `{}`", ty)
            }
            Self::InvalidAssignTarget => {
                write!(f, "invalid assignment target")
            }
            Self::SelfIsNotAssignable => {
                write!(f, "`self` is not assignable")
            }
            Self::BaseOutsideOverridingMethod => {
                write!(f, "`base` can only be used inside an overriding method")
            }
            Self::CallOnNonFunction { ty } => {
                write!(f, "cannot call value of type `{}`", ty)
            }
            Self::AssignToMethod { method } => {
                write!(f, "cannot assign to method `{}`", method)
            }
            Self::MacroReferenceFound { macro_expr } => {
                write!(f, "macro incorrectly expanded: `{}`", macro_expr)
            }

            // Inference
            Self::CannotInferType { symbol } => {
                write!(
                    f,
                    "cannot infer type for `{}`; add an explicit annotation",
                    symbol
                )
            }
            Self::AmbiguousInference { symbol, candidates } => {
                let types: Vec<String> = candidates.iter().map(|t| t.to_string()).collect();
                write!(
                    f,
                    "ambiguous inference for `{}`: possible types are {}",
                    symbol,
                    types.join(", ")
                )
            }

            // Non-blocking warnings
            Self::UnreachableDowncast { from, to } => {
                write!(
                    f,
                    "unreachable downcast: `{}` as `{}` can never succeed",
                    from, to
                )
            }
            Self::NonExhaustiveMatch => {
                write!(f, "non-exhaustive match: no catch-all pattern")
            }

            // Internal
            Self::Message {error_message} => {
                write!(f, "{}", error_message)
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hulk_ast::SourceSpan;

    fn dummy_span() -> SourceSpan {
        SourceSpan::new(1, 1)
    }

    #[test]
    fn display_format_for_each_kind() {
        // Table of (SemanticErrorKind, expected_display_string)
        let cases = vec![
            (
                SemanticErrorKind::UndefinedVariable("x".to_string()),
                "undefined variable `x`",
            ),
            (
                SemanticErrorKind::UndefinedFunction {
                    name: "foo".to_string(),
                    arity: 2,
                },
                "undefined function `foo` with 2 argument(s)",
            ),
            (
                SemanticErrorKind::UndefinedType("MyType".to_string()),
                "undefined type `MyType`",
            ),
            (
                SemanticErrorKind::UnknownMember {
                    ty: Type::Named("T".to_string()),
                    member: "field".to_string(),
                },
                "type `T` has no member `field`",
            ),
            (
                SemanticErrorKind::DuplicateFunction("f".to_string()),
                "duplicate function `f`",
            ),
            (
                SemanticErrorKind::DuplicateMethod {
                    ty: "A".to_string(),
                    method: "m".to_string(),
                },
                "duplicate method `m` in type `A`",
            ),
            (
                SemanticErrorKind::DuplicateType("T".to_string()),
                "duplicate type `T`",
            ),
            (
                SemanticErrorKind::DuplicateAttribute {
                    ty: "A".to_string(),
                    attribute: "x".to_string(),
                },
                "duplicate attribute `x` in type `A`",
            ),
            (
                SemanticErrorKind::DuplicateParameter("p".to_string()),
                "duplicate parameter `p`",
            ),
            (
                SemanticErrorKind::InheritFromBuiltinValueType("Number".to_string()),
                "cannot inherit from builtin value type `Number`",
            ),
            (
                SemanticErrorKind::InheritFromUndefinedType("Missing".to_string()),
                "cannot inherit from undefined type `Missing`",
            ),
            (
                SemanticErrorKind::InheritanceCycle(vec!["A".to_string(), "B".to_string(), "A".to_string()]),
                "inheritance cycle detected: A -> B -> A",
            ),
            (
                SemanticErrorKind::InvalidOverride {
                    method: "foo".to_string(),
                    in_type: "A".to_string(),
                    expected: "() -> Number".to_string(),
                    found: "() -> String".to_string(),
                },
                "invalid override of method `foo` in type `A`: expected `() -> Number`, found `() -> String`",
            ),
            (
                SemanticErrorKind::MissingTypeAnnotation {
                    symbol: "x".to_string(),
                    context: "protocol method `bar`".to_string(),
                },
                "missing type annotation for `x` in protocol method `bar`",
            ),
            (
                SemanticErrorKind::ProtocolNotImplemented {
                    ty: Type::Named("T".to_string()),
                    protocol: "P".to_string(),
                    missing: vec!["f".to_string()],
                },
                "type `T` does not implement protocol `P`; missing methods: f",
            ),
            (
                SemanticErrorKind::InvalidProtocolVariance {
                    method: "f".to_string(),
                    reason: "return type mismatch".to_string(),
                },
                "invalid protocol variance for `f`: return type mismatch",
            ),
            (
                SemanticErrorKind::TypeMismatch {
                    expected: Type::Number,
                    found: Type::String,
                },
                "type mismatch: expected `Number`, found `String`",
            ),
            (
                SemanticErrorKind::NotConforming {
                    found: Type::Number,
                    expected: Type::String,
                },
                "type `Number` does not conform to `String`",
            ),
            (
                SemanticErrorKind::ArityMismatch {
                    expected: 2,
                    found: 3,
                },
                "arity mismatch: expected 2, found 3",
            ),
            (
                SemanticErrorKind::InvalidOperator {
                    op: "+".to_string(),
                    operand_types: vec![Type::Number, Type::String],
                },
                "invalid operator `+` for types Number, String",
            ),
            (
                SemanticErrorKind::NonBooleanCondition(Type::Number),
                "non-boolean condition of type `Number`",
            ),
            (
                SemanticErrorKind::NotIterable(Type::Number),
                "type `Number` is not iterable",
            ),
            (
                SemanticErrorKind::IndexOnNonVector(Type::Number),
                "indexing applied to non-vector type `Number`",
            ),
            (
                SemanticErrorKind::InvalidAssignTarget,
                "invalid assignment target",
            ),
            (
                SemanticErrorKind::SelfIsNotAssignable,
                "`self` is not assignable",
            ),
            (
                SemanticErrorKind::BaseOutsideOverridingMethod,
                "`base` can only be used inside an overriding method",
            ),
            (
                SemanticErrorKind::CannotInferType {
                    symbol: "x".to_string(),
                },
                "cannot infer type for `x`; add an explicit annotation",
            ),
            (
                SemanticErrorKind::AmbiguousInference {
                    symbol: "x".to_string(),
                    candidates: vec![Type::Number, Type::String],
                },
                "ambiguous inference for `x`: possible types are Number, String",
            ),
            (
                SemanticErrorKind::UnreachableDowncast {
                    from: Type::Named("A".to_string()),
                    to: Type::Named("B".to_string()),
                },
                "unreachable downcast: `A` as `B` can never succeed",
            ),
            (
                SemanticErrorKind::NonExhaustiveMatch,
                "non-exhaustive match: no catch-all pattern",
            ),
        ];

        for (kind, expected) in cases {
            let rendered = kind.to_string();
            assert_eq!(
                rendered, expected,
                "rendered: `{}`, expected: `{}`",
                rendered, expected
            );
        }
    }

    #[test]
    fn severity_prefix_in_full_error_display() {
        let span = dummy_span();
        let error =
            SemanticError::error(SemanticErrorKind::UndefinedVariable("x".to_string()), span);
        assert!(error
            .to_string()
            .starts_with("semantic error at line 1, col 1: undefined variable `x`"));

        let warning = SemanticError::warning(SemanticErrorKind::NonExhaustiveMatch, span);
        assert!(warning.to_string().starts_with(
            "semantic warning at line 1, col 1: non-exhaustive match: no catch-all pattern"
        ));
    }
}
