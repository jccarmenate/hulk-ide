//! HULK type representation and core operations.
//!
//! This module defines the `Type` enum (the synthesized attribute of every
//! expression) and the fundamental relations used by the semantic analyzer:
//! conformance (`<=`) and lowest‑common‑ancestor (LCA) for multi‑branch
//! constructs.
//!
//! All types are self‑contained values with no lifetimes or AST references,
//! so they can be stored, compared, and returned cheaply.

use std::collections::HashSet;
use std::fmt;

pub mod registry;
pub use registry::{seeded_registry, TypeInfo, TypeRegistry};

// -----------------------------------------------------------------------------
// Type enum
// -----------------------------------------------------------------------------

/// A fully‑resolved HULK type, as computed by the semantic analyzer.
///
/// This is the synthesized attribute of every expression visit (Section 2.2
/// of the implementation plan). It carries no lifetime or AST reference so it
/// can be stored in maps, returned up the tree, and compared cheaply.
///
/// # Invariants
/// - `Unknown` and `Error` are internal placeholders. `Unknown` is used
///   only during inference (e.g., for self‑recursive functions) and must
///   never survive into a successfully `analyze`d program. `Error` is a
///   poison value that suppresses cascading diagnostics after an error
///   has already been reported for a node.
/// - All user‑defined types (classes and protocols) are represented as
///   `Named(String)`. They share one namespace, because both can appear
///   in annotation position (hulk‑docs §A.10.2).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    /// Number builtin value type
    Number,
    /// String builtin value type
    String,
    /// Boolean builtin value type
    Boolean,

    /// Root of the nominal hierarchy; every type conforms to it.
    Object,

    /// A user‑defined `type` or `protocol`, resolved by name through
    /// the `TypeRegistry`.
    Named(String),

    /// `Vector<T>` — both the `T[]` annotation sugar (§A.12.3) and the
    /// type of vector literals / comprehensions.
    Vector(Box<Type>),

    /// `Iterable<T>` — the `T*` annotation sugar (§A.11.2) and the
    /// builtin `Iterable` protocol specialized to an element type.
    Iterable(Box<Type>),

    /// Function type: (params) -> return_type.
    /// Used for method references and future lambda expressions.
    Function {
        /// Parameter types (in order)
        params: Vec<Type>,
        /// Return type
        return_type: Box<Type>,
    },

    /// Internal placeholder used while a symbol's type is still being
    /// inferred (e.g., a self‑recursive function). Never appears in a
    /// successfully verified program.
    Unknown,

    /// Poison value produced after a type error has already been
    /// reported for this expression, so that the error does not cascade
    /// into a flood of unrelated follow‑up errors.
    Error,
}

// -----------------------------------------------------------------------------
// Conformance (≤)
// -----------------------------------------------------------------------------

