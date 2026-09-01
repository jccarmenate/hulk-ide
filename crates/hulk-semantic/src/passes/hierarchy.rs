//! Pass 1: Hierarchy & Protocol Resolution
//!
//! This pass resolves every `inherits` and `extends` link collected in Pass 0
//! into a validated, linked tree. After this pass, `Type::conforms_to` becomes
//! well‑defined, and member lookup in later passes is a single `IndexMap` access.
//!
//! Checks are performed in a specific order:
//! 1. Parent existence
//! 2. No inheriting from builtin value types (`Number`, `String`, `Boolean`)
//! 3. Cycle detection (returns the span of the type where the cycle starts)
//! 4. Override signature compatibility (class‑to‑class, no variance) — skipped if cycle
//! 5. Protocol extension variance (contravariant params, covariant return)
//! 6. Flatten attribute/method tables (parent‑to‑child) — skipped if cycle

use indexmap::IndexMap;
use std::collections::HashSet;

use hulk_ast::SourceSpan;

use crate::error::{SemanticError, SemanticErrorKind};
use crate::passes::utils::topological_order;
use crate::types::registry::{MethodSignature, TypeRegistry};

// -----------------------------------------------------------------------------
// Public entry point
// -----------------------------------------------------------------------------

/// Runs Pass 1: validates and links the type and protocol hierarchies.
///
/// # Arguments
/// * `registry` – The registry (mutated in place: parent links are resolved,
///   flattened method/attribute tables are built).
/// * `errors` – Vector to append any hierarchy‑related errors.
///
/// # Precondition
/// Pass 0 must have been run, so the registry contains all signatures.
/// The registry must already be seeded with builtins.
pub fn run(registry: &mut TypeRegistry, errors: &mut Vec<SemanticError>) {
    // Step 1: Resolve parent links and check existence.
    resolve_parent_links(registry, errors);

    // Step 2: Reject inheritance from builtin value types.
    check_builtin_inheritance(registry, errors);

    // Step 3: Detect cycles in the inheritance graph.
    let cycle = detect_cycles(registry);
    let has_cycle = cycle.is_some(); // Store flag before moving.

    if let Some((cycle_path, span)) = cycle {
        errors.push(SemanticError::error(
            SemanticErrorKind::InheritanceCycle(cycle_path),
            span,
        ));
        // Cycle is a hard error. Skip flattening to prevent cascading errors;
        // protocol checks are independent and can continue. Override checks
        // depend on a valid hierarchy, so they are skipped when a cycle is present.
    }

    // Step 4: Check override signature compatibility (class‑to‑class).
    // Only if no cycle exists.
    if !has_cycle {
        check_overrides(registry, errors);
    }

    // Step 5: Check protocol extension variance (always, because independent).
    flatten_protocols(registry, errors); // Flatten protocol method tables so variance check sees inherited methods
    check_protocol_variance(registry, errors);

    // Step 6: Flatten attribute and method tables.
    // Only if no cycle exists.
    if !has_cycle {
        flatten_tables(registry, errors);
    }
}

// -----------------------------------------------------------------------------
// Step 1: Parent existence
// -----------------------------------------------------------------------------

