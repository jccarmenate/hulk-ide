//! Type registry – the global, context‑independent knowledge base.
//!
//! This module builds and maintains the registry of all types, protocols,
//! and functions known to the HULK program (both builtins and user‑defined).
//! It is constructed once during semantic analysis and remains read‑only
//! after Pass 1.

use indexmap::IndexMap;

use hulk_ast::{Expr, SourceSpan};

use super::Type;

// -----------------------------------------------------------------------------
// Core registry structures
// -----------------------------------------------------------------------------

/// The global registry of types, protocols, and functions.
///
/// Three separate maps because HULK never resolves a function name and a
/// type name through the same syntax (`f(...)` vs. `new T(...)`), so there
/// is no ambiguity to arbitrate between them.
#[derive(Debug, Clone)]
pub struct TypeRegistry {
    /// All user‑defined and builtin types, keyed by their name.
    /// Includes classes (`type`) and builtin types (`Number`, `Object`, etc.).
    pub types: IndexMap<String, TypeInfo>,

    /// All user‑defined and builtin protocols, keyed by protocol name.
    pub protocols: IndexMap<String, ProtocolInfo>,

    /// All user‑defined and builtin global functions, keyed by function name.
    /// Builtins such as `print`, `sqrt`, `sin`, `cos`, `exp`, `log`, `rand`,
    /// `range`, `PI`, `E` are seeded here.
    pub functions: IndexMap<String, FunctionSignature>,
}

/// Information about a user‑defined `type` declaration.
#[derive(Debug, Clone)]
pub struct TypeInfo {
    /// The name of the type (as declared in source).
    pub name: String,
    /// Constructor parameters (type arguments) of the type.
    pub params: Vec<(String, Type)>,
    /// Parent type and its constructor arguments, as written in source.
    pub parent: Option<ParentLink>,
    /// Own attributes (not yet including inherited ones).
    pub attributes: IndexMap<String, AttributeInfo>,
    /// Own methods (not yet including inherited ones).
    pub methods: IndexMap<String, MethodSignature>,
    /// Flattened method table (own + inherited), built during Pass 1.
    pub flattened_methods: IndexMap<String, MethodSignature>,
    /// Flag to mark builtin value types (`Number`, `String`, `Boolean`)
    /// so that inheritance from them can be rejected.
    pub is_builtin_value: bool,
    /// The source span of the `type` declaration.
    pub span: SourceSpan,
}

/// The `inherits Base(args)` clause, as written in source.
#[derive(Debug, Clone)]
pub struct ParentLink {
    pub name: String,
    pub args: Vec<Expr>, // untyped Expr<()>, collected before inference
}

/// Information about an attribute (field) of a type.
#[derive(Debug, Clone)]
pub struct AttributeInfo {
    pub declared_type: Option<Type>, // None until inferred in Pass 2
    pub span: SourceSpan,
}

/// Signature of a method (or protocol method).
#[derive(Debug, Clone)]
pub struct MethodSignature {
    pub params: Vec<(String, Type)>,
    pub return_type: Type,
    pub defined_in: String, // owning type name, for `base` resolution
    pub span: SourceSpan,
}

/// Information about a `protocol` declaration.
#[derive(Debug, Clone)]
pub struct ProtocolInfo {
    pub name: String,
    pub extends: Vec<String>, // names of protocols this one extends
    pub methods: IndexMap<String, MethodSignature>,
    pub flattened_methods: IndexMap<String, MethodSignature>,
    pub span: SourceSpan,
}

/// Signature of a global function (builtin or user‑defined).
#[derive(Debug, Clone)]
pub struct FunctionSignature {
    pub params: Vec<(String, Type)>,
    pub return_type: Type,
    pub span: SourceSpan,
    pub is_constant: bool, // true for PI, E, etc.
}

// -----------------------------------------------------------------------------
// Builtin seeding
// -----------------------------------------------------------------------------

