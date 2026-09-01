//! Pass 3: Type Checking
//!
//! This pass performs a final validation of the fully typed program produced
//! by the type inference pass. It focuses on consistency checks that depend
//! on the complete, resolved type information:
//!
//! - Re‑check all explicit type annotations against their inferred types.
//! - Ensure that protocol conformance is upheld at use sites.
//! - Verify that no unresolved `Unknown` type placeholders remain.
//!
//! The pass only reads the typed tree and the registry; it does not build
//! any new structures.

use hulk_ast::{
    AttributeDecl, Declaration, DeclarationKind, ExprKind, FunctionDecl, LetBinding, SourceSpan,
    TypeDecl, TypeMember, TypeMemberKind, TypeRef,
};

use crate::error::{SemanticError, SemanticErrorKind};
use crate::typed::{TypedExpr, TypedProgram};
use crate::types::registry::TypeRegistry;
use crate::types::Type;

// -----------------------------------------------------------------------------
// Public entry point
// -----------------------------------------------------------------------------

/// Runs type checking on the typed program.
///
/// # Arguments
/// * `typed_program` – The fully typed AST (`Program<Type>`).
/// * `registry` – The registry (read‑only, used for conformance checks).
/// * `errors` – Vector to append any type‑checking errors.
pub fn run(typed_program: &TypedProgram, registry: &TypeRegistry, errors: &mut Vec<SemanticError>) {
    let mut checker = Checker {
        registry,
        errors,
        current_type: None,
    };
    checker.check_program(typed_program);
}

// -----------------------------------------------------------------------------
// Checker state
// -----------------------------------------------------------------------------

/// State for the type checking traversal.
struct Checker<'a> {
    /// Read‑only registry containing all type and signature information.
    registry: &'a TypeRegistry,
    /// Accumulator for diagnostics.
    errors: &'a mut Vec<SemanticError>,
    /// The type currently being checked, used for class-level attribute privacy.
    current_type: Option<Type>,
}

// -----------------------------------------------------------------------------
// Program traversal
// -----------------------------------------------------------------------------

impl<'a> Checker<'a> {
    /// Checks every declaration and the entry expression of the program.
    fn check_program(&mut self, program: &TypedProgram) {
        for decl in &program.declarations {
            self.check_declaration(decl);
        }
        self.check_expr(&program.entry);
    }

    /// Dispatches to the appropriate declaration check.
    fn check_declaration(&mut self, decl: &Declaration<Type>) {
        match &decl.kind {
            DeclarationKind::Function(f) => self.check_function(f),
            DeclarationKind::Type(t) => self.check_type(t),
            // Protocols have no bodies or initializers – nothing to check.
            DeclarationKind::Protocol(_) => {}
            DeclarationKind::Macro(m) => {
                // Macro declarations should have been expanded earlier.
                self.errors.push(SemanticError::error(
                    SemanticErrorKind::MacroReferenceFound {
                        macro_expr: m.name.clone(),
                    },
                    decl.span,
                ));
                // Do not check the body – it is invalid at this stage.
            }
        }
    }

    // -------------------------------------------------------------------------
    // Function / method checks
    // -------------------------------------------------------------------------

    /// Checks a function or method declaration: annotations and body.
    ///
    /// For each parameter and the return type, if an explicit annotation is
    /// present, it is compared against the inferred type stored in the registry.
    /// The body is traversed to detect any leftover `Unknown` types.
    fn check_function(&mut self, func: &FunctionDecl<Type>) {
        // Retrieve the inferred signature from the registry.
        if let Some(sig) = self.registry.lookup_function(&func.name) {
            // Return type annotation, if any.
            if let Some(ann) = &func.return_type {
                let ann_type = self.resolve_type_ref(ann);
                self.check_conformance(&sig.return_type, &ann_type, func.body.span);
            }

            // Parameter annotations.
            for (param, (_, inferred)) in func.params.iter().zip(&sig.params) {
                if let Some(ann) = &param.type_annotation {
                    let ann_type = self.resolve_type_ref(ann);
                    self.check_conformance(inferred, &ann_type, func.body.span);
                }
            }

            // Verify that the body contains no `Unknown` types.
            self.check_expr(&func.body);
        } else {
            // Should never occur if the inference pass is correct.
            panic!("internal: function signature missing for `{}`", func.name);
        }
    }

