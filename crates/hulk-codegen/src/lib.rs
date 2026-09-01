//! Code generation for HULK: turns a fully type-checked
//! `hulk_semantic::VerifiedProgram` into a native Linux x86_64 executable.
//!
//! The compiler is written in Rust and can be developed on any platform
//! (Windows, macOS, Linux). However, all generated HULK executables are
//! Linux x86_64 binaries — this is a cross-compiler in the strict sense
//!  when developed on Windows.
//!
//! The pipeline is: lower the typed AST into an LLVM module, verify and
//! optimize that module, emit it as a Linux x86_64 relocatable object file,
//! and link that object file with the `hulk-rt` runtime library using `clang`
//! (or `cc` on non-Windows) with the Linux target triple. This crate owns the
//! first three steps; linking is delegated to a thin driver function so the
//! compiler frontend can place the resulting executable wherever its own
//! contract requires it.

pub mod context;
pub mod emit;
pub mod error;
pub mod itables;
pub mod layout;
pub mod lower;
pub mod optimize;
pub mod options;
pub mod runtime_decls;

use std::path::Path;
use std::path::PathBuf;

use inkwell::context::Context;
use inkwell::values::FunctionValue;

pub use context::CodegenCtx;
pub use error::CodegenError;
pub use options::{CodegenOptions, OptLevel};

const CARGO_MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// Declares `hulk_rt_noop` as an external symbol so generated IR can call
/// it. Stands in for the `runtime_decls` module that later phases will use
/// to declare every `hulk-rt` entry point this crate calls into.
fn declare_smoke_runtime_fn<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    let fn_type = ctx.context.void_type().fn_type(&[], false);
    ctx.module.add_function("hulk_rt_noop", fn_type, None)
}

/// Builds the smoke-test module: a `main` function that calls
/// `hulk_rt_noop` and returns `0`. Exposed independently of `compile` so
/// the smoke example and unit tests can exercise it without needing a real
/// `VerifiedProgram`.
pub fn build_smoke_module(context: &Context) -> Result<CodegenCtx<'_>, CodegenError> {
    let ctx = CodegenCtx::new(context, "hulk_smoke", OptLevel::None)?;

    let noop = declare_smoke_runtime_fn(&ctx);

    let i32_t = ctx.context.i32_type();
    let main_fn = ctx
        .module
        .add_function("main", i32_t.fn_type(&[], false), None);
    let entry_bb = ctx.context.append_basic_block(main_fn, "entry");
    ctx.builder.position_at_end(entry_bb);
    ctx.builder
        .build_call(noop, &[], "call_noop")
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;
    ctx.builder
        .build_return(Some(&i32_t.const_int(0, false)))
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    ctx.module
        .verify()
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))?;

    Ok(ctx)
}

/// Writes `ctx`'s module to `path` as human-readable LLVM IR.
pub fn emit_llvm_ir_to_file(ctx: &CodegenCtx, path: &Path) -> Result<(), CodegenError> {
    ctx.module
        .print_to_file(path)
        .map_err(|e| CodegenError::llvm_verification(e.to_string()))
}