/// Returns a `TypeRegistry` pre‑populated with all HULK builtins.
///
/// This is called once at the start of `analyze` so that every builtin
/// name is already present before the user's declarations are collected.
pub fn seeded_registry() -> TypeRegistry {
    let mut registry = TypeRegistry {
        types: IndexMap::new(),
        protocols: IndexMap::new(),
        functions: IndexMap::new(),
    };

    // ─── Builtin types ─────────────────────────────────────────────────────

    // Object: root of the hierarchy
    let object_type = TypeInfo {
        name: "Object".to_string(),
        params: Vec::new(),
        parent: None,
        attributes: IndexMap::new(),
        methods: IndexMap::new(),
        flattened_methods: IndexMap::new(),
        is_builtin_value: false,
        span: SourceSpan::new(0, 0),
    };
    registry.types.insert("Object".to_string(), object_type);

    // Number, String, Boolean – implicitly inherit Object, but are value types
    for name in ["Number", "String", "Boolean"] {
        let info = TypeInfo {
            name: name.to_string(),
            params: Vec::new(),
            parent: Some(ParentLink {
                name: "Object".to_string(),
                args: Vec::new(),
            }),
            attributes: IndexMap::new(),
            methods: IndexMap::new(),
            flattened_methods: IndexMap::new(),
            is_builtin_value: true, // prevents inheriting from them
            span: SourceSpan::new(0, 0),
        };
        registry.types.insert(name.to_string(), info);
    }

    // ─── Builtin protocols ────────────────────────────────────────────────

    // Iterable protocol
    let placeholder_t = Type::Named("T".to_string());
    let iterable_methods = IndexMap::from([
        (
            "next".to_string(),
            MethodSignature {
                params: Vec::new(),
                return_type: Type::Boolean,
                defined_in: "Iterable".to_string(),
                span: SourceSpan::new(0, 0),
            },
        ),
        (
            "current".to_string(),
            MethodSignature {
                params: Vec::new(),
                return_type: placeholder_t.clone(),
                defined_in: "Iterable".to_string(),
                span: SourceSpan::new(0, 0),
            },
        ),
    ]);
    let iterable_protocol = ProtocolInfo {
        name: "Iterable".to_string(),
        extends: Vec::new(),
        methods: iterable_methods,
        flattened_methods: IndexMap::new(),
        span: SourceSpan::new(0, 0),
    };
    registry
        .protocols
        .insert("Iterable".to_string(), iterable_protocol);

    // Enumerable protocol
    let enumerable_methods = IndexMap::from([(
        "iter".to_string(),
        MethodSignature {
            params: Vec::new(),
            return_type: Type::Iterable(Box::new(Type::Object)),
            defined_in: "Enumerable".to_string(),
            span: SourceSpan::new(0, 0),
        },
    )]);
    let enumerable_protocol = ProtocolInfo {
        name: "Enumerable".to_string(),
        extends: Vec::new(),
        methods: enumerable_methods,
        flattened_methods: IndexMap::new(),
        span: SourceSpan::new(0, 0),
    };
    registry
        .protocols
        .insert("Enumerable".to_string(), enumerable_protocol);

    // ─── Builtin type: Range ──────────────────────────────────────────────

    // Range(min: Number, max: Number) implements Iterable with current(): Number
    let range_type = TypeInfo {
        name: "Range".to_string(),
        params: vec![
            ("min".to_string(), Type::Number),
            ("max".to_string(), Type::Number),
        ],
        parent: Some(ParentLink {
            name: "Object".to_string(),
            args: Vec::new(),
        }),
        attributes: IndexMap::new(),
        methods: IndexMap::from([
            (
                "current".to_string(),
                MethodSignature {
                    params: Vec::new(),
                    return_type: Type::Number,
                    defined_in: "Range".to_string(),
                    span: SourceSpan::new(0, 0),
                },
            ),
            (
                "next".to_string(),
                MethodSignature {
                    params: Vec::new(),
                    return_type: Type::Boolean,
                    defined_in: "Range".to_string(),
                    span: SourceSpan::new(0, 0),
                },
            ),
        ]),
        flattened_methods: IndexMap::new(),
        is_builtin_value: false,
        span: SourceSpan::new(0, 0),
    };
    registry.types.insert("Range".to_string(), range_type);

    // ─── Builtin type: Vector ──────────────────────────────────────────────

    // Vector is a generic collection. Its method signatures use a placeholder
    // type `T` which will be substituted with the concrete element type.
    let vector_methods = IndexMap::from([
        (
            "size".to_string(),
            MethodSignature {
                params: Vec::new(),
                return_type: Type::Number,
                defined_in: "Vector".to_string(),
                span: SourceSpan::new(0, 0),
            },
        ),
        (
            "get".to_string(),
            MethodSignature {
                params: vec![("index".to_string(), Type::Number)],
                return_type: placeholder_t.clone(),
                defined_in: "Vector".to_string(),
                span: SourceSpan::new(0, 0),
            },
        ),
        (
            "set".to_string(),
            MethodSignature {
                params: vec![
                    ("index".to_string(), Type::Number),
                    ("value".to_string(), placeholder_t.clone()),
                ],
                return_type: Type::Object, // like Unit
                defined_in: "Vector".to_string(),
                span: SourceSpan::new(0, 0),
            },
        ),
        (
            "next".to_string(),
            MethodSignature {
                params: Vec::new(),
                return_type: Type::Boolean,
                defined_in: "Vector".to_string(),
                span: SourceSpan::new(0, 0),
            },
        ),
        (
            "current".to_string(),
            MethodSignature {
                params: Vec::new(),
                return_type: placeholder_t.clone(),
                defined_in: "Vector".to_string(),
                span: SourceSpan::new(0, 0),
            },
        ),
    ]);

    let vector_type = TypeInfo {
        name: "Vector".to_string(),
        params: Vec::new(), // Vector has no constructor params in this model
        parent: Some(ParentLink {
            name: "Object".to_string(),
            args: Vec::new(),
        }),
        attributes: IndexMap::new(),
        methods: vector_methods,
        flattened_methods: IndexMap::new(), // will be filled later, but we can copy now
        is_builtin_value: false,
        span: SourceSpan::new(0, 0),
    };
    registry.types.insert("Vector".to_string(), vector_type);

    // ─── Builtin functions ────────────────────────────────────────────────

    // print: (x: Object) -> Object
    registry.functions.insert(
        "print".to_string(),
        FunctionSignature {
            params: vec![("x".to_string(), Type::Object)],
            return_type: Type::Object,
            span: SourceSpan::new(0, 0),
            is_constant: false,
        },
    );

    // sqrt, sin, cos, exp: (x: Number) -> Number
    for name in ["sqrt", "sin", "cos", "exp"] {
        registry.functions.insert(
            name.to_string(),
            FunctionSignature {
                params: vec![("x".to_string(), Type::Number)],
                return_type: Type::Number,
                span: SourceSpan::new(0, 0),
                is_constant: false,
            },
        );
    }

    // log: (base: Number, x: Number) -> Number
    registry.functions.insert(
        "log".to_string(),
        FunctionSignature {
            params: vec![
                ("base".to_string(), Type::Number),
                ("x".to_string(), Type::Number),
            ],
            return_type: Type::Number,
            span: SourceSpan::new(0, 0),
            is_constant: false,
        },
    );

    // rand: () -> Number
    registry.functions.insert(
        "rand".to_string(),
        FunctionSignature {
            params: Vec::new(),
            return_type: Type::Number,
            span: SourceSpan::new(0, 0),
            is_constant: false,
        },
    );

    // range: (min: Number, max: Number) -> Range
    registry.functions.insert(
        "range".to_string(),
        FunctionSignature {
            params: vec![
                ("min".to_string(), Type::Number),
                ("max".to_string(), Type::Number),
            ],
            return_type: Type::Named("Range".to_string()),
            span: SourceSpan::new(0, 0),
            is_constant: false,
        },
    );

    // Constants PI and E: modeled as zero-argument functions returning Number
    for name in ["PI", "E"] {
        registry.functions.insert(
            name.to_string(),
            FunctionSignature {
                params: Vec::new(),
                return_type: Type::Number,
                span: SourceSpan::new(0, 0),
                is_constant: true, // treated as constants
            },
        );
    }

    registry
}