    /// Checks a type declaration and all its members.
    fn check_type(&mut self, ty_decl: &TypeDecl<Type>) {
        // Crafting Interpreters "currentClass" pattern: save/set/restore so
        // nested types (if ever supported) would be handled correctly.
        let previous_type = self.current_type.take();
        self.current_type = Some(Type::Named(ty_decl.name.clone()));
        for member in &ty_decl.members {
            self.check_type_member(member, &ty_decl.name);
        }
        self.current_type = previous_type;
    }

    /// Checks a type member: attribute initializer or method.
    fn check_type_member(&mut self, member: &TypeMember<Type>, type_name: &str) {
        match &member.kind {
            TypeMemberKind::Attribute(attr) => self.check_attribute(attr, type_name),
            TypeMemberKind::Method(method) => {
                // Retrieve the method's inferred signature from the registry.
                if let Some(type_info) = self.registry.lookup_type(type_name) {
                    if let Some(sig) = type_info.flattened_methods.get(&method.name) {
                        // Return type annotation.
                        if let Some(ann) = &method.return_type {
                            let ann_type = self.resolve_type_ref(ann);
                            self.check_conformance(&sig.return_type, &ann_type, method.body.span);
                        }

                        // Parameter annotations.
                        for (param, (_, inferred)) in method.params.iter().zip(&sig.params) {
                            if let Some(ann) = &param.type_annotation {
                                let ann_type = self.resolve_type_ref(ann);
                                self.check_conformance(inferred, &ann_type, method.body.span);
                            }
                        }

                        // Check the method body.
                        self.check_expr(&method.body);
                    } else {
                        // Should not happen; report an internal error.
                        panic!(
                            "internal: method signature missing for `{}` in type `{}`",
                            method.name, type_name
                        );
                    }
                } else {
                    panic!("internal: type `{}` not found in registry", type_name);
                }
            }
        }
    }

    /// Checks an attribute declaration: annotation vs. initializer type,
    /// and ensures the initializer contains no `Unknown`.
    fn check_attribute(&mut self, attr: &AttributeDecl<Type>, _type_name: &str) {
        if let Some(ann) = &attr.type_annotation {
            let ann_type = self.resolve_type_ref(ann);
            self.check_conformance(&attr.initializer.anno, &ann_type, attr.initializer.span);
        }
        self.check_expr(&attr.initializer);
    }

    // -------------------------------------------------------------------------
    // Expression traversal (for Unknown sweep and attribute privacy)
    // -------------------------------------------------------------------------