impl Type {
    /// Returns `true` if a value of `self`'s type can be used wherever
    /// a value of `other`'s type is expected.
    ///
    /// This implements the `<=` relation from hulk‑docs §A.8.4, with the
    /// addition of cascade suppression for `Error` and inference support
    /// for `Unknown`.
    ///
    /// # Rules (in priority order)
    /// 1. Reflexivity: `self == other` → `true`.
    /// 2. Everything conforms to `Object`.
    /// 3. Cascade suppression: if either side is `Type::Error`, return `true`.
    /// 4. `Unknown` conforms to everything (and vice versa) – this allows
    ///    inference to proceed with placeholders.
    /// 5. Nominal inheritance: if both are `Named`, return `true` iff
    ///    `other` is an ancestor of `self` in the single‑inheritance tree
    ///    **or** `self`'s type structurally implements the protocol named
    ///    by `other`.
    /// 6. Protocol conformance: if `self` is `Named` and `other` is a
    ///    protocol name, delegate to `registry.implements_protocol`.
    /// 7. `Iterable` conformance and covariance:
    ///    - `Named(T)` ≤ `Iterable(U)` if `T` implements `Iterable` and
    ///      `current(): R` with `R ≤ U`.
    ///    - `Vector(T)` ≤ `Iterable(U)` if `T ≤ U`.
    ///    - `Iterable(T)` ≤ `Iterable(U)` if `T ≤ U`.
    /// 8. `Vector` covariance: `Vector(T)` ≤ `Vector(U)` if `T ≤ U`.
    /// 9. Function types: `(A1, A2, ...) -> R1` ≤ `(B1, B2, ...) -> R2` if
    ///    - the parameter lists are the same length, and
    ///    - each parameter in `self` conforms to the corresponding parameter in `other`.
    /// 10. Otherwise: return `false` (no implicit numeric widening in HULK).
    pub fn conforms_to(&self, other: &Self, registry: &TypeRegistry) -> bool {
        // 1. Reflexivity
        if self == other {
            return true;
        }

        // 2. Everything conforms to Object
        if matches!(other, Type::Object) {
            return true;
        }

        // 3. Cascade suppression
        if matches!(self, Type::Error) || matches!(other, Type::Error) {
            return true;
        }

        // 4. Unknown is a placeholder that conforms to everything (and vice versa)
        if matches!(self, Type::Unknown) || matches!(other, Type::Unknown) {
            return true;
        }

        // 5. Nominal inheritance or protocol implementation (both Named)
        if let (Type::Named(t1), Type::Named(t2)) = (self, other) {
            // a) nominal ancestor
            if registry.is_ancestor(t2, t1) {
                return true;
            }
            // b) structural protocol conformance
            if registry.implements_protocol(t1, t2) {
                return true;
            }
        }

        // 6. Self is Named and other is a protocol (structural conformance)
        if let Type::Named(t1) = self {
            if registry.is_protocol(other) {
                if let Type::Named(t2) = other {
                    if registry.implements_protocol(t1, t2) {
                        return true;
                    }
                }
            }
        }

        // 7. Conformance to Iterable<T> (includes Iterable covariance)
        if let Type::Iterable(expected_inner) = other {
            return match self {
                Type::Named(t1) => {
                    // Primary path: use the registry's structural protocol conformance check.
                    if registry.implements_protocol(t1, "Iterable") {
                        if let Some(info) = registry.lookup_type(t1) {
                            if let Some(sig) = info.flattened_methods.get("current") {
                                if sig.return_type.conforms_to(expected_inner, registry) {
                                    return true;
                                }
                            }
                        }
                    }
                    // Fallback: directly verify the type has both required methods.
                    // This handles cases where the protocol's method table might be incomplete.
                    if let Some(info) = registry.lookup_type(t1) {
                        let has_next = info
                            .flattened_methods
                            .get("next")
                            .map(|sig| sig.params.is_empty() && sig.return_type == Type::Boolean)
                            .unwrap_or(false);
                        if has_next {
                            if let Some(sig) = info.flattened_methods.get("current") {
                                if sig.params.is_empty() {
                                    return sig.return_type.conforms_to(expected_inner, registry);
                                }
                            }
                        }
                    }
                    false
                }
                Type::Vector(inner) => {
                    // Vector<T> implements Iterable with current returning T.
                    inner.conforms_to(expected_inner, registry)
                }
                Type::Iterable(inner) => {
                    // Iterable<T> covariance.
                    inner.conforms_to(expected_inner, registry)
                }
                _ => false,
            };
        }

        // 8. Vector covariance: Vector<T> ≤ Vector<U> if T ≤ U
        if let Type::Vector(expected_inner) = other {
            if let Type::Vector(inner) = self {
                return inner.conforms_to(expected_inner, registry);
            }
            return false;
        }

        // 9. Function types: contravariant params, covariant return.
        if let (
            Type::Function {
                params: p1,
                return_type: r1,
            },
            Type::Function {
                params: p2,
                return_type: r2,
            },
        ) = (self, other)
        {
            if p1.len() != p2.len() {
                return false;
            }
            for (a, b) in p1.iter().zip(p2) {
                if !b.conforms_to(a, registry) {
                    return false;
                } // contravariant
            }
            return r1.conforms_to(r2, registry); // covariant
        }

        // 10. Fallback: no conformance
        false
    }
}

