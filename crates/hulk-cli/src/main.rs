//! Entry point for the HULK compiler.
//!
//! Reads a `.hulk` source file, runs it through the compiler pipeline,
//! and reports any errors with precise source locations.

use std::fs;
use std::path::PathBuf;
use std::process;

use clap::Parser as clapParser;
use hulk_lexer::{LexError, Lexer};
use hulk_parser::{ParseErrorKind, Parser};
use hulk_transpile::expand_program;
use hulk_semantic::analyze;
use hulk_codegen::{compile, link_output, CodegenOptions};

/// The HULK compiler.
#[derive(clapParser)]
#[command(version, about = "HULK language compiler", long_about = None)]
struct Args {
    /// Path to the .hulk source file to compile.
    file: String,
}

fn main() {
    // Pipeline: read source → lex (exit 1) → parse (exit 2) → semantic analysis (exit 3).
    // Exit codes are mandated by the matcom/compilers grader interface contract.

    // Parse command line arguments.
    let args = Args::parse();

    // Read the source file.
    let source = fs::read_to_string(&args.file).unwrap_or_else(|err| {
        eprintln!("error: could not read '{}': {}", args.file, err);
        // Pre-pipeline I/O failure; exit 1 is the closest applicable code.
        process::exit(1);
    });

    // Lex the source code.
    let tokens = Lexer::new(&source).tokenize().unwrap_or_else(|err| {
        // WHY: grader contract requires (line,col) TYPE: message format
        let (line, col, msg) = match &err {
            LexError::UnexpectedChar { ch, span } => (
                span.line,
                span.col,
                format!("unexpected character '{}'", ch),
            ),
            LexError::UnterminatedString { span } => (
                span.line,
                span.col,
                "unterminated string literal".to_string(),
            ),
            LexError::InvalidEscape { ch, span } => (
                span.line,
                span.col,
                format!("invalid escape sequence '\\{}'", ch),
            ),
        };
        eprintln!("({},{}) LEXICAL: {}", line, col, msg);
        process::exit(1);
    });

    // Parse the token stream into an AST.
    let mut program = Parser::new(tokens).parse_program().unwrap_or_else(|err| {
        // WHY: grader contract requires (line,col) TYPE: message format
        let msg = match &err.kind {
            ParseErrorKind::UnexpectedToken { expected, found } => {
                format!("expected {}, found {}", expected, found)
            }
            ParseErrorKind::ExpectedExpression { found } => {
                format!("expected expression, found {}", found)
            }
            ParseErrorKind::ExpectedIdentifier { found } => {
                format!("expected identifier, found {}", found)
            }
            ParseErrorKind::InvalidAssignmentTarget => "invalid assignment target".to_string(),
            ParseErrorKind::Message(message) => message.clone(),
        };
        eprintln!("({},{}) SYNTACTIC: {}", err.span.line, err.span.col, msg);
        process::exit(2);
    });

    // ── Macro expansion ──────────────────────────────────────────────────────────
    let macro_errors = expand_program(&mut program);
    if !macro_errors.is_empty() {
        for err in &macro_errors {
            eprintln!(
                "({},{}) MACRO: {}",
                err.span.line, err.span.col, err.kind
            );
        }
        process::exit(2); // Macro errors are parse-phase failures.
    }

    match analyze(&program) {
        Err(errors) => {
            for error in &errors {
                // WHY: grader contract requires (line,col) TYPE: message format
                eprintln!(
                    "({},{}) SEMANTIC: {}",
                    error.span.line, error.span.col, error.kind
                );
            }
            process::exit(3);
        }
        Ok(verified) => {
            // Print warnings, if any.
            for warning in &verified.warnings {
                eprintln!("warning: {}", warning);
            }

            // Grader contract: produce ./output executable on success.
            let opts = CodegenOptions::with_output_path(PathBuf::from("./output"));
            if let Err(err) = compile(&verified, &opts) {
                eprintln!("error: {}", err);
                process::exit(4); // exit 4 = internal codegen error
            }
            let obj_path = opts.output_path.with_extension("o");
            if let Err(err) = link_output(&obj_path, &opts.output_path) {
                eprintln!("error: {}", err);
                process::exit(4);
            }
        }
    }
}