    /// Recursively traverses an expression tree to detect any remaining
    /// `Type::Unknown` placeholders, performs attribute privacy checks,
    /// and recursively checks sub‑expressions.
    fn check_expr(&mut self, expr: &TypedExpr) {
        // Report any unresolved type.
        if matches!(expr.anno, Type::Unknown) {
            self.errors.push(SemanticError::error(
                SemanticErrorKind::CannotInferType {
                    symbol: "expression".to_string(),
                },
                expr.span,
            ));
        }

        // Recurse into children based on the expression kind.
        match &expr.kind {
            ExprKind::Literal(_)
            | ExprKind::Variable(_)
            | ExprKind::SelfRef
            | ExprKind::BaseRef => {}

            ExprKind::Unary(unary) => self.check_expr(&unary.expr),
            ExprKind::Binary(binary) => {
                self.check_expr(&binary.left);
                self.check_expr(&binary.right);
            }

            ExprKind::Let(let_expr) => {
                for binding in &let_expr.bindings {
                    self.check_let_binding(binding);
                }
                self.check_expr(&let_expr.body);
            }

            ExprKind::Assign(assign) => {
                self.check_assign_target(&assign.target);
                self.check_expr(&assign.value);
            }

            ExprKind::Block(block) => {
                for e in &block.expressions {
                    self.check_expr(e);
                }
            }

            ExprKind::If(if_expr) => {
                self.check_expr(&if_expr.condition);
                self.check_expr(&if_expr.then_branch);
                for elif in &if_expr.elif_branches {
                    self.check_expr(&elif.condition);
                    self.check_expr(&elif.body);
                }
                self.check_expr(&if_expr.else_branch);
            }

            ExprKind::While(while_expr) => {
                self.check_expr(&while_expr.condition);
                self.check_expr(&while_expr.body);
            }

            ExprKind::For(for_expr) => {
                self.check_expr(&for_expr.iterable);
                self.check_expr(&for_expr.body);
            }

            ExprKind::Call(call) => {
                self.check_expr(&call.callee);
                for arg in &call.args {
                    self.check_expr(arg);
                }
            }

            ExprKind::Lambda(lambda) => self.check_expr(&lambda.body),

            ExprKind::Member(member) => {
                // Recurse into the object (also checks for Unknown).
                self.check_expr(&member.object);

                // Attribute privacy check.
                if let Some(owner_type) = self.attribute_owner(&member.object.anno, &member.member)
                {
                    // It's an attribute: only allowed if object is `self` of the exact same type.
                    match &member.object.kind {
                        ExprKind::SelfRef => {
                            let self_type = &member.object.anno;
                            if self_type != &owner_type {
                                self.errors.push(SemanticError::error(
                                    SemanticErrorKind::UnknownMember {
                                        ty: owner_type,
                                        member: member.member.clone(),
                                    },
                                    member.object.span,
                                ));
                            }
                        }
                        _ => {
                            // WHY: HULK spec §A.7.1 says "outside the type". Java, C++, and
                            // Kotlin allow same-type instances to access each other's private
                            // members (access is class-level, not object-level).
                            // Crafting Interpreters uses the same currentClass pattern.
                            let is_same_type_access =
                                self.current_type.as_ref() == Some(&owner_type);
                            if !is_same_type_access {
                                self.errors.push(SemanticError::error(
                                    SemanticErrorKind::UnknownMember {
                                        ty: owner_type,
                                        member: member.member.clone(),
                                    },
                                    member.object.span,
                                ));
                            }
                        }
                    }
                }
                // If the member is not an attribute, it's a method (public, no restriction).
            }

            ExprKind::New(new_expr) => {
                for arg in &new_expr.args {
                    self.check_expr(arg);
                }
            }

            ExprKind::TypeTest(type_test) => self.check_expr(&type_test.expr),
            ExprKind::Downcast(downcast) => self.check_expr(&downcast.expr),

            ExprKind::Vector(vector) => match vector {
                hulk_ast::VectorExpr::Literal(items) => {
                    for item in items {
                        self.check_expr(item);
                    }
                }
                hulk_ast::VectorExpr::Comprehension(comp) => {
                    self.check_expr(&comp.expr);
                    self.check_expr(&comp.iterable);
                }
            },

            ExprKind::Index(index) => {
                self.check_expr(&index.object);
                self.check_expr(&index.index);
            }

            ExprKind::Match(match_expr) => {
                self.check_expr(&match_expr.value);
                for case in &match_expr.cases {
                    self.check_expr(&case.body);
                }
            }

            ExprKind::MacroCall(mc) => {
                // Should never be reached if hulk-transpile ran before the semantic pass.
                self.errors.push(SemanticError::error(
                    SemanticErrorKind::MacroReferenceFound {
                        macro_expr: mc.name.clone(),
                    },
                    expr.span,
                ));
                // Do not recurse into arguments or body – they are invalid at this stage.
            }
            ExprKind::MacroMatch(_mm) => {
                // Should never be reached if hulk-transpile ran before the semantic pass.
                self.errors.push(SemanticError::error(
                    SemanticErrorKind::MacroReferenceFound {
                        macro_expr: "Match expression".to_string(),
                    },
                    expr.span,
                ));
                // Do not recurse into arguments or body – they are invalid at this stage.
            }
        }
    }

    /// Checks a `let` binding: annotation vs. initializer, and recurses
    /// into the initializer.
    fn check_let_binding(&mut self, binding: &LetBinding<Type>) {
        if let Some(ann) = &binding.type_annotation {
            let ann_type = self.resolve_type_ref(ann);
            self.check_conformance(
                &binding.initializer.anno,
                &ann_type,
                binding.initializer.span,
            );
        }
        self.check_expr(&binding.initializer);
    }

