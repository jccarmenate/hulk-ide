// hulk-transpile/src/pattern.rs
//
// Structural pattern matching for macro arguments (§A.14.5).
//
// This module provides the `try_match` function, which compares a macro
// pattern (representing an AST shape) with a concrete `Expr` AST node.
// On success, it returns a mapping from capture variable names to the
// matched sub‑expressions. The expansion engine in `substitute.rs` uses this
// to implement compile‑time `match` inside macro bodies.

use std::collections::HashMap;
use hulk_ast::{
    Expr, ExprKind, MacroPattern, MacroPatternBind,
};
use crate::error::MacroErrorKind;

/// Attempts to match a `pattern` against the concrete AST node `expr`.
///
/// # Returns
/// - `Ok(Some(bindings))` if the pattern matches, where `bindings` maps each
///   capture variable name to the `Expr` subtree that was matched.
/// - `Ok(None)` if the pattern does not match (shape mismatch).
/// - `Err(MacroErrorKind)` if a structural error occurs, e.g., duplicate bindings.
///
/// # Matching rules
/// - `MacroPattern::Wildcard` always matches, producing no bindings.
/// - `MacroPattern::Literal(lit)` matches only if `expr` is a literal with
///   the same value.
/// - `MacroPattern::Bind` recursively matches its inner pattern; if successful
///   and the bind has a `name`, the matched expression is inserted into the map.
/// - `MacroPattern::BinaryExpr` matches an `ExprKind::Binary` node with the
///   given operator, then recursively matches the left and right operands
///   (using `MacroPatternBind` to capture them if named).
/// - `MacroPattern::UnaryExpr` matches an `ExprKind::Unary` node similarly.
///
/// # Duplicate bindings
/// If the same variable name appears more than once in a single pattern,
/// the match fails with `MacroErrorKind::DuplicatePatternBinding`.
pub fn try_match(
    pattern: &MacroPattern,
    expr: &Expr,
) -> Result<Option<HashMap<String, Expr>>, MacroErrorKind> {
    match pattern {
        MacroPattern::Wildcard => Ok(Some(HashMap::new())),

        MacroPattern::Literal(lit) => {
            if let ExprKind::Literal(ref e_lit) = expr.kind {
                if e_lit == lit {
                    return Ok(Some(HashMap::new()));
                }
            }
            Ok(None)
        }

        MacroPattern::Bind { name, ty: _, pattern: inner } => {
            let inner_result = try_match(inner, expr)?;
            if let Some(mut map) = inner_result {
                if let Some(ref n) = name {
                    if map.contains_key(n) {
                        return Err(MacroErrorKind::DuplicatePatternBinding { name: n.clone() });
                    }
                    map.insert(n.clone(), expr.clone());
                }
                Ok(Some(map))
            } else {
                Ok(None)
            }
        }

        MacroPattern::BinaryExpr { op, left, right } => {
            if let ExprKind::Binary(ref bin) = expr.kind {
                if bin.op == *op {
                    let left_map = match_bind(left, &bin.left)?;
                    let right_map = match_bind(right, &bin.right)?;
                    match (left_map, right_map) {
                        (Some(l), Some(r)) => merge_maps(l, r).map(Some),
                        _ => Ok(None),
                    }
                } else {
                    Ok(None)
                }
            } else {
                Ok(None)
            }
        }

        MacroPattern::UnaryExpr { op, operand } => {
            if let ExprKind::Unary(ref unary) = expr.kind {
                if unary.op == *op {
                    match_bind(operand, &unary.expr)
                } else {
                    Ok(None)
                }
            } else {
                Ok(None)
            }
        }
    }
}

/// Matches a `MacroPatternBind` against an expression.
///
/// This first recursively matches the inner pattern. If that succeeds and the
/// bind has a `name`, the whole expression is inserted into the bindings map
/// under that name. The optional type annotation (`ty`) is ignored during matching.
fn match_bind(
    bind: &MacroPatternBind,
    expr: &Expr,
) -> Result<Option<HashMap<String, Expr>>, MacroErrorKind> {
    let inner_result = try_match(&bind.pattern, expr)?;
    if let Some(mut map) = inner_result {
        if let Some(ref name) = bind.name {
            if map.contains_key(name) {
                return Err(MacroErrorKind::DuplicatePatternBinding { name: name.clone() });
            }
            map.insert(name.clone(), expr.clone());
        }
        Ok(Some(map))
    } else {
        Ok(None)
    }
}

/// Merges two binding maps. Returns `Err` if they share any key.
fn merge_maps(
    left: HashMap<String, Expr>,
    right: HashMap<String, Expr>,
) -> Result<HashMap<String, Expr>, MacroErrorKind> {
    let mut merged = left;
    for (k, v) in right {
        if merged.contains_key(&k) {
            return Err(MacroErrorKind::DuplicatePatternBinding { name: k });
        }
        merged.insert(k, v);
    }
    Ok(merged)
}
#[cfg(test)]
mod tests {
    use super::*;
    use hulk_ast::{
        BinaryOp, Expr, Literal, MacroPattern, MacroPatternBind, SourceSpan, UnaryOp,
    };
    use crate::error::MacroErrorKind;

