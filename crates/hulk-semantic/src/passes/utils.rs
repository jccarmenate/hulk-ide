use std::collections::{HashMap, VecDeque};

use crate::types::registry::TypeRegistry;

/// Returns a topological order of types (parents before children) using Kahn's algorithm.
pub fn topological_order(registry: &TypeRegistry) -> Vec<String> {
    let mut in_degree: HashMap<String, usize> = HashMap::new();
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();

    // Initialize.
    for name in registry.types.keys() {
        in_degree.entry(name.clone()).or_insert(0);
        graph.entry(name.clone()).or_default();
    }

    // Build edges from parent to child.
    for (name, info) in &registry.types {
        if let Some(parent) = &info.parent {
            if registry.types.contains_key(&parent.name) {
                graph
                    .entry(parent.name.clone())
                    .or_default()
                    .push(name.clone());
                *in_degree.entry(name.clone()).or_insert(0) += 1;
            }
        }
    }

    // Kahn's algorithm.
    let mut queue: VecDeque<String> = VecDeque::new();
    for (name, &deg) in &in_degree {
        if deg == 0 {
            queue.push_back(name.clone());
        }
    }

    let mut order = Vec::new();
    while let Some(name) = queue.pop_front() {
        order.push(name.clone());
        if let Some(children) = graph.get(&name) {
            for child in children {
                if let Some(deg) = in_degree.get_mut(child) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(child.clone());
                    }
                }
            }
        }
    }

    // If there are cycles, the order will be incomplete, but we already checked for cycles.
    order
}

#[cfg(test)]
use crate::error::{SemanticError, SemanticErrorKind};

#[cfg(test)]
pub fn assert_error_kind(errors: &[SemanticError], expected: SemanticErrorKind) {
    assert!(
        errors.iter().any(|e| e.kind == expected),
        "expected error {:?} not found",
        expected
    );
}