    /// Checks an assignment target for nested expressions.
    fn check_assign_target(&mut self, target: &hulk_ast::AssignTarget<Type>) {
        match target {
            hulk_ast::AssignTarget::Variable(_) => {}
            hulk_ast::AssignTarget::Member { object, .. } => self.check_expr(object),
            hulk_ast::AssignTarget::Index { object, index } => {
                self.check_expr(object);
                self.check_expr(index);
            }
        }
    }

    // -------------------------------------------------------------------------
    // Conformance check helper
    // -------------------------------------------------------------------------

    /// Verifies that the actual type conforms to the expected type.
    /// If not, and expected is a protocol and actual is Named, try
    /// to get detailed missing list. Otherwise appends a `NotConforming`
    /// error at the given span.
    fn check_conformance(&mut self, actual_type: &Type, expected_type: &Type, span: SourceSpan) {
        if actual_type.conforms_to(expected_type, self.registry) {
            return;
        }
        // If expected is a protocol and actual is Named, try to get detailed missing list.
        if let Type::Named(actual_name) = actual_type {
            if let Type::Named(protocol_name) = expected_type {
                if self.registry.is_protocol(expected_type) {
                    if let Err(missing) = self
                        .registry
                        .protocol_conformance_details(actual_name, protocol_name)
                    {
                        self.errors.push(SemanticError::error(
                            SemanticErrorKind::ProtocolNotImplemented {
                                ty: actual_type.clone(),
                                protocol: protocol_name.clone(),
                                missing,
                            },
                            span,
                        ));
                        return;
                    }
                }
            }
        }
        // Fallback to generic NotConforming.
        self.errors.push(SemanticError::error(
            SemanticErrorKind::NotConforming {
                found: actual_type.clone(),
                expected: expected_type.clone(),
            },
            span,
        ));
    }

    // -------------------------------------------------------------------------
    // Attribute privacy helper
    // -------------------------------------------------------------------------

    /// Returns the owner type if `member` is an attribute of `ty`.
    /// Otherwise returns `None` (the member is a method or does not exist).
    fn attribute_owner(&self, ty: &Type, member: &str) -> Option<Type> {
        match ty {
            Type::Named(name) => self
                .registry
                .lookup_type(name)
                .and_then(|info| info.attributes.get(member))
                .map(|_| Type::Named(name.clone())),
            _ => None,
        }
    }

    // -------------------------------------------------------------------------
    // Helper: resolve TypeRef to Type
    // -------------------------------------------------------------------------