    fn s() -> SourceSpan {
        SourceSpan::new(1, 1)
    }

    fn num(n: f64) -> Expr {
        Expr::number(n, s())
    }

    fn bool_lit(b: bool) -> Expr {
        Expr::boolean(b, s())
    }

    fn string_lit(st: &str) -> Expr {
        Expr::string(st, s())
    }

    fn _var(name: &str) -> Expr {
        Expr::variable(name, s())
    }

    fn bin(op: BinaryOp, left: Expr, right: Expr) -> Expr {
        Expr::binary(op, left, right, s())
    }

    fn un(op: UnaryOp, expr: Expr) -> Expr {
        Expr::unary(op, expr, s())
    }

    // Helper to create a Bind pattern with a name and inner pattern.
    fn bind(name: &str, pattern: MacroPattern) -> MacroPattern {
        MacroPattern::Bind {
            name: Some(name.to_string()),
            ty: None,
            pattern: Box::new(pattern),
        }
    }

    // Helper to create a Bind pattern with type annotation.
    fn bind_ty(name: &str, ty: &str, pattern: MacroPattern) -> MacroPattern {
        MacroPattern::Bind {
            name: Some(name.to_string()),
            ty: Some(hulk_ast::TypeRef::named(ty)),
            pattern: Box::new(pattern),
        }
    }

    #[test]
    fn wildcard_matches_anything() {
        let pattern = MacroPattern::Wildcard;
        let expr = num(42.0);
        let result = try_match(&pattern, &expr);
        assert_eq!(result, Ok(Some(HashMap::new())));
    }

    #[test]
    fn literal_number_matches_exact() {
        let pattern = MacroPattern::Literal(Literal::Number(42.0));
        assert_eq!(try_match(&pattern, &num(42.0)), Ok(Some(HashMap::new())));
        assert_eq!(try_match(&pattern, &num(43.0)), Ok(None));
        assert_eq!(try_match(&pattern, &bool_lit(true)), Ok(None));
    }

    #[test]
    fn literal_string_matches_exact() {
        let pattern = MacroPattern::Literal(Literal::String("hello".to_string()));
        assert_eq!(try_match(&pattern, &string_lit("hello")), Ok(Some(HashMap::new())));
        assert_eq!(try_match(&pattern, &string_lit("world")), Ok(None));
    }

    #[test]
    fn literal_bool_matches_exact() {
        let pattern = MacroPattern::Literal(Literal::Boolean(true));
        assert_eq!(try_match(&pattern, &bool_lit(true)), Ok(Some(HashMap::new())));
        assert_eq!(try_match(&pattern, &bool_lit(false)), Ok(None));
    }

    #[test]
    fn bind_captures_expression() {
        let pattern = bind("x", MacroPattern::Wildcard);
        let expr = num(42.0);
        let result = try_match(&pattern, &expr);
        let mut expected = HashMap::new();
        expected.insert("x".to_string(), expr.clone());
        assert_eq!(result, Ok(Some(expected)));
    }

    #[test]
    fn bind_with_type_ignores_type_during_matching() {
        let pattern = bind_ty("x", "Number", MacroPattern::Wildcard);
        let expr = string_lit("hello");
        let result = try_match(&pattern, &expr);
        let mut expected = HashMap::new();
        expected.insert("x".to_string(), expr.clone());
        assert_eq!(result, Ok(Some(expected)));
    }

    #[test]
    fn bind_on_compound_pattern_captures_whole() {
        let pattern = MacroPattern::Bind {
            name: Some("whole".to_string()),
            ty: None,
            pattern: Box::new(MacroPattern::BinaryExpr {
                op: BinaryOp::Add,
                left: Box::new(MacroPatternBind {
                    name: Some("left".to_string()),
                    ty: None,
                    pattern: MacroPattern::Wildcard,
                }),
                right: Box::new(MacroPatternBind {
                    name: Some("right".to_string()),
                    ty: None,
                    pattern: MacroPattern::Wildcard,
                }),
            }),
        };
        let expr = bin(BinaryOp::Add, num(1.0), num(2.0));
        let result = try_match(&pattern, &expr);
        let mut expected = HashMap::new();
        expected.insert("whole".to_string(), expr.clone());
        expected.insert("left".to_string(), num(1.0));
        expected.insert("right".to_string(), num(2.0));
        assert_eq!(result, Ok(Some(expected)));
    }

    #[test]
    fn binary_expr_matches_operator_and_binds_operands() {
        let pattern = MacroPattern::BinaryExpr {
            op: BinaryOp::Add,
            left: Box::new(MacroPatternBind {
                name: Some("a".to_string()),
                ty: None,
                pattern: MacroPattern::Wildcard,
            }),
            right: Box::new(MacroPatternBind {
                name: Some("b".to_string()),
                ty: None,
                pattern: MacroPattern::Wildcard,
            }),
        };
        let expr = bin(BinaryOp::Add, num(1.0), num(2.0));
        let result = try_match(&pattern, &expr);
        let mut expected = HashMap::new();
        expected.insert("a".to_string(), num(1.0));
        expected.insert("b".to_string(), num(2.0));
        assert_eq!(result, Ok(Some(expected)));

        // Wrong operator
        let expr2 = bin(BinaryOp::Subtract, num(1.0), num(2.0));
        assert_eq!(try_match(&pattern, &expr2), Ok(None));
    }

