//! Collects all `MacroDecl` nodes from the program into a registry.
//!
//! After this pass, `MacroDecl` entries are removed from the program's
//! declaration list. Only `Function`, `Type`, and `Protocol` declarations
//! survive to the semantic phase.

use std::collections::HashMap;
use hulk_ast::{DeclarationKind, MacroDecl, Program, SourceSpan};
use crate::error::{MacroError, MacroErrorKind};

pub type MacroRegistry = HashMap<String, (MacroDecl, SourceSpan)>;

/// Partitions `program.declarations` into macro declarations (returned) and
/// everything else (left in `program`).
///
/// Duplicate macro names are reported as errors.
pub fn collect(
    program: &mut Program,
    errors: &mut Vec<MacroError>,
) -> MacroRegistry {
    let mut registry = MacroRegistry::new();
    let mut remaining = Vec::new();

    for decl in program.declarations.drain(..) {
        match decl.kind {
            DeclarationKind::Macro(m) => {
                if registry.contains_key(&m.name) {
                    errors.push(MacroError::new(
                        MacroErrorKind::UndefinedMacro(format!(
                            "duplicate macro definition `{}`", m.name
                        )),
                        decl.span,
                    ));
                } else {
                    registry.insert(m.name.clone(), (m, decl.span));
                }
            }
            _ => remaining.push(decl),
        }
    }

    program.declarations = remaining;
    registry
}