    /// Converts a syntactic type reference (`TypeRef`) to a semantic `Type`.
    /// This mirrors the same logic used during type inference.
    fn resolve_type_ref(&self, tr: &TypeRef) -> Type {
        match tr.name.as_str() {
            "Number" => Type::Number,
            "String" => Type::String,
            "Boolean" => Type::Boolean,
            "Object" => Type::Object,
            _ => {
                if tr.args.is_empty() {
                    Type::Named(tr.name.clone())
                } else {
                    let args: Vec<Type> = tr
                        .args
                        .iter()
                        .map(|arg| self.resolve_type_ref(arg))
                        .collect();
                    match tr.name.as_str() {
                        "Vector" if !args.is_empty() => Type::Vector(Box::new(args[0].clone())),
                        "Iterable" if !args.is_empty() => Type::Iterable(Box::new(args[0].clone())),
                        "Function" if !args.is_empty() => {
                            let return_type = args.last().cloned().unwrap_or(Type::Object);
                            let params = args[..args.len() - 1].to_vec();
                            Type::Function {
                                params,
                                return_type: Box::new(return_type),
                            }
                        }
                        _ => Type::Named(tr.name.clone()),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze;
    use hulk_ast::VectorExpr;
    use hulk_lexer::Lexer;
    use hulk_parser::parse;

    #[test]
    fn annotation_mismatch() {
        let src = "let x: Number = \"hello\" in print(x);";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_err());
        let errs = result.err().unwrap();
        assert!(errs
            .iter()
            .any(|e| matches!(e.kind, SemanticErrorKind::NotConforming { .. })));
    }

    #[test]
    fn protocol_conformance_use() {
        let src = "
            protocol P { f(): Number; }
            type T { f(): Number => 42; }
            let x: P = new T() in print(x.f());
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok(), "protocol conformance should work");
    }

    /// Tests that a protocol conformance failure due to contravariance violation
    /// produces a ProtocolNotImplemented error with the offending method name in
    /// the missing list.
    #[test]
    fn protocol_conformance_variance_violation() {
        // Protocol P requires f(x: Object): Number; type T provides f(x: Number): Number.
        // For contravariance, the protocol parameter type (Object) must conform to the
        // type parameter type (Number). Object does not conform to Number, so this is
        // a violation.
        let src = "
            protocol P { f(x: Object): Number; }
            type T { f(x: Number): Number => x; }
            let x: P = new T() in print(x.f(1));
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        let errors = result.expect_err("should fail due to variance violation");

        // Look for a ProtocolNotImplemented error mentioning method 'f'.
        assert!(
            errors.iter().any(|e| {
                matches!(
                    &e.kind,
                    SemanticErrorKind::ProtocolNotImplemented { missing, .. }
                    if missing.iter().any(|m| m == "f")
                )
            }),
            "Expected ProtocolNotImplemented error with missing method 'f'"
        );
    }

    /// Tests that when a type is missing a protocol method, the error's missing
    /// list contains the method name, providing a detailed diagnostic.
    #[test]
    fn protocol_conformance_missing_method_detailed() {
        let src = "
            protocol P { f(): Number; g(): String; }
            type T { f(): Number => 42; }  // missing g
            let x: P = new T() in print(x.g());
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        let errors = result.expect_err("should fail due to missing method");

        // Look for ProtocolNotImplemented with missing list containing "g".
        assert!(
            errors.iter().any(|e| {
                matches!(
                    &e.kind,
                    SemanticErrorKind::ProtocolNotImplemented { ty, protocol, missing }
                    if ty == &Type::Named("T".to_string())
                        && protocol == &"P".to_string()
                        && missing.len() == 1
                        && missing[0] == "g"
                )
            }),
            "Expected ProtocolNotImplemented with missing method 'g'"
        );
    }

    /// Tests that a covariance violation (return type too specific) is also
    /// caught and reported as a ProtocolNotImplemented error with the method name.
    #[test]
    fn protocol_conformance_covariance_violation() {
        let src = "
            protocol P { f(): Number; }
            type T { f(): String => \"hello\"; }
            let x: P = new T() in print(x.f());
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        let errors = result.expect_err("should fail due to return type covariance violation");

        assert!(
            errors.iter().any(|e| {
                matches!(
                    &e.kind,
                    SemanticErrorKind::ProtocolNotImplemented { missing, .. }
                    if missing.iter().any(|m| m == "f")
                )
            }),
            "Expected ProtocolNotImplemented error with missing method 'f' for covariance violation"
        );
    }

    /// Tests that the `Unknown` sweep detects nested recursive call nodes that were
    /// patched incorrectly. This directly inspects the typed program after inference
    /// to ensure the recursive call node (callee = Variable("fib")) has its `anno`
    /// set to `Number`, not `Unknown`.
    #[test]
    fn unknown_sweep_finds_nested_callee_after_patch() {
        let src = "
            function fib(n) => if (n == 0 | n == 1) 1 else fib(n-1) + fib(n-2);
            print(fib(10));
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok(), "inference should succeed");

        let typed_program = result.unwrap().typed_program;
        let fib_decl = &typed_program.declarations[0];
        let fib_body = match &fib_decl.kind {
            DeclarationKind::Function(f) => &f.body,
            _ => panic!("expected function declaration"),
        };

        // Recursively search for a Call with callee Variable("fib").
        fn find_recursive_call(expr: &TypedExpr) -> Option<&TypedExpr> {
            match &expr.kind {
                ExprKind::Call(call) => {
                    if let ExprKind::Variable(name) = &call.callee.kind {
                        if name == "fib" {
                            return Some(expr);
                        }
                    }
                    // Recurse into children.
                    if let Some(found) = find_recursive_call(&call.callee) {
                        return Some(found);
                    }
                    for arg in &call.args {
                        if let Some(found) = find_recursive_call(arg) {
                            return Some(found);
                        }
                    }
                    None
                }
                // For other nodes, recurse normally.
                ExprKind::Unary(unary) => find_recursive_call(&unary.expr),
                ExprKind::Binary(binary) => {
                    find_recursive_call(&binary.left).or_else(|| find_recursive_call(&binary.right))
                }
                ExprKind::If(if_expr) => find_recursive_call(&if_expr.condition)
                    .or_else(|| find_recursive_call(&if_expr.then_branch))
                    .or_else(|| {
                        for elif in &if_expr.elif_branches {
                            if let Some(found) = find_recursive_call(&elif.condition) {
                                return Some(found);
                            }
                            if let Some(found) = find_recursive_call(&elif.body) {
                                return Some(found);
                            }
                        }
                        find_recursive_call(&if_expr.else_branch)
                    }),
                ExprKind::Let(let_expr) => {
                    for binding in &let_expr.bindings {
                        if let Some(found) = find_recursive_call(&binding.initializer) {
                            return Some(found);
                        }
                    }
                    find_recursive_call(&let_expr.body)
                }
                ExprKind::Block(block) => {
                    for e in &block.expressions {
                        if let Some(found) = find_recursive_call(e) {
                            return Some(found);
                        }
                    }
                    None
                }
                ExprKind::While(while_expr) => find_recursive_call(&while_expr.condition)
                    .or_else(|| find_recursive_call(&while_expr.body)),
                ExprKind::For(for_expr) => find_recursive_call(&for_expr.iterable)
                    .or_else(|| find_recursive_call(&for_expr.body)),
                ExprKind::Member(member) => find_recursive_call(&member.object),
                ExprKind::Lambda(lambda) => find_recursive_call(&lambda.body),
                ExprKind::New(new_expr) => {
                    for arg in &new_expr.args {
                        if let Some(found) = find_recursive_call(arg) {
                            return Some(found);
                        }
                    }
                    None
                }
                ExprKind::TypeTest(type_test) => find_recursive_call(&type_test.expr),
                ExprKind::Downcast(downcast) => find_recursive_call(&downcast.expr),
                ExprKind::Vector(vector) => match vector {
                    VectorExpr::Literal(items) => {
                        for item in items {
                            if let Some(found) = find_recursive_call(item) {
                                return Some(found);
                            }
                        }
                        None
                    }
                    VectorExpr::Comprehension(comp) => find_recursive_call(&comp.expr)
                        .or_else(|| find_recursive_call(&comp.iterable)),
                },
                ExprKind::Index(index) => {
                    find_recursive_call(&index.object).or_else(|| find_recursive_call(&index.index))
                }
                ExprKind::Match(match_expr) => {
                    find_recursive_call(&match_expr.value).or_else(|| {
                        for case in &match_expr.cases {
                            if let Some(found) = find_recursive_call(&case.body) {
                                return Some(found);
                            }
                        }
                        None
                    })
                }
                // Leaves: Literal, Variable, SelfRef, BaseRef have no children.
                _ => None,
            }
        }

        let recursive_call =
            find_recursive_call(fib_body).expect("should find a recursive call to fib");

        // The recursive call should have its annotation resolved to Number.
        assert_eq!(
            recursive_call.anno,
            Type::Number,
            "recursive call annotation should be Number, got {:?}",
            recursive_call.anno
        );
    }

    /// Tests that a method declared on an ancestor protocol can be called through
    /// a variable typed as a descendant protocol.
    #[test]
    fn protocol_method_call_through_two_protocols() {
        let src = "
            protocol P { f(): Number; }
            protocol Q extends P { g(): Number; }
            type T { f(): Number => 1; g(): Number => 2; }
            let x: Q = new T() in print(x.f() + x.g());
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(
            result.is_ok(),
            "protocol ancestor method call should work: {:?}",
            result.err()
        );
    }

    /// Tests that attribute access through a variable typed as a protocol is rejected
    /// (since protocols never expose attributes). This guards against a future change
    /// that might accidentally allow attribute lookup through protocol-typed variables.
    #[test]
    fn attribute_privacy_violation_through_protocol_view() {
        let src = "
            type T { attr = 42; }
            protocol P { }
            let x: P = new T() in print(x.attr);
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(
            result.is_err(),
            "attribute access through protocol should fail"
        );
        let errors = result.err().unwrap();
        // Expect an UnknownMember error (or a privacy violation).
        assert!(
            errors
                .iter()
                .any(|e| matches!(e.kind, SemanticErrorKind::UnknownMember { .. })),
            "missing UnknownMember error; got errors: {:?}",
            errors
        );
    }
}
