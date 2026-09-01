//! Error type returned by `hulk_codegen::compile` and its building blocks.
//!
//! Reaching this type at all means `hulk-semantic`'s `analyze` already
//! accepted the program — these are failures in turning an already
//! type-checked program into machine code, not language-level errors. They
//! are reported and handled separately from the lexical/syntax/semantic
//! error classes the compiler driver already knows about.

use hulk_ast::SourceSpan;
use std::fmt;

/// A code generation error with an optional source location.
#[derive(Debug)]
pub struct CodegenError {
    pub kind: CodegenErrorKind,
    pub span: Option<SourceSpan>,
}

#[derive(Debug)]
pub enum CodegenErrorKind {
    /// LLVM module verification failed (internal).
    LlvmVerification(String),
    /// Filesystem operation failed.
    Io(std::io::Error),
    /// Target machine emission failed.
    TargetEmission(String),
    /// System linker invocation failed.
    Link {
        driver: String,
        status: Option<i32>,
        stderr: String,
    },
    /// Unsupported language construct encountered (internal).
    Unsupported { construct: String },
    /// Internal unnidentified compiler error.
    Internal(String),
    /// The LLVM optimization pipeline rejected or crashed on the module.
    Optimization(String),
}

// ─── Constructors ─────────────────────────────────────────────────────────

impl CodegenError {
    pub fn llvm_verification(msg: impl Into<String>) -> Self {
        Self {
            kind: CodegenErrorKind::LlvmVerification(msg.into()),
            span: None,
        }
    }

    pub fn io(err: std::io::Error) -> Self {
        Self {
            kind: CodegenErrorKind::Io(err),
            span: None,
        }
    }

    pub fn target_emission(msg: impl Into<String>) -> Self {
        Self {
            kind: CodegenErrorKind::TargetEmission(msg.into()),
            span: None,
        }
    }

    pub fn link(driver: impl Into<String>, status: Option<i32>, stderr: impl Into<String>) -> Self {
        Self {
            kind: CodegenErrorKind::Link {
                driver: driver.into(),
                status,
                stderr: stderr.into(),
            },
            span: None,
        }
    }

    pub fn unsupported(construct: impl Into<String>, span: Option<SourceSpan>) -> Self {
        Self {
            kind: CodegenErrorKind::Unsupported {
                construct: construct.into(),
            },
            span,
        }
    }

    pub fn internal(msg: impl Into<String>, span: Option<SourceSpan>) -> Self {
        Self {
            kind: CodegenErrorKind::Internal(msg.into()),
            span: span,
        }
    }

    pub fn optimization(msg: impl Into<String>) -> Self {
        Self {
            kind: CodegenErrorKind::Optimization(msg.into()),
            span: None,
        }
    }

    pub fn with_span(mut self, span: SourceSpan) -> Self {
        self.span = Some(span);
        self
    }
}

impl From<std::io::Error> for CodegenError {
    fn from(err: std::io::Error) -> Self {
        Self::io(err)
    }
}

pub trait ResultExt<T> {
    fn with_span(self, span: SourceSpan) -> Result<T, CodegenError>;
}

impl<T> ResultExt<T> for Result<T, CodegenError> {
    fn with_span(self, span: SourceSpan) -> Result<T, CodegenError> {
        self.map_err(|e| e.with_span(span))
    }
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (line, col) = self.span.map_or((0, 0), |s| (s.line, s.col));
        write!(f, "({},{}) ", line, col)?;
        match &self.kind {
            CodegenErrorKind::LlvmVerification(msg) => {
                write!(f, "internal error: LLVM module failed verification: {msg}")
            }
            CodegenErrorKind::Io(err) => write!(f, "I/O error during code generation: {err}"),
            CodegenErrorKind::TargetEmission(msg) => {
                write!(f, "failed to emit object code: {msg}")
            }
            CodegenErrorKind::Link {
                driver,
                status,
                stderr,
            } => {
                write!(
                    f,
                    "linker `{driver}` failed (exit status {status:?}):\n{stderr}"
                )
            }
            CodegenErrorKind::Unsupported { construct } => {
                write!(f, "unsupported construct: {construct}")
            }
            CodegenErrorKind::Internal(msg) => {
                write!(f, "internal compiler error: {msg}")
            }
            CodegenErrorKind::Optimization(msg) => {
                write!(f, "optimization pipeline failed: {msg}")
            }
        }
    }
}