// -----------------------------------------------------------------------------
// Query helpers
// -----------------------------------------------------------------------------

impl TypeRegistry {
    /// Look up a type by name.
    pub fn lookup_type(&self, name: &str) -> Option<&TypeInfo> {
        self.types.get(name)
    }

    /// Mutable lookup for a type.
    pub fn lookup_type_mut(&mut self, name: &str) -> Option<&mut TypeInfo> {
        self.types.get_mut(name)
    }

    /// Look up a protocol by name.
    pub fn lookup_protocol(&self, name: &str) -> Option<&ProtocolInfo> {
        self.protocols.get(name)
    }

    /// Look up a function by name.
    pub fn lookup_function(&self, name: &str) -> Option<&FunctionSignature> {
        self.functions.get(name)
    }

    /// Returns a method table (flattened) for the given type, with any
    /// generic parameters substituted.
    ///
    /// For `Type::Named`, it looks up the type or protocol and returns a clone
    /// of its flattened methods.
    /// For `Type::Vector(inner)` and `Type::Iterable(inner)`, it retrieves the
    /// built‑in table for "Vector" or "Iterable" and substitutes `inner` for `T`.
    /// For other types, returns `None`.
    pub fn method_table_for(&self, ty: &Type) -> Option<IndexMap<String, MethodSignature>> {
        match ty {
            Type::Named(name) => {
                // Prefer type over protocol (they share a namespace anyway).
                if let Some(info) = self.lookup_type(name) {
                    if !info.flattened_methods.is_empty() {
                        return Some(info.flattened_methods.clone());
                    }
                    // Fallback to own methods (if flattening not yet done).
                    if !info.methods.is_empty() {
                        return Some(info.methods.clone());
                    }
                }
                if let Some(proto) = self.lookup_protocol(name) {
                    if !proto.flattened_methods.is_empty() {
                        return Some(proto.flattened_methods.clone());
                    }
                    if !proto.methods.is_empty() {
                        return Some(proto.methods.clone());
                    }
                }
                None
            }
            Type::Vector(inner) => self.lookup_type("Vector").and_then(|info| {
                if !info.flattened_methods.is_empty() {
                    Some(substitute_method_table(&info.flattened_methods, inner))
                } else if !info.methods.is_empty() {
                    Some(substitute_method_table(&info.methods, inner))
                } else {
                    None
                }
            }),
            Type::Iterable(inner) => self.lookup_protocol("Iterable").and_then(|proto| {
                if !proto.flattened_methods.is_empty() {
                    Some(substitute_method_table(&proto.flattened_methods, inner))
                } else if !proto.methods.is_empty() {
                    Some(substitute_method_table(&proto.methods, inner))
                } else {
                    None
                }
            }),
            _ => None,
        }
    }