pub fn compile(
    verified: &hulk_semantic::VerifiedProgram,
    opts: &options::CodegenOptions,
) -> Result<(), error::CodegenError> {
    let context = inkwell::context::Context::create();
    let mut codegen = context::CodegenCtx::new(&context, "hulk_main", opts.opt_level)?;

    // Declare runtime functions
    runtime_decls::declare_all(&mut codegen);

    // Create main function that returns i32.
    let i32_type = context.i32_type();
    let main_fn = codegen
        .module
        .add_function("main", i32_type.fn_type(&[], false), None);
    let entry_bb = context.append_basic_block(main_fn, "entry");
    codegen.builder.position_at_end(entry_bb);

    // Build type layouts
    layout::build_layouts(&verified.typed_program, &verified.registry, &mut codegen)?;

    // Declare all functions (free and methods)
    lower::decl::declare_functions(&mut codegen, &verified.typed_program, &verified.registry)?;
    lower::method::declare_methods(&mut codegen, &verified.typed_program, &verified.registry)?;

    // Emit GC field maps (before build_vtables)
    layout::build_gc_field_maps(&mut codegen, &verified.registry)?;

    // Build vtables (requires method declarations)
    layout::build_vtables(&mut codegen, &verified.registry)?;

    // Build itables for used protocols.
    itables::build_itables(&mut codegen, &verified.registry, &verified.typed_program)?;

    // Define all functions (free and methods)
    lower::decl::define_functions(&mut codegen, &verified.typed_program, &verified.registry)?;
    lower::method::define_methods(&mut codegen, &verified.typed_program, &verified.registry)?;

    // Reset builder to main entry
    codegen.builder.position_at_end(entry_bb);

    // Declare downcast check and fail functions (used in type tests and downcasts)
    let _downcast_check = runtime_decls::declare_downcast_check(&codegen);
    let _downcast_fail = runtime_decls::declare_downcast_fail(&codegen);
    let _internal_error = runtime_decls::declare_internal_error(&codegen);

    // Lower the entry expression and capture its value.
    let entry_result = {
        let mut lower_ctx =
            lower::LowerCtx::new(&mut codegen, &verified.registry, &verified.typed_program);
        lower::lower_expr(&mut lower_ctx, &verified.typed_program.entry)?
    };

    // Release the entry result if it is heap-allocated.
    if lower::utils::is_heap_allocated_type(&verified.typed_program.entry.anno, &verified.registry)
    {
        if let Some(release_fn) = codegen.functions.get("hulk_rt_release").cloned() {
            let _ = codegen
                .builder
                .build_call(release_fn, &[entry_result.into()], "release_entry");
        }
    }

    // Return 0.
    codegen
        .builder
        .build_return(Some(&i32_type.const_int(0, false)))
        .map_err(|e| error::CodegenError::llvm_verification(e.to_string()))?;

    // Verify pre-optimization module.
    codegen
        .module
        .verify()
        .map_err(|e| error::CodegenError::llvm_verification(
            format!("pre-optimization: {}", e.to_string())
        ))?;

    // Optimize IR code
    optimize::optimize(&codegen, opts.opt_level)?;
    
    // Verify post-optimization module.
    codegen
        .module
        .verify()
        .map_err(|e| error::CodegenError::llvm_verification(
            format!("post-optimization: {}", e.to_string())
        ))?;

    // Optional IR dump
    if let Some(ll_path) = &opts.emit_llvm_path {
        emit_llvm_ir_to_file(&codegen, ll_path)?;
    }

    // Emit object file.
    let obj_path = opts.output_path.with_extension("o");
    emit::write_object_file(&codegen.target_machine, &codegen.module, &obj_path)?;

    Ok(())
}

