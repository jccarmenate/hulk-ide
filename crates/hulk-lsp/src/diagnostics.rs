//! Turns HULK source text into LSP diagnostics.
//!
//! Runs the same phases as `hulk-cli` in the same order — lex, parse,
//! expand macros, analyze — but uses the recovering lexer/parser entry
//! points (see `hulk-lexer`/`hulk-parser`) so every error in a phase is
//! reported at once instead of stopping at the first one.

use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

use hulk_lexer::LexError;
use hulk_semantic::SemanticError;

/// Everything one call to [`compute_diagnostics`] produces: the
/// diagnostics to publish, and — when analysis succeeded — the fully
/// typed program, kept around so hover/completion/go-to-definition can
/// query it even while a *later* edit has a syntax error (see the
/// "last-good tree" idea in the design spec).
pub struct DiagnosticsOutcome {
    pub diagnostics: Vec<Diagnostic>,
    pub last_good: Option<hulk_semantic::VerifiedProgram>,
}

/// Runs the full compiler pipeline over `text` and returns every diagnostic
/// found. Never panics on malformed input — every phase either recovers or
/// short-circuits cleanly.
pub fn compute_diagnostics(text: &str) -> DiagnosticsOutcome {
    let (tokens, lex_errors) = hulk_lexer::Lexer::new(text).tokenize_recovering();
    if !lex_errors.is_empty() {
        return DiagnosticsOutcome {
            diagnostics: lex_errors.iter().map(lex_diagnostic).collect(),
            last_good: None,
        };
    }

    let (mut program, parse_errors) = hulk_parser::parse_recovering(tokens);
    let mut diagnostics: Vec<Diagnostic> = parse_errors
        .iter()
        .map(|err| diagnostic(err.span.line, err.span.col, DiagnosticSeverity::ERROR, err.to_string()))
        .collect();

    let macro_errors = hulk_transpile::expand_program(&mut program);
    diagnostics.extend(macro_errors.iter().map(|err| {
        diagnostic(err.span.line, err.span.col, DiagnosticSeverity::ERROR, err.kind.to_string())
    }));

    let last_good = match hulk_semantic::analyze(&program) {
        Err(sem_errors) => {
            diagnostics.extend(
                sem_errors
                    .iter()
                    .map(|err| semantic_diagnostic(err, DiagnosticSeverity::ERROR)),
            );
            None
        }
        Ok(verified) => {
            diagnostics.extend(
                verified
                    .warnings
                    .iter()
                    .map(|err| semantic_diagnostic(err, DiagnosticSeverity::WARNING)),
            );
            Some(verified)
        }
    };

    DiagnosticsOutcome {
        diagnostics,
        last_good,
    }
}

fn lex_diagnostic(err: &LexError) -> Diagnostic {
    match err {
        LexError::UnexpectedChar { ch, span } => diagnostic(
            span.line,
            span.col,
            DiagnosticSeverity::ERROR,
            format!("unexpected character '{}'", ch),
        ),
        LexError::UnterminatedString { span } => diagnostic(
            span.line,
            span.col,
            DiagnosticSeverity::ERROR,
            "unterminated string literal".to_string(),
        ),
        LexError::InvalidEscape { ch, span } => diagnostic(
            span.line,
            span.col,
            DiagnosticSeverity::ERROR,
            format!("invalid escape sequence '\\{}'", ch),
        ),
    }
}

fn semantic_diagnostic(err: &SemanticError, severity: DiagnosticSeverity) -> Diagnostic {
    diagnostic(err.span.line, err.span.col, severity, err.kind.to_string())
}

fn diagnostic(line: usize, col: usize, severity: DiagnosticSeverity, message: String) -> Diagnostic {
    Diagnostic {
        range: span_to_range(line, col),
        severity: Some(severity),
        source: Some("hulk".to_string()),
        message,
        ..Diagnostic::default()
    }
}

/// Converts a 1-based HULK source position to a one-character 0-based LSP
/// range. HULK spans are single points, not ranges, so a one-character
/// range is the most precise conversion available without deeper
/// AST/token-length lookup.
pub(crate) fn span_to_range(line: usize, col: usize) -> Range {
    let line = line.saturating_sub(1) as u32;
    let character = col.saturating_sub(1) as u32;
    Range {
        start: Position { line, character },
        end: Position {
            line,
            character: character + 1,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compute_diagnostics_only(text: &str) -> Vec<Diagnostic> {
        compute_diagnostics(text).diagnostics
    }

    #[test]
    fn valid_program_has_no_diagnostics() {
        let diagnostics = compute_diagnostics_only("print(1 + 2);");
        assert!(diagnostics.is_empty(), "unexpected diagnostics: {diagnostics:?}");
    }

    #[test]
    fn lexical_error_short_circuits_to_a_single_diagnostic_with_correct_range() {
        let diagnostics = compute_diagnostics_only("#");
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("unexpected character"));
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(
            diagnostics[0].range,
            Range {
                start: Position { line: 0, character: 0 },
                end: Position { line: 0, character: 1 },
            }
        );
    }

    #[test]
    fn parse_error_is_reported_and_semantic_analysis_still_runs_on_the_rest() {
        let source = "function broken(x: Number\nfunction tan(x: Number): Number => sin(x) / cos(x);\nprint(tan(PI));";
        let diagnostics = compute_diagnostics_only(source);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.severity == Some(DiagnosticSeverity::ERROR) && d.range.start.line == 1),
            "expected a parse error diagnostic on 0-based line 1, got: {diagnostics:?}"
        );
    }

    #[test]
    fn semantic_analysis_reports_every_error_not_just_the_first() {
        let diagnostics = compute_diagnostics_only("{ print(a); print(b); }");
        assert_eq!(
            diagnostics.len(),
            2,
            "expected both undefined variables to be reported: {diagnostics:?}"
        );
        assert!(diagnostics.iter().all(|d| d.severity == Some(DiagnosticSeverity::ERROR)));
        assert!(diagnostics.iter().any(|d| d.message.contains("undefined variable `a`")));
        assert!(diagnostics.iter().any(|d| d.message.contains("undefined variable `b`")));
    }

    #[test]
    fn semantic_warning_is_reported_with_warning_severity() {
        let source = r#"
            function classify(x: Object): String {
                match x {
                    case n: Number => "number";
                }
            }
            print(classify(1));
        "#;
        let diagnostics = compute_diagnostics_only(source);
        assert_eq!(diagnostics.len(), 1, "expected exactly one warning: {diagnostics:?}");
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::WARNING));
        assert!(diagnostics[0].message.contains("non-exhaustive match"));
    }

    #[test]
    fn last_good_is_populated_on_success_and_absent_on_semantic_error() {
        let ok = compute_diagnostics("print(1 + 2);");
        assert!(ok.last_good.is_some());

        let broken = compute_diagnostics("{ print(a); }");
        assert!(broken.last_good.is_none());
    }
}