    /// Convenience: look up a single method by name.
    pub fn lookup_method(&self, ty: &Type, name: &str) -> Option<MethodSignature> {
        self.method_table_for(ty)
            .and_then(|table| table.get(name).cloned())
    }

    /// Returns `true` if the given `Type` is a protocol name.
    pub fn is_protocol(&self, ty: &Type) -> bool {
        match ty {
            Type::Named(name) => self.protocols.contains_key(name),
            _ => false,
        }
    }

    /// Returns `true` if `ancestor` is a nominal ancestor of `descendant`.
    ///
    /// Walks the parent chain of `descendant` and checks if `ancestor`
    /// appears at any point. Handles builtin value types, which have
    /// `Object` as their parent.
    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
        let mut current = descendant;
        while let Some(info) = self.lookup_type(current) {
            if current == ancestor {
                return true;
            }
            if let Some(parent) = &info.parent {
                current = &parent.name;
            } else {
                break;
            }
        }
        // If ancestor is Object and we reached the root, we already
        // would have returned true when current == ancestor.
        false
    }

    /// Returns `true` if the type named `type_name` structurally
    /// implements the protocol named `protocol_name`.
    ///
    /// This implements the structural conformance rules of §A.10.4.
    /// For each method in the protocol's flattened method set, the type
    /// must have a method with:
    ///   - the same name
    ///   - the same number of parameters
    ///   - contravariant parameter types (protocol param <= type param)
    ///   - covariant return type (type return <= protocol return)
    pub fn implements_protocol(&self, type_name: &str, protocol_name: &str) -> bool {
        let type_info = match self.lookup_type(type_name) {
            Some(info) => info,
            None => return false,
        };
        let protocol_info = match self.lookup_protocol(protocol_name) {
            Some(info) => info,
            None => return false,
        };

        // Use the flattened method table of the type (includes inherited)
        // If not yet flattened (should have been done in Pass 1), fall back
        // to own methods.
        let type_methods = if !type_info.flattened_methods.is_empty() {
            &type_info.flattened_methods
        } else {
            &type_info.methods
        };

        // Use the flattened method table of the protocol (includes inherited).
        let proto_methods = if !protocol_info.flattened_methods.is_empty() {
            &protocol_info.flattened_methods
        } else {
            &protocol_info.methods
        };

        for (method_name, proto_sig) in proto_methods {
            let type_sig = match type_methods.get(method_name) {
                Some(sig) => sig,
                None => return false, // missing method
            };

            // Same arity
            if type_sig.params.len() != proto_sig.params.len() {
                return false;
            }

            // Check contravariance of parameters:
            // For each parameter, protocol param type P must conform to type param type T
            // i.e. P <= T
            for ((_, p_type), (_, t_type)) in proto_sig.params.iter().zip(&type_sig.params) {
                if !p_type.conforms_to(t_type, self) {
                    return false;
                }
            }

            // Check covariance of return type:
            // type return type R must conform to protocol return type P
            // i.e. R <= P
            if !type_sig
                .return_type
                .conforms_to(&proto_sig.return_type, self)
            {
                return false;
            }
        }

        // All methods matched and variance rules satisfied.
        true
    }

    /// Returns the parent name of a type, if any.
    pub fn parent_of(&self, type_name: &str) -> Option<String> {
        self.lookup_type(type_name)
            .and_then(|info| info.parent.as_ref())
            .map(|parent| parent.name.clone())
    }

    /// Returns `Ok(())` if `type_name` structurally implements `protocol_name`.
    /// Returns `Err(missing)` where `missing` is a list of method names that are
    /// either missing from the type or have incompatible signatures (arity, variance).
    pub fn protocol_conformance_details(
        &self,
        type_name: &str,
        protocol_name: &str,
    ) -> Result<(), Vec<String>> {
        let type_info = match self.lookup_type(type_name) {
            Some(info) => info,
            None => return Err(vec!["type not found".to_string()]),
        };
        let protocol_info = match self.lookup_protocol(protocol_name) {
            Some(info) => info,
            None => return Err(vec!["protocol not found".to_string()]),
        };

        let type_methods = if !type_info.flattened_methods.is_empty() {
            &type_info.flattened_methods
        } else {
            &type_info.methods
        };

        let mut missing = Vec::new();
        for (method_name, proto_sig) in &protocol_info.flattened_methods {
            if let Some(type_sig) = type_methods.get(method_name) {
                // Check arity and variance.
                if type_sig.params.len() != proto_sig.params.len() {
                    missing.push(method_name.clone());
                    continue;
                }
                // Check contravariance of parameters: protocol param <= type param.
                let mut ok = true;
                for ((_, p_type), (_, t_type)) in proto_sig.params.iter().zip(&type_sig.params) {
                    if !p_type.conforms_to(t_type, self) {
                        ok = false;
                        break;
                    }
                }
                if !ok {
                    missing.push(method_name.clone());
                    continue;
                }
                // Check covariance of return: type return <= protocol return.
                if !type_sig
                    .return_type
                    .conforms_to(&proto_sig.return_type, self)
                {
                    missing.push(method_name.clone());
                    continue;
                }
            } else {
                missing.push(method_name.clone());
            }
        }

        if missing.is_empty() {
            Ok(())
        } else {
            Err(missing)
        }
    }
}

