//! Semantic analysis for HULK.
//!
//! This crate consumes the untyped AST produced by `hulk-parser` and
//! performs name resolution, type inference, and type checking.
//! The output is a fully typed AST (`Program<Type>`) ready for code generation.

#![deny(missing_docs)]

mod environment;
mod error;
mod passes;
mod typed;
mod types;

use crate::error::Severity;

// -----------------------------------------------------------------------------
// Public API re-exports
// -----------------------------------------------------------------------------

pub use environment::{Binding, Environment};
pub use error::{SemanticError, SemanticErrorKind};
pub use passes::topological_order;
pub use typed::{TypedExpr, TypedProgram};
pub use types::{seeded_registry, Type, TypeInfo, TypeRegistry};

// -----------------------------------------------------------------------------
// Main structure
// -----------------------------------------------------------------------------

/// The result of a successful semantic analysis.
///
/// Contains the global type registry (with all resolved signatures), the fully
/// typed AST, and any non‑fatal warnings that were emitted during analysis.
///
/// This is the structure that `hulk-codegen` will consume.
#[derive(Debug, Clone)]
pub struct VerifiedProgram {
    /// The complete global knowledge base: types, protocols, and functions.
    pub registry: TypeRegistry,
    /// The fully typed program tree, guaranteed by the type system to have
    /// a resolved `Type` for every expression.
    pub typed_program: TypedProgram,
    /// Non‑fatal diagnostics (warnings) that were emitted during analysis.
    /// These do not block compilation but should be surfaced to the user.
    pub warnings: Vec<SemanticError>,
}

// -----------------------------------------------------------------------------
// Main entry point
// -----------------------------------------------------------------------------