/// Links the compiled object file with hulk-rt to produce the final executable.
/// Called by the CLI after compile() succeeds.
pub fn link_output(
    obj_path: &std::path::Path,
    output_path: &std::path::Path,
) -> Result<(), error::CodegenError> {
    let cwd = std::env::current_dir()
        .map_err(|e| error::CodegenError::link("cc", None, format!("cannot get cwd: {e}")))?;

    // Compute the workspace target directory from CARGO_MANIFEST_DIR.
    let manifest_dir = PathBuf::from(CARGO_MANIFEST_DIR);
    let workspace_target = manifest_dir
        .parent()                 // crates/
        .and_then(|p| p.parent()) // workspace root
        .map(|root| root.join("target"))
        .expect("locate workspace target directory");

    // The binary may be invoked as target/release/hulk-cli (dev)
    // or as ./hulk copied to repo root (grader). Probe both.
    let rt_lib_dir = [
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|p| p.to_path_buf())),
        Some(cwd.join("target").join("release")),
        Some(cwd.join("target").join("debug")),
        Some(workspace_target.join("release")),
        Some(workspace_target.join("debug")),
    ]
    .into_iter()
    .flatten()
    .find(|p| p.join("libhulk_rt.a").exists())
    .ok_or_else(|| {
        error::CodegenError::link(
            "cc",
            None,
            "cannot find libhulk_rt.a — run make build first".to_string(),
        )
    })?;

    let cc_output = std::process::Command::new("cc")
        .arg(obj_path)
        .arg(format!("-L{}", rt_lib_dir.display()))
        .arg("-lhulk_rt")
        .arg("-lm")
        .arg("-o")
        .arg(output_path)
        .output()
        .map_err(|e| error::CodegenError::link("cc", None, format!("failed to invoke cc: {e}")))?;

    if !cc_output.status.success() {
        return Err(error::CodegenError::link(
            "cc",
            cc_output.status.code(),
            String::from_utf8_lossy(&cc_output.stderr).into_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Integration tests for the full HULK compiler pipeline: lex → parse → analyze → codegen.
    //!
    //! These tests validate that a real source string can be compiled to a valid Linux x86_64
    //! object file. They also check that semantic errors are caught and that unsupported
    //! constructs (in Phase 3) are properly rejected.

    use inkwell::context::Context;
    use std::io::Read;
    use std::path::PathBuf;

    use crate::{build_smoke_module, compile, CodegenOptions};
    use hulk_lexer::Lexer;
    use hulk_parser::parse;
    use hulk_semantic::analyze;
    use tempfile::tempdir;

    /// Compiles a HULK source string to an object file and an executable path (the latter is
    /// a placeholder since linking is not yet implemented in Phase 3). Returns the path to
    /// the generated object file.
    ///
    /// # Panics
    /// Panics if lexing, parsing, semantic analysis, or code generation fails.
    fn compile_source_to_obj(src: &str) -> (tempfile::TempDir, PathBuf) {
        let tokens = Lexer::new(src).tokenize().expect("lex failed");
        let program = parse(tokens).expect("parse failed");
        let verified = analyze(&program).expect("semantic analysis failed");

        let temp_dir = tempdir().expect("create temp dir");
        let output_path = temp_dir.path().join("output");
        let opts = CodegenOptions::with_output_path(output_path);
        compile(&verified, &opts).expect("code generation failed");

        let obj_path = temp_dir.path().join("output.o");
        assert!(obj_path.exists(), "object file not created");
        (temp_dir, obj_path)
    }

    /// Checks that a file is a valid ELF binary by reading its magic number.
    fn is_elf(path: &PathBuf) -> bool {
        let mut file = std::fs::File::open(path).unwrap();
        let mut header = [0u8; 4];
        file.read_exact(&mut header).unwrap();
        header == [0x7f, b'E', b'L', b'F']
    }

    // ─── Positive tests ──────────────────────────────────────────────────────

    #[test]
    fn test_simple_arithmetic() {
        let src = "let x = 5 in x + 3;";
        let (_tmp_dir, obj) = compile_source_to_obj(src);
        assert!(is_elf(&obj));
    }

    #[test]
    fn test_if_expression() {
        let src = "if (true) 1 else 2;";
        let (_tmp_dir, obj) = compile_source_to_obj(src);
        assert!(is_elf(&obj));
    }

    #[test]
    fn test_if_elif_else() {
        let src = "
            let x = 42 in
            if (x < 10) 1
            elif (x < 50) 2
            else 3;
        ";
        let (_tmp_dir, obj) = compile_source_to_obj(src);
        assert!(is_elf(&obj));
    }

    #[test]
    fn test_while_loop() {
        let src = "
            let x = 0 in
            while (x < 5) x := x + 1;
        ";
        let (_tmp_dir, obj) = compile_source_to_obj(src);
        assert!(is_elf(&obj));
    }

    #[test]
    fn test_string_literal_and_concat() {
        let src = "
            let a = \"Hello\" in
            let b = \"World\" in
            a @ \" \" @ b;
        ";
        let (_tmp_dir, obj) = compile_source_to_obj(src);
        assert!(is_elf(&obj));
    }

    #[test]
    fn test_let_shadowing() {
        let src = "
            let a = 1 in
            let a = 2 in
            a;
        ";
        let (_tmp_dir, obj) = compile_source_to_obj(src);
        assert!(is_elf(&obj));
    }

    #[test]
    fn test_block_expression() {
        let src = "
            {
                let x = 10 in x + 20;
            }
        ";
        let (_tmp_dir, obj) = compile_source_to_obj(src);
        assert!(is_elf(&obj));
    }

    #[test]
    fn test_assign_variable() {
        let src = "
            let x = 0 in
            {
                x := 42;
                x;
            }
        ";
        let (_tmp_dir, obj) = compile_source_to_obj(src);
        assert!(is_elf(&obj));
    }

    #[test]
    fn test_function_call() {
        let src = "
            function f(): Number => 42;
            f();
        ";
        let (_tmp_dir, obj) = compile_source_to_obj(src);
        assert!(is_elf(&obj));
    }

    #[test]
    fn test_function_with_args() {
        let src = "
            function add(x: Number, y: Number): Number => x + y;
            add(2, 3);
        ";
        let (_tmp_dir, obj) = compile_source_to_obj(src);
        assert!(is_elf(&obj));
    }

    #[test]
    fn test_recursive_function() {
        let src = "
            function fact(n: Number): Number =>
                if (n == 0) 1 else n * fact(n - 1);
            fact(5);
        ";
        let (_tmp_dir, obj) = compile_source_to_obj(src);
        assert!(is_elf(&obj));
    }

    // ─── Negative tests (semantic errors) ──────────────────────────────────

    #[test]
    fn test_semantic_type_mismatch() {
        let src = "let x: Number = \"hello\" in print(x);";
        let tokens = Lexer::new(src).tokenize().unwrap();
        let program = parse(tokens).unwrap();
        let result = analyze(&program);
        assert!(result.is_err(), "expected semantic error");
    }

    #[test]
    fn test_undefined_variable() {
        let src = "print(x);";
        let tokens = Lexer::new(src).tokenize().unwrap();
        let program = parse(tokens).unwrap();
        let result = analyze(&program);
        assert!(result.is_err(), "expected semantic error");
    }

    // // ─── Codegen unsupported constructs ────────────────────────────────────

    // #[test]
    // fn test_unsupported_vector() {
    //     let src = "let v = [1,2,3] in v;";
    //     let tokens = Lexer::new(src).tokenize().unwrap();
    //     let program = parse(tokens).unwrap();
    //     let verified = analyze(&program).expect("semantic analysis should succeed");
    //     let temp_dir = tempdir().expect("create temp dir");
    //     let output_path = temp_dir.path().join("output");
    //     let opts = CodegenOptions::with_output_path(output_path);
    //     let result = compile(&verified, &opts);
    //     assert!(result.is_err(), "expected codegen error for vector");
    //     let err = result.unwrap_err();
    //     assert!(err.to_string().contains("vectors not yet supported"));
    // }

    // ─── Additional check: object file is valid ELF ─────────────────────────

    #[test]
    fn test_object_file_valid_elf() {
        let src = "let x = 1 in x;";
        let (_tmp_dir, obj) = compile_source_to_obj(src);
        assert!(is_elf(&obj));
    }

    // ─── Verify smoke module builds and contains expected functions ────────────

    #[test]
    fn smoke_module_builds_and_verifies() {
        let context = Context::create();
        let ctx = build_smoke_module(&context).expect("smoke module should build and verify");
        assert!(ctx.module.get_function("main").is_some());
        assert!(ctx.module.get_function("hulk_rt_noop").is_some());
    }

    // ─── Object‑oriented features (classes, methods, inheritance) ────────

    #[test]
    fn test_class_with_method() {
        let src = "
            type A {
                f(): Number => 42;
            }
            let a = new A() in a.f();
        ";
        let (_tmp_dir, obj) = compile_source_to_obj(src);
        assert!(is_elf(&obj));
    }

    #[test]
    fn test_class_with_attribute_via_method() {
        let src = "
            type A {
                x = 10;
                getX(): Number => self.x;
            }
            let a = new A() in a.getX();
        ";
        let (_tmp_dir, obj) = compile_source_to_obj(src);
        assert!(is_elf(&obj));
    }

    #[test]
    fn test_inheritance_method_override() {
        let src = "
            type A {
                f(): Number => 1;
            }
            type B inherits A {
                f(): Number => 2;
            }
            let b = new B() in b.f();
        ";
        let (_tmp_dir, obj) = compile_source_to_obj(src);
        assert!(is_elf(&obj));
    }

    #[test]
    fn test_base_call() {
        let src = "
            type A {
                f(): Number => 1;
            }
            type B inherits A {
                f(): Number => base();
            }
            let b = new B() in b.f();
        ";
        let (_tmp_dir, obj) = compile_source_to_obj(src);
        assert!(is_elf(&obj));
    }

    #[test]
    fn test_type_test() {
        let src = "
            type A { }
            type B inherits A { }
            let b = new B() in b is A;
        ";
        let (_tmp_dir, obj) = compile_source_to_obj(src);
        assert!(is_elf(&obj));
    }

    #[test]
    fn test_downcast() {
        let src = "
            type A { }
            type B inherits A { }
            let x: A = new B() in x as B;
        ";
        let (_tmp_dir, obj) = compile_source_to_obj(src);
        assert!(is_elf(&obj));
    }

    #[test]
    fn test_bare_method_reference() {
        let src = "
            type A {
                f(): Number => 42;
            }
            let a = new A() in
            let g = a.f in
            g();
        ";
        let (_tmp_dir, obj) = compile_source_to_obj(src);
        assert!(is_elf(&obj));
    }

    #[test]
    fn test_assign_member() {
        let src = "
            type A {
                x = 0;
                setX(v: Number) => self.x := v;
                getX(): Number => self.x;
            }
            let a = new A() in {
                a.setX(42);
                a.getX();
            }
        ";
        let (_tmp_dir, obj) = compile_source_to_obj(src);
        assert!(is_elf(&obj));
    }
}

#[cfg(test)]
mod macro_tests {
    use super::*;
    use hulk_lexer::Lexer;
    use hulk_parser::parse;
    use hulk_transpile::expand_program;
    use hulk_semantic::analyze;
    use std::process::Command;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn ensure_rt_built() {
        // Locate the workspace target directory.
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_target = manifest_dir
            .parent()
            .and_then(|p| p.parent())
            .map(|root| root.join("target"))
            .expect("locate workspace target directory");

        let raw_profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());

        let dir_name: &str = match raw_profile.as_str() {
        "dev" | "test" => "debug",
        other => other,
        };

        let cargo_profile: &str = match raw_profile.as_str() {
            "debug" => "dev",
            other => other,
        };

        // Build hulk-rt with the current profile.
        let status = std::process::Command::new("cargo")
            .args(["build", "-p", "hulk-rt", "--profile", cargo_profile])
            .status()
            .unwrap_or_else(|e| panic!("Failed to invoke cargo to build hulk-rt: {e}"));

        assert!(status.success(), "cargo build -p hulk-rt --profile {cargo_profile} failed");

        let rt_lib_path = workspace_target.join(dir_name).join("libhulk_rt.a");

        // Verify the library now exists.
        assert!(
            rt_lib_path.exists(),
            "hulk-rt built but library not found at {:?}",
            rt_lib_path
        );
    }

    fn compile_and_run(src: &str) -> String {
        let tokens = Lexer::new(src).tokenize().expect("lex failed");
        let mut program = parse(tokens).expect("parse failed");
        let macro_errors = expand_program(&mut program);
        assert!(macro_errors.is_empty(), "macro expansion errors: {:?}", macro_errors);
        let verified = analyze(&program).expect("semantic analysis failed");

        let temp_dir = tempdir().expect("create temp dir");
        let output_path = temp_dir.path().join("output");
        let opts = CodegenOptions::with_output_path(output_path.clone());
        compile(&verified, &opts).expect("codegen failed");

        ensure_rt_built();

        let obj_path = output_path.with_extension("o");
        link_output(&obj_path, &output_path).expect("linking failed");

        let output = Command::new(&output_path)
            .output()
            .expect("failed to run executable");
        assert!(output.status.success(), "executable failed: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn repeat_macro_basic() {
        let src = r#"
            def repeat(n: Number, *expr: Object): Object =>
                let total = n in
                while (total >= 0) {
                    total := total - 1;
                    expr;
                };
            repeat(3) { print("hi"); }
        "#;
        let output = compile_and_run(src);
        assert_eq!(output, "hi\nhi\nhi\nhi");
    }

    #[test]
    fn swap_macro_symbolic() {
        let src = r#"
            def swap(@a: Object, @b: Object) {
                let temp: Object = a in {
                    a := b;
                    b := temp;
                }
            }
            let x: Object = 5, y: Object = 10 in {
                swap(@x, @y);
                print(x);
                print(y);
            }
        "#;
        let output = compile_and_run(src);
        assert_eq!(output, "10\n5");
    }

    #[test]
    fn repeat_macro_with_placeholder() {
        let src = r#"
            def repeat($iter: Number, n: Number, *expr:Object) {
                let iter: Number = 0, total:Number = n in {
                    while (total >= 0) {
                        total := total - 1;
                        expr;
                        iter := iter + 1
                    };
                }
            }
            repeat(current, 3) {
                print(current);
            }
        "#;
        let output = compile_and_run(src);
        assert_eq!(output, "0\n1\n2\n3");
    }

    #[test]
    fn simplify_macro_pattern_matching() {
        let src = r#"
            def simplify(expr:Number) {
                match(expr) {
                    case (x1:Number + x2:Number) => simplify(x1) + simplify(x2);
                    case (x1:Number + 0) => simplify(x1);
                    case (x1:Number - x2:Number) => simplify(x1) - simplify(x2);
                    case (x1:Number - 0) => simplify(x1);
                    case (x1:Number * x2:Number) => simplify(x1) * simplify(x2);
                    case (x1:Number * 1) => simplify(x1);
                    default => expr;
                };
            }
            print(simplify((42+0)*1));
        "#;
        let output = compile_and_run(src);
        assert_eq!(output, "42");
    }

    #[test]
    fn simplify_macro_default_branch() {
        let src = r#"
            def simplify(expr:Number) {
                match(expr) {
                    case (x1:Number + 0) => simplify(x1);
                    default => expr;
                };
            }
            print(simplify(5));
        "#;
        let output = compile_and_run(src);
        assert_eq!(output, "5");
    }

    #[test]
    fn nested_macro_calls() {
        let src = r#"
            def repeat(n: Number, *expr: Object): Object =>
                let total = n in
                while (total >= 0) {
                    total := total - 1;
                    expr;
                };
            def repeat_twice(n: Number, *expr: Object): Object =>
                repeat(n) { repeat(n) { expr; } };
            repeat_twice(1) { print("x"); }
        "#;
        let output = compile_and_run(src);
        assert_eq!(output, "x\nx\nx\nx");
    }

    #[test]
    fn macro_returning_value() {
        let src = r#"
            def inc(x: Number): Number => x + 1;
            print(inc(41));
        "#;
        let output = compile_and_run(src);
        assert_eq!(output, "42");
    }
}