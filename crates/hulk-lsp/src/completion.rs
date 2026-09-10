//! Builds completion suggestions for a cursor position.
//!
//! Two modes: right after `object.`, suggest that receiver's members
//! (methods and attributes, walking the inheritance chain); otherwise,
//! suggest every variable/`self` name found anywhere in the last-good
//! program, plus every global function and type name from the type
//! registry.
//!
//! This is deliberately more approximate than hover/go-to-definition (see
//! `resolve.rs`'s module doc comment and this project's Plan 5): the
//! buffer at the moment completion is requested (typically right after
//! typing `.`) has almost always just stopped parsing cleanly, so there
//! is no fresh, exact position to resolve against — only the last-good
//! tree from before this edit. Suggestions are found by name, not by
//! scope-accurate position.

use std::collections::HashSet;

use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind, Position};

use hulk_semantic::{Type, VerifiedProgram};

pub fn completion_items(
    verified: &VerifiedProgram,
    text: &str,
    position: Position,
) -> Vec<CompletionItem> {
    match receiver_before_dot(text, position) {
        Some(receiver) => member_completions(verified, &receiver),
        None => general_completions(verified),
    }
}

/// If the cursor immediately follows `<ident>.`, returns `<ident>`.
fn receiver_before_dot(text: &str, position: Position) -> Option<String> {
    let line = text.lines().nth(position.line as usize)?;
    let col = (position.character as usize).min(line.len());
    let before_cursor = &line[..col];
    let before_dot = before_cursor.strip_suffix('.')?;
    let ident_start = before_dot
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(|i| i + 1)
        .unwrap_or(0);
    let ident = &before_dot[ident_start..];
    let first = ident.chars().next()?;
    if !(first.is_alphabetic() || first == '_') {
        return None;
    }
    Some(ident.to_string())
}

fn member_completions(verified: &VerifiedProgram, receiver: &str) -> Vec<CompletionItem> {
    let Some(ty) = crate::resolve::find_named_type(&verified.typed_program, receiver) else {
        return Vec::new();
    };

    let mut items = Vec::new();
    if let Some(methods) = verified.registry.method_table_for(&ty) {
        for (name, sig) in methods {
            let params: Vec<String> = sig
                .params
                .iter()
                .map(|(n, t)| format!("{n}: {t}"))
                .collect();
            items.push(CompletionItem {
                label: name,
                kind: Some(CompletionItemKind::METHOD),
                detail: Some(format!("({}) -> {}", params.join(", "), sig.return_type)),
                ..Default::default()
            });
        }
    }

    if let Type::Named(type_name) = &ty {
        let mut current = Some(type_name.clone());
        let mut seen = HashSet::new();
        while let Some(name) = current {
            if let Some(info) = verified.registry.lookup_type(&name) {
                for (attr_name, attr) in &info.attributes {
                    if seen.insert(attr_name.clone()) {
                        items.push(CompletionItem {
                            label: attr_name.clone(),
                            kind: Some(CompletionItemKind::FIELD),
                            detail: attr.declared_type.as_ref().map(|t| t.to_string()),
                            ..Default::default()
                        });
                    }
                }
            }
            current = verified.registry.parent_of(&name);
        }
    }

    items
}

fn general_completions(verified: &VerifiedProgram) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();

    for name in crate::resolve::all_declared_names(&verified.typed_program) {
        if seen.insert(name.clone()) {
            items.push(CompletionItem {
                label: name,
                kind: Some(CompletionItemKind::VARIABLE),
                ..Default::default()
            });
        }
    }

    for (name, sig) in &verified.registry.functions {
        if seen.insert(name.clone()) {
            let params: Vec<String> = sig
                .params
                .iter()
                .map(|(n, t)| format!("{n}: {t}"))
                .collect();
            items.push(CompletionItem {
                label: name.clone(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some(format!("({}) -> {}", params.join(", "), sig.return_type)),
                ..Default::default()
            });
        }
    }

    for name in verified.registry.types.keys() {
        if seen.insert(name.clone()) {
            items.push(CompletionItem {
                label: name.clone(),
                kind: Some(CompletionItemKind::CLASS),
                ..Default::default()
            });
        }
    }

    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_analyze(source: &str) -> VerifiedProgram {
        let tokens = hulk_lexer::Lexer::new(source)
            .tokenize()
            .expect("valid tokens");
        let mut program = hulk_parser::parse(tokens).expect("valid parse");
        hulk_transpile::expand_program(&mut program);
        hulk_semantic::analyze(&program).expect("valid program")
    }

    #[test]
    fn receiver_before_dot_finds_the_identifier_right_before_the_cursor() {
        assert_eq!(
            receiver_before_dot(
                "obj.",
                Position {
                    line: 0,
                    character: 4
                }
            ),
            Some("obj".to_string())
        );
        assert_eq!(
            receiver_before_dot(
                "let x = obj.",
                Position {
                    line: 0,
                    character: 12
                }
            ),
            Some("obj".to_string())
        );
    }

    #[test]
    fn receiver_before_dot_is_none_without_a_trailing_dot() {
        assert_eq!(
            receiver_before_dot(
                "obj",
                Position {
                    line: 0,
                    character: 3
                }
            ),
            None
        );
    }

    #[test]
    fn completion_after_dot_suggests_methods_and_attributes() {
        let source =
            "type A {\n    value: Number = 1;\n    f(): Number => 1;\n}\nlet a = new A() in\na;";
        let verified = test_analyze(source);
        let items = completion_items(
            &verified,
            "a.",
            Position {
                line: 0,
                character: 2,
            },
        );
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"f"), "expected method `f` in {labels:?}");
        assert!(
            labels.contains(&"value"),
            "expected attribute `value` in {labels:?}"
        );
    }

    #[test]
    fn completion_without_a_dot_suggests_bound_names_and_globals() {
        let verified = test_analyze("let x = 5 in\nx + 1;");
        let items = completion_items(
            &verified,
            "x",
            Position {
                line: 1,
                character: 0,
            },
        );
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"x"));
        assert!(labels.contains(&"print"));
    }
}