// -----------------------------------------------------------------------------
// Generic substitution helpers
// -----------------------------------------------------------------------------

/// Replaces every occurrence of the formal generic parameter `param_name`
/// with `concrete` type, recursively walking through `Type`.
fn substitute_type(ty: &Type, param_name: &str, concrete: &Type) -> Type {
    match ty {
        Type::Named(name) if name == param_name => concrete.clone(),
        Type::Vector(inner) => Type::Vector(Box::new(substitute_type(inner, param_name, concrete))),
        Type::Iterable(inner) => {
            Type::Iterable(Box::new(substitute_type(inner, param_name, concrete)))
        }
        other => other.clone(),
    }
}

/// Substitutes the generic parameter `"T"` with `concrete` across an entire
/// method table, producing a new table with all signatures updated.
fn substitute_method_table(
    table: &IndexMap<String, MethodSignature>,
    concrete: &Type,
) -> IndexMap<String, MethodSignature> {
    let param = "T";
    table
        .iter()
        .map(|(name, sig)| {
            let substituted = MethodSignature {
                params: sig
                    .params
                    .iter()
                    .map(|(pname, pty)| (pname.clone(), substitute_type(pty, param, concrete)))
                    .collect(),
                return_type: substitute_type(&sig.return_type, param, concrete),
                defined_in: sig.defined_in.clone(),
                span: sig.span,
            };
            (name.clone(), substituted)
        })
        .collect()
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Type;

    // Helper: build a minimal registry with a protocol P (method f(): Number)
    // and a type T with optional custom methods.
    fn build_registry_with_protocol_and_type(
        type_methods: IndexMap<String, MethodSignature>,
    ) -> TypeRegistry {
        let mut registry = TypeRegistry {
            types: IndexMap::new(),
            protocols: IndexMap::new(),
            functions: IndexMap::new(),
        };

        // Protocol P: f(): Number
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
                flattened_methods: p_methods,
                span: SourceSpan::new(0, 0),
            },
        );

        // Type T with given methods.
        registry.types.insert(
            "T".to_string(),
            TypeInfo {
                name: "T".to_string(),
                params: Vec::new(),
                parent: None,
                attributes: IndexMap::new(),
                methods: type_methods.clone(),
                flattened_methods: type_methods,
                is_builtin_value: false,
                span: SourceSpan::new(0, 0),
            },
        );

        registry
    }

    #[test]
    fn implements_protocol_missing_method() {
        // Type T has no method `f`.
        let registry = build_registry_with_protocol_and_type(IndexMap::new());
        assert!(!registry.implements_protocol("T", "P"));
    }

    #[test]
    fn implements_protocol_wrong_arity() {
        // Type T has method `f` but with one parameter, while protocol expects zero.
        let mut methods = IndexMap::new();
        methods.insert(
            "f".to_string(),
            MethodSignature {
                params: vec![("x".to_string(), Type::Number)],
                return_type: Type::Number,
                defined_in: "T".to_string(),
                span: SourceSpan::new(0, 0),
            },
        );
        let registry = build_registry_with_protocol_and_type(methods);
        assert!(!registry.implements_protocol("T", "P"));
    }

    #[test]
    fn implements_protocol_contravariant_violation() {
        // Protocol P: f() expects no parameters.
        // Type T: f() with one parameter of type Number -> protocol param (none) <= type param (Number) is false?
        // Actually, for contravariance, protocol param type P must conform to type param type T.
        // Since there are no protocol params, there's nothing to check. To get a violation, we need a protocol
        // with a parameter and a type with a parameter of a type that does NOT conform.
        // Let's redefine: protocol P has f(x: Number), type T has f(x: String) -> String does not conform to Number.
        let mut registry = TypeRegistry {
            types: IndexMap::new(),
            protocols: IndexMap::new(),
            functions: IndexMap::new(),
        };

        let p_methods = IndexMap::from([(
            "f".to_string(),
            MethodSignature {
                params: vec![("x".to_string(), Type::Number)],
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
                flattened_methods: p_methods,
                span: SourceSpan::new(0, 0),
            },
        );

        let mut type_methods = IndexMap::new();
        type_methods.insert(
            "f".to_string(),
            MethodSignature {
                params: vec![("x".to_string(), Type::String)],
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
                parent: None,
                attributes: IndexMap::new(),
                methods: type_methods.clone(),
                flattened_methods: type_methods,
                is_builtin_value: false,
                span: SourceSpan::new(0, 0),
            },
        );

        // Protocol param (Number) must conform to type param (String) -> false.
        assert!(!registry.implements_protocol("T", "P"));
    }

    #[test]
    fn implements_protocol_covariant_violation() {
        // Protocol P: f(): Number
        // Type T: f(): String -> String does not conform to Number.
        let mut methods = IndexMap::new();
        methods.insert(
            "f".to_string(),
            MethodSignature {
                params: Vec::new(),
                return_type: Type::String,
                defined_in: "T".to_string(),
                span: SourceSpan::new(0, 0),
            },
        );
        let registry = build_registry_with_protocol_and_type(methods);
        assert!(!registry.implements_protocol("T", "P"));
    }

    #[test]
    fn is_ancestor_self_is_true() {
        let mut registry = seeded_registry();
        // Insert a user type T.
        registry.types.insert(
            "T".to_string(),
            TypeInfo {
                name: "T".to_string(),
                params: Vec::new(),
                parent: None,
                attributes: IndexMap::new(),
                methods: IndexMap::new(),
                flattened_methods: IndexMap::new(),
                is_builtin_value: false,
                span: SourceSpan::new(0, 0),
            },
        );
        assert!(registry.is_ancestor("T", "T"));
        // Also for builtins:
        assert!(registry.is_ancestor("Number", "Number"));
        assert!(registry.is_ancestor("Object", "Object"));
    }

    // ---- Generic method table substitution ----

    #[test]
    fn method_table_for_vector_substitutes_type() {
        let registry = seeded_registry();
        let vec_num = Type::Vector(Box::new(Type::Number));
        let table = registry.method_table_for(&vec_num).unwrap();

        let current_sig = table.get("current").expect("Vector should have current()");
        assert_eq!(
            current_sig.return_type,
            Type::Number,
            "current() return type should be Number"
        );

        let get_sig = table.get("get").expect("Vector should have get()");
        assert_eq!(
            get_sig.params[0].1,
            Type::Number,
            "index type should be Number"
        );
        assert_eq!(
            get_sig.return_type,
            Type::Number,
            "get() return type should be Number"
        );

        let set_sig = table.get("set").expect("Vector should have set()");
        assert_eq!(
            set_sig.params[1].1,
            Type::Number,
            "value type should be Number"
        );
    }

    #[test]
    fn method_table_for_iterable_substitutes_type() {
        let registry = seeded_registry();
        let iter_str = Type::Iterable(Box::new(Type::String));
        let table = registry.method_table_for(&iter_str).unwrap();

        let current_sig = table
            .get("current")
            .expect("Iterable should have current()");
        assert_eq!(
            current_sig.return_type,
            Type::String,
            "current() return type should be String"
        );

        let next_sig = table.get("next").expect("Iterable should have next()");
        assert_eq!(
            next_sig.return_type,
            Type::Boolean,
            "next() return type should be Boolean"
        );
    }

    #[test]
    fn method_table_for_named_type_returns_own_methods() {
        let mut registry = seeded_registry();
        // Insert a user type with a method.
        let mut methods = IndexMap::new();
        methods.insert(
            "foo".to_string(),
            MethodSignature {
                params: Vec::new(),
                return_type: Type::Number,
                defined_in: "User".to_string(),
                span: SourceSpan::new(0, 0),
            },
        );
        registry.types.insert(
            "User".to_string(),
            TypeInfo {
                name: "User".to_string(),
                params: Vec::new(),
                parent: None,
                attributes: IndexMap::new(),
                methods: methods.clone(),
                flattened_methods: methods,
                is_builtin_value: false,
                span: SourceSpan::new(0, 0),
            },
        );
        let table = registry
            .method_table_for(&Type::Named("User".to_string()))
            .unwrap();
        assert!(table.contains_key("foo"));
    }
}
