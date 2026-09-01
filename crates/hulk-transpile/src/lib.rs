//! Macro expansion pass for the HULK compiler.

mod collect;
mod error;
mod expand;
pub mod pattern; // TODO: structural pattern matching
mod substitute;

pub use error::{MacroError, MacroErrorKind};

/// Expands all macro declarations and macro calls in `program` in place.
///
/// After this call:
/// - All `DeclarationKind::Macro` entries are removed from `program.declarations`.
/// - All `ExprKind::MacroCall` nodes are replaced with their expanded forms.
/// - Any errors (undefined macros, arity mismatches, etc.) are returned.
///
/// Returns an empty `Vec` on success.
pub fn expand_program(program: &mut hulk_ast::Program) -> Vec<MacroError> {
    let mut errors = Vec::new();

    // Pass A: collect macro declarations (removes them from the program).
    let registry = collect::collect(program, &mut errors);

    if !errors.is_empty() {
        return errors;
    }

    // Pass B: expand all macro call sites.
    expand::expand(program, &registry, &mut errors);

    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use hulk_lexer::Lexer;
    use hulk_parser::parse;
    use hulk_ast::{ExprKind, DeclarationKind};

    fn expand_source(src: &str) -> (hulk_ast::Program, Vec<MacroError>) {
        let tokens = Lexer::new(src).tokenize().expect("lexer failed");
        let mut program = parse(tokens).expect("parser failed");
        let errors = expand_program(&mut program);
        (program, errors)
    }

    #[test]
    fn basic_repeat_macro_expands() {
        let (prog, errs) = expand_source(
            r#"
            def repeat(n: Number, *expr: Object): Object =>
                let total = n in
                while (total >= 0) {
                    total := total - 1;
                    expr;
                };
            repeat(3) { print("hi"); }
            "#,
        );
        assert!(errs.is_empty(), "unexpected errors: {:?}", errs);
        // No MacroCallExpr or MacroDecl should remain.
        assert!(prog.declarations.iter().all(|d| !matches!(d.kind, DeclarationKind::Macro(_))));
        // Entry should be a Let (the expanded `let total = 3 in while...`).
        assert!(
            matches!(prog.entry.kind, ExprKind::Let(_)),
            "expected Let, got {:?}",
            prog.entry.kind
        );
    }

    #[test]
    fn variable_sanitization_renames_internal_let() {
        let (prog, errs) = expand_source(
            r#"
            def repeat(n: Number, *expr: Object): Object =>
                let total = n in while (total >= 0) {
                    total := total - 1;
                    expr;
                };
            let total = 10 in repeat(total) { print(total); }
            "#,
        );
        assert!(errs.is_empty());
        // Outer let should preserve user's name.
        let outer_let = match &prog.entry.kind {
            ExprKind::Let(l) => l,
            _ => panic!("expected outer let"),
        };
        assert_eq!(outer_let.bindings[0].name, "total");
        // Inner let (macro body) must be sanitized.
        let inner_let = match &outer_let.body.kind {
            ExprKind::Let(l) => l,
            _ => panic!("expected inner let (sanitized)"),
        };
        assert_ne!(inner_let.bindings[0].name, "total");
        assert!(inner_let.bindings[0].name.starts_with("__hulk_m"));
    }

    #[test]
    fn symbolic_arg_swap_macro() {
        let (prog, errs) = expand_source(
            r#"
            def swap(@a: Object, @b: Object): Object =>
                let temp: Object = a in {
                    a := b;
                    b := temp;
                };
            let x: Object = 5, y: Object = 10 in { swap(@x, @y); x }
            "#,
        );
        assert!(errs.is_empty());
        assert!(!matches!(prog.entry.kind, ExprKind::MacroCall(_)));
    }

    #[test]
    fn undefined_macro_reports_error() {
        let (_, errs) = expand_source("nonexistent(10) { print(1); }");
        assert!(
            errs.iter().any(|e| matches!(e.kind, MacroErrorKind::UndefinedMacro(_))),
            "should report undefined macro"
        );
    }

    #[test]
    fn arity_mismatch_reports_error() {
        let (_, errs) = expand_source(
            r#"
            def repeat(n: Number, *expr: Object): Object => expr;
            repeat(1, 2) { print(1); }
            "#,
        );
        assert!(
            errs.iter().any(|e| matches!(e.kind, MacroErrorKind::ArityMismatch { .. })),
            "should report arity mismatch"
        );
    }
}