// Type enum Display formatting for error messages and debugging.
impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Number => write!(f, "Number"),
            Type::String => write!(f, "String"),
            Type::Boolean => write!(f, "Boolean"),
            Type::Object => write!(f, "Object"),
            Type::Named(name) => write!(f, "{}", name),
            Type::Vector(inner) => write!(f, "Vector<{}>", inner),
            Type::Iterable(inner) => write!(f, "Iterable<{}>", inner),
            Type::Function {
                params,
                return_type,
            } => {
                let param_str = params
                    .iter()
                    .map(|t| t.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "({}) -> {}", param_str, return_type)
            }
            Type::Unknown => write!(f, "unknown"),
            Type::Error => write!(f, "error"),
        }
    }
}

// -----------------------------------------------------------------------------
// Lowest Common Ancestor (LCA)
// -----------------------------------------------------------------------------

/// Returns the lowest common ancestor (deepest type that is an ancestor
/// of every type in `types`) according to the nominal hierarchy.
///
/// This implements the unification rule for multi‑branch constructs
/// (§A.9.2): `if`/`elif`/`else`, vector literals, and `match` cases.
///
/// # Behaviour
/// - If the slice is empty, returns `Type::Error`.
/// - If any type is `Type::Error`, returns `Type::Error` (propagate).
/// - If all types are `Type::Unknown`, returns `Type::Unknown`.
/// - Otherwise, walks each type's ancestor chain up to `Object` and
///   returns the deepest node common to all chains, ignoring `Unknown`
///   types for the purpose of finding a concrete LCA.
pub fn lowest_common_ancestor(types: &[Type], registry: &TypeRegistry) -> Type {
    if types.is_empty() {
        return Type::Error;
    }

    // Propagate Error
    if types.iter().any(|t| matches!(t, Type::Error)) {
        return Type::Error;
    }

    // Filter out Unknown types; if all are Unknown, return Unknown.
    let concrete: Vec<&Type> = types
        .iter()
        .filter(|t| !matches!(t, Type::Unknown))
        .collect();
    if concrete.is_empty() {
        return Type::Unknown;
    }

    // Collect ancestor chains for each concrete type (including the type itself)
    let mut chains: Vec<Vec<Type>> = Vec::new();
    for ty in concrete {
        let chain = ancestor_chain(ty, registry);
        if chain.is_empty() {
            // Should not happen, but if it does, return Error.
            return Type::Error;
        }
        chains.push(chain);
    }

    // Start with the first chain's ancestors, find the deepest one that appears in all chains.
    let first_chain = &chains[0];
    for candidate in first_chain {
        let mut common = true;
        for other_chain in &chains[1..] {
            if !other_chain.contains(candidate) {
                common = false;
                break;
            }
        }
        if common {
            return candidate.clone();
        }
    }

    // Fallback: should never reach here because Object is common to all.
    Type::Object
}

