//! Lexical scope stack for local variables.

use std::collections::HashMap;

use hulk_semantic::Type;
use inkwell::types::BasicTypeEnum;
use inkwell::values::PointerValue;

/// A stack of scopes, each mapping a variable name to its LLVM `alloca` pointer.
///
/// Each entry carries an `owned` flag. Only owned bindings (let variables,
/// constructor arguments) are released by `pop_scope`; borrowed bindings
/// (`self`, method/function parameters, loop variables) are skipped.
#[derive(Default)]
pub struct ScopeStack<'ctx> {
    scopes: Vec<HashMap<String, (PointerValue<'ctx>, BasicTypeEnum<'ctx>, Type, bool)>>,
    /// For each scope, the ordered list of alloca addresses that were shadow-pushed.
    /// Used by pop_scope to emit shadow pops in reverse (LIFO) order.
    shadow_slots: Vec<Vec<PointerValue<'ctx>>>,
}

impl<'ctx> ScopeStack<'ctx> {
    pub fn new() -> Self {
        Self {
            scopes: Vec::new(),
            shadow_slots: Vec::new(),
        }
    }

    /// Pushes a new empty scope.
    pub fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
        self.shadow_slots.push(Vec::new()); // parallel shadow list
    }

    /// Pops the innermost scope and returns its bindings.
    ///
    /// # Panics
    /// Panics if there is no scope to pop (i.e., the stack is empty).
    pub fn pop_scope(
        &mut self,
    ) -> (
        HashMap<String, (PointerValue<'ctx>, BasicTypeEnum<'ctx>, Type, bool)>,
        Vec<PointerValue<'ctx>>,
    ) {
        let scope = self.scopes.pop().expect("scope stack underflow");
        let shadow = self.shadow_slots.pop().expect("shadow stack underflow");
        (scope, shadow)
    }

    /// Declares a variable in the innermost scope.
    ///
    /// `owned`: if `true`, the binding owns its heap reference and will be
    /// released when the scope exits. Pass `false` for borrowed bindings
    /// (`self`, function/method parameters, loop variables) that must not be
    /// released by the callee — the caller already owns that reference.
    ///
    /// `shadow_slot`: if `Some(ptr)`, the alloca address was shadow-pushed and
    /// must be popped when this scope exits. Pass `None` for numeric/boolean
    /// locals that are not GC roots.
    /// 
    /// Overwrites any existing binding with the same name in that scope.
    pub fn declare(
        &mut self,
        name: &str,
        ptr: PointerValue<'ctx>,
        llvm_ty: BasicTypeEnum<'ctx>,
        sem_ty: Type,
        owned: bool,
        shadow_slot: Option<PointerValue<'ctx>>,
    ) {
        let scope = self.scopes.last_mut().expect("no scope to declare into");
        scope.insert(name.to_string(), (ptr, llvm_ty, sem_ty, owned));
        if let Some(slot) = shadow_slot {
            self.shadow_slots
                .last_mut()
                .expect("no shadow list")
                .push(slot);
        }
    }

    /// Looks up a variable starting from the innermost scope outward.
    pub fn lookup(
        &self,
        name: &str,
    ) -> Option<(PointerValue<'ctx>, BasicTypeEnum<'ctx>, Type)> {
        for scope in self.scopes.iter().rev() {
            if let Some((ptr, llvm_ty, sem_ty, _owned)) = scope.get(name) {
                return Some((*ptr, *llvm_ty, sem_ty.clone()));
            }
        }
        None
    }
}
