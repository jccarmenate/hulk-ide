//! Shared mutable state threaded through every lowering function.

use std::collections::HashMap;

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::targets::TargetMachine;
use inkwell::values::{FunctionValue, GlobalValue};

use crate::error::CodegenError;
use crate::layout::TypeLayout;
use crate::options::OptLevel;

/// Everything a lowering function needs: the LLVM context that owns every
/// type and value it creates, the module being built, the instruction
/// builder, and the symbol tables accumulated so far.
///
/// At this stage only `functions` exists. Later phases add a `types` table
/// (per-type struct layouts and vtables) and a lexical scope stack mirroring
/// `hulk_semantic::Environment`'s push/pop discipline.
pub struct CodegenCtx<'ctx> {
    pub context: &'ctx Context,
    pub module: Module<'ctx>,
    pub builder: Builder<'ctx>,
    pub functions: HashMap<String, FunctionValue<'ctx>>,
    /// Monotonically increasing id used to give every string-literal global a unique name
    string_literal_count: u32,
    /// Monotonically increasing id used to give every lambda function a unique name
    lambda_count: u32,
    /// Type layouts for user‑defined classes.
    pub type_layouts: HashMap<String, TypeLayout<'ctx>>,
    /// The target machine this module is built for. Owned here rather than re-created
    /// at emission time) so struct-layout queries in `layout.rs` and the final
    /// `write_object_file` call always agree on exactly the same data layout — there's
    /// only ever one `TargetMachine` per compilation.
    pub target_machine: TargetMachine,
    /// Itable globals: (type_name, protocol_name) -> GlobalValue
    pub itables: HashMap<(String, String), GlobalValue<'ctx>>,
}

impl<'ctx> CodegenCtx<'ctx> {
    /// Creates a new codegen context targeting Linux x86_64.
    ///
    /// `opt` controls both the `TargetMachine` backend optimisation level and
    /// is stored for later use by `compile()` when invoking the IR pass
    /// pipeline. Pass `OptLevel::None` for smoke tests and unit tests that
    /// do not run the optimizer.
    pub fn new(
        context: &'ctx Context,
        module_name: &str,
        opt: OptLevel,
    ) -> Result<Self, CodegenError> {
        let module = context.create_module(module_name);
        let builder = context.create_builder();
        crate::emit::init_all_targets()?;
        let target_machine = crate::emit::linux_x86_64_target_machine(opt)?;
        module.set_triple(&target_machine.get_triple());
        module.set_data_layout(&target_machine.get_target_data().get_data_layout());
        Ok(Self {
            context,
            module,
            builder,
            functions: HashMap::new(),
            string_literal_count: 0,
            lambda_count: 0,
            type_layouts: HashMap::new(),
            target_machine,
            itables: HashMap::new(),
        })
    }

    pub fn next_string_literal_id(&mut self) -> u32 {
        let id = self.string_literal_count;
        self.string_literal_count += 1;
        id
    }

    pub fn next_lambda_id(&mut self) -> u32 {
        let id = self.lambda_count;
        self.lambda_count += 1;
        id
    }
}
