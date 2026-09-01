//! Lexical scoping environment for HULK.
//!
//! This module implements the inherited attribute: a stack of scopes
//! (hash maps from variable names to their types and source locations)
//! that is threaded top‑down through every visitor during type inference
//! and checking.
//!
//! The environment is used by Pass 2 (inference) and Pass 3 (checking) to
//! resolve variable references and to track which names are in scope.
//! It is not persisted between passes; a fresh `Environment` is created
//! for each traversal.

use indexmap::IndexMap;

use hulk_ast::SourceSpan;

use crate::types::Type;

// -----------------------------------------------------------------------------
// Binding
// -----------------------------------------------------------------------------

/// A variable binding in the environment: its type, source location, and whether it is the original `self`.
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    /// The resolved type of the variable.
    pub ty: Type,
    /// The source location where the variable was declared.
    pub span: SourceSpan,
    /// Variable is the original `self`.
    pub is_self: bool,
}

impl Binding {
    /// Creates a new binding with `is_self = false`.
    pub fn new(ty: Type, span: SourceSpan) -> Self {
        Self {
            ty,
            span,
            is_self: false,
        }
    }

    /// Creates a new binding with a custom `is_self` flag.
    pub fn with_self(ty: Type, span: SourceSpan, is_self: bool) -> Self {
        Self { ty, span, is_self }
    }
}

// -----------------------------------------------------------------------------
// Environment
// -----------------------------------------------------------------------------

/// A stack of scopes, each mapping variable names to their bindings.
///
/// This is the inherited attribute passed down through the AST visitors.
/// It is created anew for each pass that needs name resolution.
///
/// # Scope discipline
/// - `push_scope` / `pop_scope` are called around every construct that
///   introduces a new lexical scope: `let` body, function/method body,
///   `for` loop body, and `match` case body.
/// - Plain `{ ... }` blocks do **not** push a scope — they are pure
///   sequencing and do not affect name resolution.
#[derive(Debug, Clone)]
pub struct Environment {
    scopes: Vec<IndexMap<String, Binding>>,
}

impl Environment {
    /// Creates a new environment with one root scope (initially empty).
    ///
    /// The root scope exists so that `declare` always has a scope to insert into.
    /// HULK has no global variables, so the root scope remains empty in practice.
    pub fn new() -> Self {
        Self {
            scopes: vec![IndexMap::new()],
        }
    }

    /// Pushes a new empty scope onto the stack.
    ///
    /// Must be paired with a subsequent `pop_scope`.
    pub fn push_scope(&mut self) {
        self.scopes.push(IndexMap::new());
    }

    /// Pops the innermost scope, discarding all its bindings.
    ///
    /// Panics if called when there is only one scope left (the root).
    /// This prevents accidental removal of the root scope.
    pub fn pop_scope(&mut self) {
        if self.scopes.len() <= 1 {
            panic!("cannot pop the root scope");
        }
        self.scopes.pop();
    }

    /// Defaultly declares a variable in the innermost scope.
    ///
    /// If a binding with the same name already exists in that scope, it is
    /// overwritten (rebinding). This is intentional and matches HULK's
    /// rule that `let a = 7, a = 7*6 in ...` is valid. This function does not
    /// check for duplicates.
    pub fn declare(&mut self, name: &str, ty: Type, span: SourceSpan) {
        self.declare_with_self(name, ty, span, false);
    }

    /// Declares a variable in the innermost scope, allowing to specify whether
    /// it is the original `self` for `Binding` creation.
    ///
    /// If a binding with the same name already exists in that scope, it is
    /// overwritten (rebinding). This is intentional and matches HULK's
    /// rule that `let a = 7, a = 7*6 in ...` is valid. This function does not
    /// check for duplicates.
    pub fn declare_with_self(&mut self, name: &str, ty: Type, span: SourceSpan, is_self: bool) {
        let scope = self.scopes.last_mut().expect("at least one scope exists");
        scope.insert(name.to_string(), Binding { ty, span, is_self });
    }

    /// Looks up a variable name starting from the innermost scope outward.
    ///
    /// Returns the first matching `Binding` if found, or `None` if the name
    /// is not bound in any scope.
    ///
    /// This implements lexical shadowing exactly as described in §A.4.5:
    /// an inner binding hides an outer one.
    pub fn lookup(&self, name: &str) -> Option<&Binding> {
        for scope in self.scopes.iter().rev() {
            if let Some(binding) = scope.get(name) {
                return Some(binding);
            }
        }
        None
    }

    /// Returns `true` if the name is declared in any scope.
    ///
    /// This is a convenience wrapper around `lookup`, useful for existence checks.
    pub fn is_declared(&self, name: &str) -> bool {
        self.lookup(name).is_some()
    }

    /// Returns the current nesting depth (number of scopes).
    pub fn depth(&self) -> usize {
        self.scopes.len()
    }
}

impl Default for Environment {
    fn default() -> Self {
        Self::new()
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_span() -> SourceSpan {
        SourceSpan::new(0, 0)
    }

    #[test]
    fn shadowing_within_same_scope() {
        let mut env = Environment::new();
        // Declare `a` as Number, then overwrite with String in the same scope.
        env.declare("a", Type::Number, dummy_span());
        env.declare("a", Type::String, dummy_span());

        let binding = env.lookup("a").expect("a should be bound");
        assert_eq!(binding.ty, Type::String);
    }

    #[test]
    fn shadowing_across_nested_scopes() {
        let mut env = Environment::new();
        // Outer scope: a = Number.
        env.declare("a", Type::Number, dummy_span());
        // Push new scope, declare a = String.
        env.push_scope();
        env.declare("a", Type::String, dummy_span());

        // Lookup in nested scope should see String.
        let binding = env.lookup("a").expect("a should be bound");
        assert_eq!(binding.ty, Type::String);

        // Pop scope, lookup should see outer Number.
        env.pop_scope();
        let binding = env.lookup("a").expect("a should be bound");
        assert_eq!(binding.ty, Type::Number);
    }

    #[test]
    #[should_panic(expected = "cannot pop the root scope")]
    fn pop_root_scope_panics() {
        let mut env = Environment::new();
        // There is only the root scope. Popping it should panic.
        env.pop_scope();
    }

    #[test]
    fn is_self_flag_preserved_through_lookup() {
        let mut env = Environment::new();
        // Declare `self` with is_self = true.
        env.declare_with_self("self", Type::Named("A".to_string()), dummy_span(), true);

        let binding = env.lookup("self").expect("self should be bound");
        assert!(binding.is_self, "is_self flag should be true");
        assert_eq!(binding.ty, Type::Named("A".to_string()));
    }
}