/// Returns the ancestor chain of `ty`, starting with `ty` itself and
/// ending with `Object`. For `Named` types, the chain is built by walking
/// the parent links in `registry`. For builtins, the chain is just
/// `[ty, Object]` (or `[Object]` if `ty` is already `Object`).
fn ancestor_chain(ty: &Type, registry: &TypeRegistry) -> Vec<Type> {
    match ty {
        Type::Object => vec![Type::Object],
        Type::Number | Type::String | Type::Boolean => {
            // Builtins implicitly inherit Object (§A.7.3)
            vec![ty.clone(), Type::Object]
        }
        Type::Named(name) => {
            let mut chain = Vec::new();
            let mut current = name.clone();
            // Avoid infinite loops (though cycles should have been caught in Pass 1)
            let mut visited = HashSet::new();
            while !visited.contains(&current) {
                visited.insert(current.clone());
                chain.push(Type::Named(current.clone()));
                // Get parent name from registry
                if let Some(parent) = registry.parent_of(&current) {
                    current = parent;
                } else {
                    // No more parent -> reached Object implicitly
                    break;
                }
            }
            // Add Object if not already present
            if !chain.contains(&Type::Object) {
                chain.push(Type::Object);
            }
            chain
        }
        Type::Vector(_inner) => {
            // Vector is a builtin type, so it inherits from Object
            vec![ty.clone(), Type::Object]
        }

        // In practice never used as branch types in HULK.
        // If we do encounter them, we just return [ty, Object].
        Type::Iterable(_inner) => {
            vec![ty.clone(), Type::Object]
        }
        Type::Function {
            params: _params,
            return_type: _return_type,
        } => {
            vec![ty.clone(), Type::Object]
        }

        Type::Unknown | Type::Error => {
            // Should never be asked for an ancestor chain of these.
            vec![]
        }
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::registry::seeded_registry;
    use crate::types::registry::{MethodSignature, ParentLink, ProtocolInfo, TypeInfo};
    use hulk_ast::SourceSpan;
    use indexmap::IndexMap;

    #[test]
    fn conforms_to_reflexivity() {
        let registry = seeded_registry();
        let t = Type::Number;
        assert!(t.conforms_to(&t, &registry));
    }

    #[test]
    fn conforms_to_object() {
        let registry = seeded_registry();
        let t = Type::Number;
        assert!(t.conforms_to(&Type::Object, &registry));
    }

    #[test]
    fn conforms_to_unknown() {
        let registry = seeded_registry();
        let t = Type::Unknown;
        assert!(t.conforms_to(&Type::Number, &registry));
        assert!(Type::Number.conforms_to(&t, &registry));
    }

    #[test]
    fn lca_simple() {
        let registry = seeded_registry();
        let types = vec![Type::Number, Type::String];
        let lca = lowest_common_ancestor(&types, &registry);
        assert_eq!(lca, Type::Object);
    }

    #[test]
    fn lca_with_unknown_filter() {
        let registry = seeded_registry();
        let types = vec![Type::Unknown, Type::Number];
        let lca = lowest_common_ancestor(&types, &registry);
        assert_eq!(lca, Type::Number); // Unknown filtered out
    }

    #[test]
    fn lca_empty_returns_error() {
        let registry = seeded_registry();
        let types = vec![];
        let lca = lowest_common_ancestor(&types, &registry);
        assert_eq!(lca, Type::Error);
    }

    /// Tests structural protocol conformance: a type that implements all protocol
    /// methods should conform to that protocol.
    #[test]
    fn conforms_to_protocol_structural() {
        let mut registry = seeded_registry();

        // Insert protocol P with method f(): Number.
        let p_methods = IndexMap::from([(
            "f".to_string(),
            MethodSignature {
                params: Vec::new(),
                return_type: Type::Number,
                defined_in: "P".to_string(),
                span: SourceSpan::new(0, 0),
            },
        )]);
        registry.protocols.insert(
            "P".to_string(),
            ProtocolInfo {
                name: "P".to_string(),
                extends: Vec::new(),
                methods: p_methods.clone(),
                flattened_methods: p_methods, // already flattened
                span: SourceSpan::new(0, 0),
            },
        );

        // Insert type T with method f(): Number => 42.
        let t_methods = IndexMap::from([(
            "f".to_string(),
            MethodSignature {
                params: Vec::new(),
                return_type: Type::Number,
                defined_in: "T".to_string(),
                span: SourceSpan::new(0, 0),
            },
        )]);
        registry.types.insert(
            "T".to_string(),
            TypeInfo {
                name: "T".to_string(),
                params: Vec::new(),
                parent: None,
                attributes: IndexMap::new(),
                methods: t_methods.clone(),
                flattened_methods: t_methods,
                is_builtin_value: false,
                span: SourceSpan::new(0, 0),
            },
        );

        // T should conform to P structurally.
        let t = Type::Named("T".to_string());
        let p = Type::Named("P".to_string());
        assert!(t.conforms_to(&p, &registry));
        // Also check that the registry's implements_protocol works.
        assert!(registry.implements_protocol("T", "P"));
    }

    /// Tests LCA with a user-defined hierarchy where the common ancestor is not Object.
    #[test]
    fn lca_three_way_with_shared_grandparent() {
        let mut registry = seeded_registry();

        // A ← B, A ← C
        for (name, parent) in [("B", "A"), ("C", "A")] {
            registry.types.insert(
                name.to_string(),
                TypeInfo {
                    name: name.to_string(),
                    params: Vec::new(),
                    parent: Some(ParentLink {
                        name: parent.to_string(),
                        args: Vec::new(),
                    }),
                    attributes: IndexMap::new(),
                    methods: IndexMap::new(),
                    flattened_methods: IndexMap::new(),
                    is_builtin_value: false,
                    span: SourceSpan::new(0, 0),
                },
            );
        }
        // Insert A (root).
        registry.types.insert(
            "A".to_string(),
            TypeInfo {
                name: "A".to_string(),
                params: Vec::new(),
                parent: None,
                attributes: IndexMap::new(),
                methods: IndexMap::new(),
                flattened_methods: IndexMap::new(),
                is_builtin_value: false,
                span: SourceSpan::new(0, 0),
            },
        );

        let types = vec![Type::Named("B".to_string()), Type::Named("C".to_string())];
        let lca = lowest_common_ancestor(&types, &registry);
        assert_eq!(lca, Type::Named("A".to_string()));
    }

    /// Tests that `ancestor_chain` terminates cleanly even if a type's parent
    /// does not exist in the registry (shouldn't happen post-Pass-1, but the function
    /// is defensive). It should return a chain ending with Object.
    #[test]
    fn ancestor_chain_terminates_on_missing_parent() {
        let mut registry = seeded_registry();

        // Insert type X with parent "Missing" (not in registry).
        registry.types.insert(
            "X".to_string(),
            TypeInfo {
                name: "X".to_string(),
                params: Vec::new(),
                parent: Some(ParentLink {
                    name: "Missing".to_string(),
                    args: Vec::new(),
                }),
                attributes: IndexMap::new(),
                methods: IndexMap::new(),
                flattened_methods: IndexMap::new(),
                is_builtin_value: false,
                span: SourceSpan::new(0, 0),
            },
        );

        let ty = Type::Named("X".to_string());
        let chain = ancestor_chain(&ty, &registry);
        // The chain should contain at least the type itself and Object.
        assert!(chain.len() >= 2);
        assert_eq!(chain[0], Type::Named("X".to_string()));
        assert_eq!(chain.last(), Some(&Type::Object));
    }

    #[test]
    fn named_conforms_to_iterable_covariant() {
        let mut registry = seeded_registry();

        // Insert a type T that implements Iterable with current(): Number.
        let mut methods = IndexMap::new();
        methods.insert(
            "next".to_string(),
            MethodSignature {
                params: Vec::new(),
                return_type: Type::Boolean,
                defined_in: "T".to_string(),
                span: SourceSpan::new(0, 0),
            },
        );
        methods.insert(
            "current".to_string(),
            MethodSignature {
                params: Vec::new(),
                return_type: Type::Number,
                defined_in: "T".to_string(),
                span: SourceSpan::new(0, 0),
            },
        );
        registry.types.insert(
            "T".to_string(),
            TypeInfo {
                name: "T".to_string(),
                params: Vec::new(),
                parent: Some(ParentLink {
                    name: "Object".to_string(),
                    args: Vec::new(),
                }),
                attributes: IndexMap::new(),
                methods: methods.clone(),
                flattened_methods: methods,
                is_builtin_value: false,
                span: SourceSpan::new(0, 0),
            },
        );

        // T should conform to Iterable<Number>.
        let t = Type::Named("T".to_string());
        let iter_num = Type::Iterable(Box::new(Type::Number));
        assert!(t.conforms_to(&iter_num, &registry));

        // T should NOT conform to Iterable<String> (Number does not conform to String).
        let iter_str = Type::Iterable(Box::new(Type::String));
        assert!(!t.conforms_to(&iter_str, &registry));
    }

    #[test]
    fn iterable_covariance() {
        let registry = seeded_registry();
        let iter_num = Type::Iterable(Box::new(Type::Number));
        let iter_obj = Type::Iterable(Box::new(Type::Object));
        // Iterable<Number> <= Iterable<Object> because Number <= Object.
        assert!(iter_num.conforms_to(&iter_obj, &registry));
        // Iterable<Object> does not conform to Iterable<Number>.
        assert!(!iter_obj.conforms_to(&iter_num, &registry));
    }

    #[test]
    fn vector_covariance() {
        let registry = seeded_registry();
        let vec_num = Type::Vector(Box::new(Type::Number));
        let vec_obj = Type::Vector(Box::new(Type::Object));
        // Vector<Number> <= Vector<Object> because Number <= Object.
        assert!(vec_num.conforms_to(&vec_obj, &registry));
        // Vector<Object> does not conform to Vector<Number>.
        assert!(!vec_obj.conforms_to(&vec_num, &registry));
    }
}
