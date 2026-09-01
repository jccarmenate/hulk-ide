//! Error types for macro expansion in the Hulk compiler.

use hulk_ast::SourceSpan;
use std::fmt;

/// Represents an error that occurred during macro expansion.
///
/// Contains a specific error kind and the source span where the error was
/// detected.
#[derive(Debug, Clone, PartialEq)]
pub struct MacroError {
    pub kind: MacroErrorKind,
    pub span: SourceSpan,
}

impl MacroError {
    /// Creates a new macro error with the given kind and source span.
    pub fn new(kind: MacroErrorKind, span: SourceSpan) -> Self {
        Self { kind, span }
    }
}

impl fmt::Display for MacroError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({},{}) MACRO: {}", self.span.line, self.span.col, self.kind)
    }
}

/// Enumerates the possible kinds of macro errors.
///
/// Each variant carries additional context about the failure.
#[derive(Debug, Clone, PartialEq)]
pub enum MacroErrorKind {
    /// A macro call references a name that was never defined with `def`.
    UndefinedMacro(String),
    /// Wrong number of arguments supplied to a macro.
    ArityMismatch { name: String, expected: usize, got: usize },
    /// A `@sym` argument was not a plain variable reference.
    SymbolicArgNotVariable { macro_name: String, param: String },
    /// A macro calls itself (direct recursion detected during expansion).
    RecursiveMacro(String),
    /// A `*expr` body argument is required but missing trailing `{ }` block.
    MissingBodyBlock(String),
    /// A regular call was given a `@sym` or `$ph` arg without a matching param.
    MacroArgInNonMacroCall { name: String },
    /// No case matched in the structure.
    NonExhaustiveMacroMatch,
    /// A pattern binding is duplicated.
    DuplicatePatternBinding { name: String },
}

impl fmt::Display for MacroErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UndefinedMacro(n) => write!(f, "undefined macro `{}`", n),
            Self::ArityMismatch { name, expected, got } =>
                write!(f, "macro `{}` expects {} arguments, got {}", name, expected, got),
            Self::SymbolicArgNotVariable { macro_name, param } =>
                write!(f, "symbolic argument `@{}` in macro `{}` must be a variable or attribute", param, macro_name),
            Self::RecursiveMacro(n) => write!(f, "macro `{}` recursively expands itself", n),
            Self::MissingBodyBlock(n) => write!(f, "macro `{}` requires a trailing `{{ }}` block", n),
            Self::MacroArgInNonMacroCall { name } =>
                write!(f, "`@{}` or placeholder argument used in a non-macro call", name),
            Self::NonExhaustiveMacroMatch  => 
                write!(f, "non-exhaustive macro match"),
            Self::DuplicatePatternBinding { name } =>
                write!(f, "pattern binding `{}` is duplicated", name),
        }
    }
}