/// Resolves parent links and checks that every parent type exists.
/// If a parent is undefined, we clear the parent link to avoid further errors.
fn resolve_parent_links(registry: &mut TypeRegistry, errors: &mut Vec<SemanticError>) {
    let type_names: Vec<String> = registry.types.keys().cloned().collect();
    for name in type_names {
        let parent_name = match registry
            .types
            .get(&name)
            .and_then(|info| info.parent.as_ref())
        {
            Some(parent) => &parent.name,
            None => continue,
        };
        if !registry.types.contains_key(parent_name) {
            errors.push(SemanticError::error(
                SemanticErrorKind::InheritFromUndefinedType(parent_name.clone()),
                registry.types[&name].span,
            ));
            // Clear the invalid parent.
            if let Some(info) = registry.types.get_mut(&name) {
                info.parent = None;
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Step 2: No inheriting from builtin value types
// -----------------------------------------------------------------------------

/// Rejects inheritance from `Number`, `String`, or `Boolean` (§A.7.3).
fn check_builtin_inheritance(registry: &mut TypeRegistry, errors: &mut Vec<SemanticError>) {
    let builtin_value_types = ["Number", "String", "Boolean"];
    let type_names: Vec<String> = registry.types.keys().cloned().collect();
    for name in type_names {
        let parent_name = match registry
            .types
            .get(&name)
            .and_then(|info| info.parent.as_ref())
        {
            Some(parent) => &parent.name,
            None => continue,
        };
        if builtin_value_types.contains(&parent_name.as_str()) {
            errors.push(SemanticError::error(
                SemanticErrorKind::InheritFromBuiltinValueType(parent_name.clone()),
                registry.types[&name].span,
            ));
            // Clear the invalid parent.
            if let Some(info) = registry.types.get_mut(&name) {
                info.parent = None;
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Step 3: Cycle detection (with span)
// -----------------------------------------------------------------------------

/// Detects a cycle in the inheritance graph and returns the cycle path
/// together with the span of the type where the cycle was first detected.
///
/// Returns `Some((cycle_path, span))` if a cycle is found, else `None`.
fn detect_cycles(registry: &TypeRegistry) -> Option<(Vec<String>, SourceSpan)> {
    let mut visited = HashSet::new();
    let mut recursion_stack = HashSet::new();
    let mut path = Vec::new();

    for name in registry.types.keys() {
        if !visited.contains(name) {
            if let Some((cycle, span)) = dfs_cycle(
                name,
                registry,
                &mut visited,
                &mut recursion_stack,
                &mut path,
            ) {
                return Some((cycle, span));
            }
        }
    }
    None
}

/// Depth‑first search helper for cycle detection.
/// Returns the cycle path and the span of the type that caused the cycle.
fn dfs_cycle(
    current: &str,
    registry: &TypeRegistry,
    visited: &mut HashSet<String>,
    recursion_stack: &mut HashSet<String>,
    path: &mut Vec<String>,
) -> Option<(Vec<String>, SourceSpan)> {
    if recursion_stack.contains(current) {
        // Found a cycle: construct the path from the current node to the start.
        let start_idx = path.iter().position(|n| n == current).unwrap();
        let cycle = path[start_idx..].to_vec();
        let span = registry
            .types
            .get(current)
            .map(|info| info.span)
            .unwrap_or_else(|| SourceSpan::new(0, 0));
        return Some((cycle, span));
    }
    if visited.contains(current) {
        return None;
    }

    visited.insert(current.to_string());
    recursion_stack.insert(current.to_string());
    path.push(current.to_string());

    if let Some(parent) = registry.parent_of(current) {
        if let Some(result) = dfs_cycle(&parent, registry, visited, recursion_stack, path) {
            return Some(result);
        }
    }

    path.pop();
    recursion_stack.remove(current);
    None
}

// -----------------------------------------------------------------------------
// Step 4: Override signature compatibility (class‑to‑class)
// -----------------------------------------------------------------------------

/// Checks that every overriding method in a child type has the exact same
/// signature (parameter types and return type) as the parent's method.
/// This is stricter than protocol variance (§A.7.4).
fn check_overrides(registry: &mut TypeRegistry, errors: &mut Vec<SemanticError>) {
    let type_names: Vec<String> = registry.types.keys().cloned().collect();
    for name in type_names {
        let parent_name = match registry
            .types
            .get(&name)
            .and_then(|info| info.parent.as_ref())
        {
            Some(parent) => &parent.name,
            None => continue,
        };
        // If parent is unresolved (cleared earlier), skip.
        if !registry.types.contains_key(parent_name) {
            continue;
        }
        let parent_methods = &registry.types[parent_name].methods;
        let own_methods = registry.types[&name].methods.clone(); // clone to avoid borrow issues

        for (method_name, own_sig) in own_methods {
            if let Some(parent_sig) = parent_methods.get(&method_name) {
                // Must have exactly same signature (no variance).
                let mut mismatch = false;
                let mut expected = String::new();
                let mut found = String::new();

                if own_sig.params.len() != parent_sig.params.len() {
                    mismatch = true;
                    expected = format!(
                        "({}) -> {}",
                        parent_sig
                            .params
                            .iter()
                            .map(|(_, t)| t.to_string())
                            .collect::<Vec<_>>()
                            .join(", "),
                        parent_sig.return_type
                    );
                    found = format!(
                        "({}) -> {}",
                        own_sig
                            .params
                            .iter()
                            .map(|(_, t)| t.to_string())
                            .collect::<Vec<_>>()
                            .join(", "),
                        own_sig.return_type
                    );
                } else {
                    // Check each parameter type and return type for equality.
                    for ((_, p_type), (_, o_type)) in parent_sig.params.iter().zip(&own_sig.params)
                    {
                        if p_type != o_type {
                            mismatch = true;
                            break;
                        }
                    }
                    if !mismatch && parent_sig.return_type != own_sig.return_type {
                        mismatch = true;
                    }
                    if mismatch {
                        expected = format!(
                            "({}) -> {}",
                            parent_sig
                                .params
                                .iter()
                                .map(|(_, t)| t.to_string())
                                .collect::<Vec<_>>()
                                .join(", "),
                            parent_sig.return_type
                        );
                        found = format!(
                            "({}) -> {}",
                            own_sig
                                .params
                                .iter()
                                .map(|(_, t)| t.to_string())
                                .collect::<Vec<_>>()
                                .join(", "),
                            own_sig.return_type
                        );
                    }
                }

                if mismatch {
                    errors.push(SemanticError::error(
                        SemanticErrorKind::InvalidOverride {
                            method: method_name.clone(),
                            in_type: name.clone(),
                            expected,
                            found,
                        },
                        own_sig.span,
                    ));
                }
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Step 5: Protocol extension variance
// -----------------------------------------------------------------------------

/// Checks that a protocol extension (`extends`) respects variance rules:
/// - No method may be removed.
/// - Parameter types are contravariant (parent param <= child param).
/// - Return type is covariant (child return <= parent return).
fn check_protocol_variance(registry: &mut TypeRegistry, errors: &mut Vec<SemanticError>) {
    let protocol_names: Vec<String> = registry.protocols.keys().cloned().collect();
    for name in protocol_names {
        let protocol = registry.protocols.get(&name).unwrap().clone();
        for parent_name in &protocol.extends {
            if !registry.protocols.contains_key(parent_name) {
                errors.push(SemanticError::error(
                    SemanticErrorKind::UndefinedType(parent_name.clone()),
                    protocol.span,
                ));
                continue;
            }
            let parent_methods = &registry.protocols[parent_name].methods;
            let child_methods = &registry.protocols[&name].flattened_methods;

            // A protocol extension may not remove a method.
            for (method_name, parent_sig) in parent_methods {
                let own_sig = match child_methods.get(method_name) {
                    Some(sig) => sig,
                    None => {
                        errors.push(SemanticError::error(
                            SemanticErrorKind::InvalidProtocolVariance {
                                method: method_name.clone(),
                                reason: format!("method `{}` is required by parent protocol `{}` but not defined", method_name, parent_name),
                            },
                            protocol.span,
                        ));
                        continue;
                    }
                };
                // Check contravariance of parameters (parent param <= child param)
                // and covariance of return (child return <= parent return).
                let mut reason = String::new();
                let mut ok = true;
                if own_sig.params.len() != parent_sig.params.len() {
                    reason = format!(
                        "parameter count mismatch: expected {}, found {}",
                        parent_sig.params.len(),
                        own_sig.params.len()
                    );
                    ok = false;
                } else {
                    for ((_, p_type), (_, o_type)) in parent_sig.params.iter().zip(&own_sig.params)
                    {
                        if !p_type.conforms_to(o_type, registry) {
                            reason = format!("parameter type mismatch: expected {} (or supertype) for parameter of type {}, but got {}", p_type, p_type, o_type);
                            ok = false;
                            break;
                        }
                    }
                    if ok
                        && !own_sig
                            .return_type
                            .conforms_to(&parent_sig.return_type, registry)
                    {
                        reason = format!(
                            "return type mismatch: expected {} (or subtype) but got {}",
                            parent_sig.return_type, own_sig.return_type
                        );
                        ok = false;
                    }
                }
                if !ok {
                    errors.push(SemanticError::error(
                        SemanticErrorKind::InvalidProtocolVariance {
                            method: method_name.clone(),
                            reason,
                        },
                        own_sig.span,
                    ));
                }
            }
        }
    }
}

// Internal helpers

/// Flattens method tables for every protocol.
fn flatten_protocols(registry: &mut TypeRegistry, _errors: &mut Vec<SemanticError>) {
    let names: Vec<String> = registry.protocols.keys().cloned().collect();
    for name in names {
        let mut flattened = IndexMap::new();
        let mut visited = HashSet::new();
        flatten_protocol_recursive(registry, &name, &mut flattened, &mut visited);
        if let Some(proto) = registry.protocols.get_mut(&name) {
            proto.flattened_methods = flattened;
        }
    }
}

/// Recursively collects methods from a protocol and its ancestors.
fn flatten_protocol_recursive(
    registry: &TypeRegistry,
    name: &str,
    result: &mut IndexMap<String, MethodSignature>,
    visited: &mut HashSet<String>,
) {
    if !visited.insert(name.to_string()) {
        return;
    }
    if let Some(proto) = registry.protocols.get(name) {
        // First, inherit from parents.
        for parent in &proto.extends {
            flatten_protocol_recursive(registry, parent, result, visited);
        }
        // Then add own methods (override parents).
        for (k, v) in &proto.methods {
            result.insert(k.clone(), v.clone());
        }
    }
}

// -----------------------------------------------------------------------------
// Step 6: Flatten attribute and method tables
// -----------------------------------------------------------------------------

/// Flattens attribute and method tables for every type.
///
/// This copies inherited members from the parent into the child, then overwrites
/// with the child's own members. The result is that each `TypeInfo`'s `attributes`
/// and `methods` maps contain the full set of accessible members.
///
/// # Precondition
/// No cycles exist in the inheritance graph (ensured by `detect_cycles`).
fn flatten_tables(registry: &mut TypeRegistry, _errors: &mut Vec<SemanticError>) {
    // Compute topological order: parents before children.
    let order = topological_order(registry);

    for name in order {
        // Clone parent members if the type has a parent.
        let parent_name = registry
            .types
            .get(&name)
            .and_then(|info| info.parent.as_ref())
            .map(|p| p.name.clone());

        if let Some(ref parent_name) = parent_name {
            if let Some(parent_info) = registry.types.get(parent_name) {
                let parent_attrs = parent_info.attributes.clone();
                let parent_methods = parent_info.methods.clone();

                let child_info = registry.types.get_mut(&name).unwrap();

                // Merge parent attributes into child, then overwrite with child's own.
                let mut combined_attrs = parent_attrs;
                combined_attrs.extend(child_info.attributes.clone());
                child_info.attributes = combined_attrs;

                // Merge parent methods into child, then overwrite with child's own.
                let mut combined_methods = parent_methods;
                combined_methods.extend(child_info.methods.clone());
                child_info.methods = combined_methods;
            }
        }

        // Whether or not the type has a parent, its own (possibly merged) `methods`
        // table now represents the full flattened set.
        let child_info = registry.types.get_mut(&name).unwrap();
        child_info.flattened_methods = child_info.methods.clone();
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze;
    use crate::error::Severity;
    use crate::passes::utils::assert_error_kind;
    use crate::passes::{collect, hierarchy};
    use crate::seeded_registry;
    use crate::types::{Type, TypeRegistry};
    use hulk_lexer::Lexer;
    use hulk_parser::parse;

    fn parse_and_hierarchy(src: &str) -> (TypeRegistry, Vec<SemanticError>) {
        let tokens = Lexer::new(src).tokenize().expect("lex ok");
        let program = parse(tokens).expect("parse ok");
        let mut registry = seeded_registry();
        let mut errors = Vec::new();
        collect::run(&program, &mut registry, &mut errors);
        if errors.iter().any(|e| e.severity == Severity::Error) {
            return (registry, errors);
        }
        hierarchy::run(&mut registry, &mut errors);
        (registry, errors)
    }

    #[test]
    fn valid_inheritance_chain() {
        let src = "
            type A { }
            type B inherits A { }
            type C inherits B { }
            print(0);
        ";
        let (registry, errors) = parse_and_hierarchy(src);
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);
        // Check ancestry.
        assert!(registry.is_ancestor("A", "B"));
        assert!(registry.is_ancestor("B", "C"));
        assert!(registry.is_ancestor("A", "C"));
        assert!(!registry.is_ancestor("C", "A"));
    }

    #[test]
    fn inheritance_cycle_detected() {
        let src = "
            type A inherits B { }
            type B inherits C { }
            type C inherits A { }
            print(0);
        ";
        let (_, errors) = parse_and_hierarchy(src);
        assert!(errors
            .iter()
            .any(|e| matches!(e.kind, SemanticErrorKind::InheritanceCycle(_))));
    }

    #[test]
    fn inherit_from_builtin_value_type() {
        let src = "type A inherits Number { } print(0);";
        let (_, errors) = parse_and_hierarchy(src);
        assert_error_kind(
            &errors,
            SemanticErrorKind::InheritFromBuiltinValueType("Number".to_string()),
        );
    }

    #[test]
    fn override_mismatch() {
        let src = "
            type A { f(): Number => 1; }
            type B inherits A { f(): String => \"hello\"; }
            print(0);
        ";
        let (_, errors) = parse_and_hierarchy(src);
        // Expected: return type mismatch (Number vs String)
        assert!(errors
            .iter()
            .any(|e| matches!(e.kind, SemanticErrorKind::InvalidOverride { .. })));
    }

    #[test]
    fn base_resolution() {
        // Person/Knight example from §A.7.4
        let src = "
            type Person(firstname, lastname) {
                firstname = firstname;
                lastname = lastname;
                name(): String => self.firstname @@ self.lastname;
            }
            type Knight(firstname, lastname) inherits Person(firstname, lastname) {
                name(): String => \"Sir\" @@ base();
            }
            print((new Knight(\"Phil\", \"Collins\")).name());
        ";

        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_ok(), "base resolution failed: {:?}", result.err());
    }

    /// Tests that constructor parameters are resolved through multiple inheritance levels.
    /// A -> B -> C, with a concrete argument provided at the deepest level.
    #[test]
    fn constructor_param_resolved_through_two_levels_of_inheritance() {
        let src = "
            type A(x) { }
            type B(x) inherits A(x) { }
            type C(x) inherits B(x) { }
            print(new C(42));
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(
            result.is_ok(),
            "constructor resolution through two inheritance levels failed: {:?}",
            result.err()
        );

        let registry = &result.unwrap().registry;
        // Check that all three types have their `x` parameter resolved to Number.
        for type_name in ["A", "B", "C"] {
            let info = registry.lookup_type(type_name).expect("type should exist");
            assert_eq!(
                info.params[0].1,
                Type::Number,
                "type {} parameter should be Number, got {:?}",
                type_name,
                info.params[0].1
            );
        }
    }

    /// Tests that ambiguous constructor parameters across subclasses produce an error.
    /// Two subclasses pass different concrete types to the same base parameter.
    #[test]
    fn constructor_param_ambiguous_across_subclasses() {
        let src = "
            type Base(x) { }
            type Sub1(x) inherits Base(x) { }
            type Sub2(x) inherits Base(x) { }
            {
                print(new Sub1(42));
                print(new Sub2(\"hello\"));
            }
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(result.is_err(), "expected ambiguity error");
        let errors = result.err().unwrap();
        // Expect at least one AmbiguousInference error for parameter `x` in `Base`.
        assert!(
            errors
                .iter()
                .any(|e| matches!(e.kind, SemanticErrorKind::AmbiguousInference { .. })),
            "missing AmbiguousInference error"
        );
    }

    /// Tests that flattened method tables correctly resolve to the most derived signature
    /// across a three-level inheritance chain.
    #[test]
    fn flatten_tables_three_level_chain() {
        let src = "
            type A {
                f(): String => \"A\";
                g(): String => \"A\";
            }
            type B inherits A {
                f(): String => \"B\";  // override
                h(): String => \"B\";  // new
            }
            type C inherits B {
                g(): String => \"C\";  // override from A
            }
            print(new C().f());
        ";
        let result = analyze(&parse(Lexer::new(src).tokenize().unwrap()).unwrap());
        assert!(
            result.is_ok(),
            "three-level flattening failed: {:?}",
            result.err()
        );

        let registry = &result.unwrap().registry;
        let c_info = registry.lookup_type("C").expect("type C should exist");
        // Check that C.flattened_methods has the correct overrides:
        // f should come from B (String), g from C (String), h from B (String)
        assert_eq!(c_info.flattened_methods["f"].return_type, Type::String);
        assert_eq!(c_info.flattened_methods["g"].return_type, Type::String);
        assert_eq!(c_info.flattened_methods["h"].return_type, Type::String);
        // Additionally, we could check that the defined_in field points to the correct type.
        assert_eq!(c_info.flattened_methods["f"].defined_in, "B");
        assert_eq!(c_info.flattened_methods["g"].defined_in, "C");
        assert_eq!(c_info.flattened_methods["h"].defined_in, "B");
    }
}