/// Performs full semantic analysis on an untyped HULK program.
///
/// This runs the four‑pass pipeline:
/// 1. Declaration collection (Pass 0) – builds the global registry.
/// 2. Hierarchy resolution (Pass 1) – resolves inheritance and protocol links.
/// 3. Constructor parameter resolution (Pass 1.5) – resolves constructor parameters for types.
/// 4. Type inference (Pass 2) – builds the typed tree.
/// 5. Type checking (Pass 3) – final consistency sweep.
///
/// Returns `Ok(VerifiedProgram)` if no hard errors are present; warnings are
/// always collected and returned inside `VerifiedProgram`. If any hard error
/// is encountered, returns `Err(Vec<SemanticError>)` containing all errors.
pub fn analyze(program: &hulk_ast::Program) -> Result<VerifiedProgram, Vec<SemanticError>> {
    let mut errors = Vec::new();
    let mut registry = seeded_registry();

    passes::collect(program, &mut registry, &mut errors);
    passes::hierarchy(&mut registry, &mut errors);

    // Early exit: a broken hierarchy invalidates `conforms_to` itself.
    // Continuing would only produce misleading cascade errors.
    if errors.iter().any(|e| e.severity == Severity::Error) {
        return Err(errors);
    }

    passes::resolve_constructor_params(program, &mut registry, &mut errors);

    let typed_program = passes::infer(program, &mut registry, &mut errors);
    passes::check(&typed_program, &registry, &mut errors);

    // Separate errors from warnings.
    let (hard_errors, warnings): (Vec<_>, Vec<_>) = errors
        .into_iter()
        .partition(|e| e.severity == Severity::Error);

    if hard_errors.is_empty() {
        Ok(VerifiedProgram {
            registry,
            typed_program,
            warnings,
        })
    } else {
        Err(hard_errors)
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::types::lowest_common_ancestor;
    use hulk_lexer::Lexer;
    use hulk_parser::parse;

    #[test]
    fn multi_error_reporting_no_cascade() {
        // Program with several unrelated errors: undefined variable, type mismatch, etc.
        let src = "
            type A { }
            type B inherits A { }
            let x: Number = \"hello\" in {
                print(y);
                let z = x + 1 in print(z);
            }
        ";
        let tokens = Lexer::new(src).tokenize().unwrap();
        let program = parse(tokens).unwrap();
        let result = analyze(&program);
        assert!(result.is_err());
        let errors = result.err().unwrap();
        // Expect at least: NotConforming (x annotation), UndefinedVariable (y), and possibly others.
        // But no spurious TypeMismatch on the `x + 1` because x is already Error, so that part should not cascade.
        let not_conforming = errors
            .iter()
            .filter(|e| matches!(e.kind, SemanticErrorKind::NotConforming { .. }))
            .count();
        let undefined = errors
            .iter()
            .filter(|e| matches!(e.kind, SemanticErrorKind::UndefinedVariable(_)))
            .count();
        assert!(not_conforming >= 1);
        assert!(undefined >= 1);
        // There should be no TypeMismatch on `x + 1` because x is Error -> cascade suppression.
        let type_mismatches = errors
            .iter()
            .filter(|e| matches!(e.kind, SemanticErrorKind::TypeMismatch { .. }))
            .count();
        // It could have at most the NotConforming from the let, but not an extra one for the addition.
        // We'll just assert that if there is a TypeMismatch, it's only from the let.
        // The addition should be typed as Error and not produce a new error.
        assert!(type_mismatches <= 1);
    }

    /// Tests that warnings are correctly returned inside `Ok(VerifiedProgram)` and not discarded.
    #[test]
    fn warnings_are_returned_not_discarded_on_success() {
        // A match expression without a catch‑all produces a NonExhaustiveMatch warning.
        let src = "
            let x = 5 in match x {
                case 1 => print(1);
                case 2 => print(2);
            }
        ";
        let tokens = Lexer::new(src).tokenize().unwrap();
        let program = parse(tokens).unwrap();
        let result = analyze(&program);
        assert!(result.is_ok(), "should succeed with warnings");
        let verified = result.unwrap();
        assert!(
            !verified.warnings.is_empty(),
            "warnings should not be empty"
        );
        assert!(
            verified
                .warnings
                .iter()
                .any(|e| matches!(e.kind, SemanticErrorKind::NonExhaustiveMatch)),
            "expected NonExhaustiveMatch warning"
        );
    }

    /// Tests that a hierarchy‑level hard error triggers the early‑return in `analyze`
    /// before inference or checking runs, preventing panics from later passes.
    #[test]
    fn hierarchy_error_short_circuits_before_infer_panics() {
        // Inheritance cycle should be caught in hierarchy pass and cause early return.
        let src = "
            type A inherits B { }
            type B inherits C { }
            type C inherits A { }
            let x = new A() in print(x);
        ";
        let tokens = Lexer::new(src).tokenize().unwrap();
        let program = parse(tokens).unwrap();
        let result = analyze(&program);
        assert!(result.is_err(), "should return Err with hierarchy error");
        let errors = result.err().unwrap();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e.kind, SemanticErrorKind::InheritanceCycle(_))),
            "expected InheritanceCycle error"
        );
    }

    /// Tests that the LCA of two user‑defined types with a shared grandparent correctly
    /// resolves to the grandparent (not Object), verifying that `ancestor_chain` works
    /// on nominal hierarchies beyond builtins.
    #[test]
    fn lca_three_way_with_shared_grandparent() {
        let src = "
            type A { }
            type B inherits A { }
            type C inherits A { }
            let x = if (true) new B() else new C() in print(x);
        ";
        let tokens = Lexer::new(src).tokenize().unwrap();
        let program = parse(tokens).unwrap();
        let result = analyze(&program);
        assert!(result.is_ok(), "LCA should resolve to A");
        let verified = result.unwrap();
        let registry = &verified.registry;

        // Directly compute the LCA of B and C using the registry.
        let lca = lowest_common_ancestor(
            &[Type::Named("B".to_string()), Type::Named("C".to_string())],
            registry,
        );
        assert_eq!(
            lca,
            Type::Named("A".to_string()),
            "LCA should be A, got {:?}",
            lca
        );
    }
}
