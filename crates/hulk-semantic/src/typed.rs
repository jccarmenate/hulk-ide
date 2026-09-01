//! Typed AST aliases for the semantic analyzer's output.
//!
//! This module provides the specific instantiations of `hulk_ast`'s generic
//! tree with `Type` as the annotation parameter. These aliases are used by
//! the later passes (checking and code generation) to work with a fully
//! typed tree.

use crate::types::Type;

/// A fully typed expression: the same shape as `hulk_ast::Expr`, but with
/// a resolved `Type` stored in the `anno` field.
pub type TypedExpr = hulk_ast::Expr<Type>;

/// A fully typed program: the same shape as `hulk_ast::Program`, but with
/// every expression annotated with its resolved `Type`.
pub type TypedProgram = hulk_ast::Program<Type>;