    #[test]
    fn binary_expr_with_nested_patterns() {
        let pattern = MacroPattern::BinaryExpr {
            op: BinaryOp::Multiply,
            left: Box::new(MacroPatternBind {
                name: None,
                ty: None,
                pattern: MacroPattern::BinaryExpr {
                    op: BinaryOp::Add,
                    left: Box::new(MacroPatternBind {
                        name: Some("x".to_string()),
                        ty: None,
                        pattern: MacroPattern::Wildcard,
                    }),
                    right: Box::new(MacroPatternBind {
                        name: Some("y".to_string()),
                        ty: None,
                        pattern: MacroPattern::Wildcard,
                    }),
                },
            }),
            right: Box::new(MacroPatternBind {
                name: Some("z".to_string()),
                ty: None,
                pattern: MacroPattern::Wildcard,
            }),
        };
        let expr = bin(
            BinaryOp::Multiply,
            bin(BinaryOp::Add, num(1.0), num(2.0)),
            num(3.0),
        );
        let result = try_match(&pattern, &expr);
        let mut expected = HashMap::new();
        expected.insert("x".to_string(), num(1.0));
        expected.insert("y".to_string(), num(2.0));
        expected.insert("z".to_string(), num(3.0));
        assert_eq!(result, Ok(Some(expected)));
    }

    #[test]
    fn unary_expr_matches_operator_and_binds_operand() {
        let pattern = MacroPattern::UnaryExpr {
            op: UnaryOp::Negate,
            operand: Box::new(MacroPatternBind {
                name: Some("x".to_string()),
                ty: None,
                pattern: MacroPattern::Wildcard,
            }),
        };
        let expr = un(UnaryOp::Negate, num(42.0));
        let result = try_match(&pattern, &expr);
        let mut expected = HashMap::new();
        expected.insert("x".to_string(), num(42.0));
        assert_eq!(result, Ok(Some(expected)));

        // Wrong operator
        let expr2 = un(UnaryOp::Not, bool_lit(true));
        assert_eq!(try_match(&pattern, &expr2), Ok(None));
    }

    #[test]
    fn duplicate_bindings_are_rejected() {
        // Pattern: (x + x) – binding `x` twice
        let pattern = MacroPattern::BinaryExpr {
            op: BinaryOp::Add,
            left: Box::new(MacroPatternBind {
                name: Some("x".to_string()),
                ty: None,
                pattern: MacroPattern::Wildcard,
            }),
            right: Box::new(MacroPatternBind {
                name: Some("x".to_string()),
                ty: None,
                pattern: MacroPattern::Wildcard,
            }),
        };
        let expr = bin(BinaryOp::Add, num(1.0), num(2.0));
        let err = try_match(&pattern, &expr).expect_err("should be duplicate binding error");
        assert!(matches!(err, MacroErrorKind::DuplicatePatternBinding { name } if name == "x"));
    }

    #[test]
    fn duplicate_bindings_across_nested_patterns() {
        // Pattern: (x + (x * 2)) – x appears twice
        let pattern = MacroPattern::BinaryExpr {
            op: BinaryOp::Add,
            left: Box::new(MacroPatternBind {
                name: Some("x".to_string()),
                ty: None,
                pattern: MacroPattern::Wildcard,
            }),
            right: Box::new(MacroPatternBind {
                name: None,
                ty: None,
                pattern: MacroPattern::BinaryExpr {
                    op: BinaryOp::Multiply,
                    left: Box::new(MacroPatternBind {
                        name: Some("x".to_string()),
                        ty: None,
                        pattern: MacroPattern::Wildcard,
                    }),
                    right: Box::new(MacroPatternBind {
                        name: Some("y".to_string()),
                        ty: None,
                        pattern: MacroPattern::Wildcard,
                    }),
                },
            }),
        };
        let expr = bin(
            BinaryOp::Add,
            num(1.0),
            bin(BinaryOp::Multiply, num(2.0), num(3.0)),
        );
        let err = try_match(&pattern, &expr).expect_err("should be duplicate binding error");
        assert!(matches!(err, MacroErrorKind::DuplicatePatternBinding { name } if name == "x"));
    }

    #[test]
    fn wildcard_does_not_bind() {
        let pattern = MacroPattern::BinaryExpr {
            op: BinaryOp::Add,
            left: Box::new(MacroPatternBind {
                name: None,
                ty: None,
                pattern: MacroPattern::Wildcard,
            }),
            right: Box::new(MacroPatternBind {
                name: None,
                ty: None,
                pattern: MacroPattern::Wildcard,
            }),
        };
        let expr = bin(BinaryOp::Add, num(1.0), num(2.0));
        let result = try_match(&pattern, &expr);
        assert_eq!(result, Ok(Some(HashMap::new())));
    }
}