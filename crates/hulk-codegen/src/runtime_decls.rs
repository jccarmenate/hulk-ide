//! Declarations of `hulk-rt` symbols as external LLVM functions.
//!
//! These are used by the lowering code to call into the runtime library.

use inkwell::values::FunctionValue;

use crate::context::CodegenCtx;
use crate::error::CodegenError;

// WHY: get-or-declare pattern — prevents duplicate LLVM symbols when the same
// runtime function is declared multiple times across lowering passes.
// inkwell's add_function() always creates a new entry, mangling the name to
// hulk_rt_X.1, .2 etc. if called twice. get_function() returns the existing one.

/// Declares `hulk_rt_alloc(size: i64) -> ptr`.
pub fn declare_alloc<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_alloc") {
        return f;
    }
    let i64_type = ctx.context.i64_type();
    let ptr_type = ctx.context.ptr_type(Default::default());
    let fn_type = ptr_type.fn_type(&[i64_type.into()], false);
    ctx.module.add_function("hulk_rt_alloc", fn_type, None)
}

/// Declares `hulk_rt_retain(ptr) -> void`.
pub fn declare_retain<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_retain") {
        return f;
    }
    let ptr_type = ctx.context.ptr_type(Default::default());
    let void_type = ctx.context.void_type();
    let fn_type = void_type.fn_type(&[ptr_type.into()], false);
    ctx.module.add_function("hulk_rt_retain", fn_type, None)
}

/// Declares `hulk_rt_release(ptr) -> void`.
pub fn declare_release<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_release") {
        return f;
    }
    let ptr_type = ctx.context.ptr_type(Default::default());
    let void_type = ctx.context.void_type();
    let fn_type = void_type.fn_type(&[ptr_type.into()], false);
    ctx.module.add_function("hulk_rt_release", fn_type, None)
}

/// Declares `hulk_rt_string_concat(a: ptr, b: ptr) -> ptr`.
pub fn declare_string_concat<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_string_concat") {
        return f;
    }
    let ptr_type = ctx.context.ptr_type(Default::default());
    let fn_type = ptr_type.fn_type(&[ptr_type.into(), ptr_type.into()], false);
    ctx.module
        .add_function("hulk_rt_string_concat", fn_type, None)
}

/// Declares `hulk_rt_string_concat_space(a: ptr, b: ptr) -> ptr`.
pub fn declare_string_concat_space<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_string_concat_space") {
        return f;
    }
    let ptr_type = ctx.context.ptr_type(Default::default());
    let fn_type = ptr_type.fn_type(&[ptr_type.into(), ptr_type.into()], false);
    ctx.module
        .add_function("hulk_rt_string_concat_space", fn_type, None)
}

/// Declares `hulk_rt_number_to_string(num: f64) -> ptr`.
pub fn declare_number_to_string<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_number_to_string") {
        return f;
    }
    let f64_type = ctx.context.f64_type();
    let ptr_type = ctx.context.ptr_type(Default::default());
    let fn_type = ptr_type.fn_type(&[f64_type.into()], false);
    ctx.module
        .add_function("hulk_rt_number_to_string", fn_type, None)
}

/// Declares `hulk_rt_bool_to_string(b: i1) -> ptr`.
pub fn declare_bool_to_string<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_bool_to_string") {
        return f;
    }
    let bool_type = ctx.context.bool_type();
    let ptr_type = ctx.context.ptr_type(Default::default());
    let fn_type = ptr_type.fn_type(&[bool_type.into()], false);
    ctx.module
        .add_function("hulk_rt_bool_to_string", fn_type, None)
}

/// Declares `hulk_rt_downcast_check(obj: ptr, target_vtable: ptr) -> i1`.
pub fn declare_downcast_check<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_downcast_check") {
        return f;
    }
    let ptr_type = ctx.context.ptr_type(Default::default());
    let bool_type = ctx.context.bool_type();
    let fn_type = bool_type.fn_type(&[ptr_type.into(), ptr_type.into()], false);
    ctx.module
        .add_function("hulk_rt_downcast_check", fn_type, None)
}

/// Declares `hulk_rt_downcast_fail() -> !` (noreturn).
pub fn declare_downcast_fail<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_downcast_fail") {
        return f;
    }
    let void_type = ctx.context.void_type();
    let fn_type = void_type.fn_type(&[], false);
    ctx.module
        .add_function("hulk_rt_downcast_fail", fn_type, None)
}

/// Declares `hulk_rt_internal_error() -> !` (noreturn).
pub fn declare_internal_error<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_internal_error") { return f; }
    let void_type = ctx.context.void_type();
    let fn_type = void_type.fn_type(&[], false);
    ctx.module.add_function("hulk_rt_internal_error", fn_type, None)
}

// ─── Vector builtin methods ───────────────────────────────────────────────

/// Declares `hulk_rt_vector_new(len: i64) -> ptr`.
pub fn declare_vector_new<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_vector_new") {
        return f;
    }
    let i64_type = ctx.context.i64_type();
    let ptr_type = ctx.context.ptr_type(Default::default());
    let fn_type = ptr_type.fn_type(&[i64_type.into()], false);
    ctx.module.add_function("hulk_rt_vector_new", fn_type, None)
}

/// Declares `hulk_rt_vector_size(vec: ptr) -> i64`.
pub fn declare_vector_size<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_vector_size") {
        return f;
    }
    let ptr_type = ctx.context.ptr_type(Default::default());
    let f64_type = ctx.context.f64_type();
    let fn_type = f64_type.fn_type(&[ptr_type.into()], false);
    ctx.module
        .add_function("hulk_rt_vector_size", fn_type, None)
}

/// Declares `hulk_rt_vector_get(vec: ptr, index: i64) -> ptr`.
pub fn declare_vector_get<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_vector_get") {
        return f;
    }
    let ptr_type = ctx.context.ptr_type(Default::default());
    let i64_type = ctx.context.i64_type();
    let fn_type = ptr_type.fn_type(&[ptr_type.into(), i64_type.into()], false);
    ctx.module.add_function("hulk_rt_vector_get", fn_type, None)
}

/// Declares `hulk_rt_vector_set(vec: ptr, index: i64, value: ptr) -> void`.
pub fn declare_vector_set<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_vector_set") {
        return f;
    }
    let ptr_type = ctx.context.ptr_type(Default::default());
    let i64_type = ctx.context.i64_type();
    let void_type = ctx.context.void_type();
    let fn_type = void_type.fn_type(&[ptr_type.into(), i64_type.into(), ptr_type.into()], false);
    ctx.module.add_function("hulk_rt_vector_set", fn_type, None)
}

/// Declares `hulk_rt_vector_next(vec: ptr) -> i1`.
pub fn declare_vector_next<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_vector_next") {
        return f;
    }
    let ptr_type = ctx.context.ptr_type(Default::default());
    let bool_type = ctx.context.bool_type();
    let fn_type = bool_type.fn_type(&[ptr_type.into()], false);
    ctx.module
        .add_function("hulk_rt_vector_next", fn_type, None)
}

/// Declares `hulk_rt_vector_current(vec: ptr) -> ptr`.
pub fn declare_vector_current<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_vector_current") {
        return f;
    }
    let ptr_type = ctx.context.ptr_type(Default::default());
    let fn_type = ptr_type.fn_type(&[ptr_type.into()], false);
    ctx.module
        .add_function("hulk_rt_vector_current", fn_type, None)
}

// ─── Range builtin methods ────────────────────────────────────────────────

/// Declares `hulk_rt_range_new(min: f64, max: f64) -> ptr`.
pub fn declare_range_new<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_range_new") {
        return f;
    }
    let f64_type = ctx.context.f64_type();
    let ptr_type = ctx.context.ptr_type(Default::default());
    let fn_type = ptr_type.fn_type(&[f64_type.into(), f64_type.into()], false);
    ctx.module.add_function("hulk_rt_range_new", fn_type, None)
}

/// Declares `hulk_rt_range_next(rng: ptr) -> i1`.
pub fn declare_range_next<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_range_next") {
        return f;
    }
    let ptr_type = ctx.context.ptr_type(Default::default());
    let bool_type = ctx.context.bool_type();
    let fn_type = bool_type.fn_type(&[ptr_type.into()], false);
    ctx.module.add_function("hulk_rt_range_next", fn_type, None)
}

/// Declares `hulk_rt_range_current(rng: ptr) -> f64`.
pub fn declare_range_current<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_range_current") {
        return f;
    }
    let ptr_type = ctx.context.ptr_type(Default::default());
    let f64_type = ctx.context.f64_type();
    let fn_type = f64_type.fn_type(&[ptr_type.into()], false);
    ctx.module
        .add_function("hulk_rt_range_current", fn_type, None)
}

// ─── Closure environments ──────────────────────────────────────────────────

/// Declares `hulk_rt_env_new(slot_count: i64, field_map: ptr) -> ptr`.
pub fn declare_env_new<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_env_new") {
        return f;
    }
    let i64_type = ctx.context.i64_type();
    let ptr_type = ctx.context.ptr_type(Default::default());
    let fn_type = ptr_type.fn_type(&[i64_type.into(), ptr_type.into()], false);
    ctx.module.add_function("hulk_rt_env_new", fn_type, None)
} 

// ─── Match fail trap ──────────────────────────────────────────────────────

/// Declares `hulk_rt_match_fail() -> !` (noreturn).
pub fn declare_match_fail<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_match_fail") {
        return f;
    }
    let void_type = ctx.context.void_type();
    let fn_type = void_type.fn_type(&[], false);
    ctx.module.add_function("hulk_rt_match_fail", fn_type, None)
}

// ─── String equality ──────────────────────────────────────────────────────

/// Declares `hulk_rt_string_equals(a: ptr, b: ptr) -> i1`.
pub fn declare_string_equals<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_string_equals") {
        return f;
    }
    let ptr_type = ctx.context.ptr_type(Default::default());
    let bool_type = ctx.context.bool_type();
    let fn_type = bool_type.fn_type(&[ptr_type.into(), ptr_type.into()], false);
    ctx.module
        .add_function("hulk_rt_string_equals", fn_type, None)
}

// ─── Dynamic vector helpers for comprehensions ──────────────────────────

/// Declares `hulk_rt_dynamic_vector_new() -> ptr`.
pub fn declare_dynamic_vector_new<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_dynamic_vector_new") {
        return f;
    }
    let ptr_type = ctx.context.ptr_type(Default::default());
    let fn_type = ptr_type.fn_type(&[], false);
    ctx.module
        .add_function("hulk_rt_dynamic_vector_new", fn_type, None)
}

/// Declares `hulk_rt_dynamic_vector_append(vec: ptr, value: ptr) -> void`.
pub fn declare_dynamic_vector_append<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_dynamic_vector_append") {
        return f;
    }
    let void_type = ctx.context.void_type();
    let ptr_type = ctx.context.ptr_type(Default::default());
    let fn_type = void_type.fn_type(&[ptr_type.into(), ptr_type.into()], false);
    ctx.module
        .add_function("hulk_rt_dynamic_vector_append", fn_type, None)
}

/// Declares `hulk_rt_dynamic_vector_to_vector(vec: ptr) -> ptr`.
pub fn declare_dynamic_vector_to_vector<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_dynamic_vector_to_vector") {
        return f;
    }
    let ptr_type = ctx.context.ptr_type(Default::default());
    let fn_type = ptr_type.fn_type(&[ptr_type.into()], false);
    ctx.module
        .add_function("hulk_rt_dynamic_vector_to_vector", fn_type, None)
}

// ─── Print ──────────────────────────────────────────────────────────────────

/// Declares `hulk_rt_print(obj: ptr) -> ptr`.
pub fn declare_print<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_print") {
        return f;
    }
    let ptr_type = ctx.context.ptr_type(Default::default());
    let fn_type = ptr_type.fn_type(&[ptr_type.into()], false);
    ctx.module.add_function("hulk_rt_print", fn_type, None)
}

// ─── Math Builtin Functions ───────────────────────────────────────────────────

/// Declares `hulk_rt_sqrt(x: f64) -> f64`.
pub fn declare_sqrt<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_sqrt") {
        return f;
    }
    let f64_type = ctx.context.f64_type();
    let fn_type = f64_type.fn_type(&[f64_type.into()], false);
    ctx.module.add_function("hulk_rt_sqrt", fn_type, None)
}

/// Declares `hulk_rt_sin(x: f64) -> f64`.
pub fn declare_sin<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_sin") {
        return f;
    }
    let f64_type = ctx.context.f64_type();
    let fn_type = f64_type.fn_type(&[f64_type.into()], false);
    ctx.module.add_function("hulk_rt_sin", fn_type, None)
}

/// Declares `hulk_rt_cos(x: f64) -> f64`.
pub fn declare_cos<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_cos") {
        return f;
    }
    let f64_type = ctx.context.f64_type();
    let fn_type = f64_type.fn_type(&[f64_type.into()], false);
    ctx.module.add_function("hulk_rt_cos", fn_type, None)
}

/// Declares `hulk_rt_exp(x: f64) -> f64`.
pub fn declare_exp<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_exp") {
        return f;
    }
    let f64_type = ctx.context.f64_type();
    let fn_type = f64_type.fn_type(&[f64_type.into()], false);
    ctx.module.add_function("hulk_rt_exp", fn_type, None)
}

/// Declares `hulk_rt_log(base: f64, x: f64) -> f64`.
pub fn declare_log<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_log") {
        return f;
    }
    let f64_type = ctx.context.f64_type();
    let fn_type = f64_type.fn_type(&[f64_type.into(), f64_type.into()], false);
    ctx.module.add_function("hulk_rt_log", fn_type, None)
}

/// Declares `hulk_rt_rand() -> f64`.
pub fn declare_rand<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_rand") {
        return f;
    }
    let f64_type = ctx.context.f64_type();
    let fn_type = f64_type.fn_type(&[], false);
    ctx.module.add_function("hulk_rt_rand", fn_type, None)
}

// ─── GC shadow stack ──────────────────────────────────────────────────────

/// Declares `hulk_rt_shadow_push(slot: ptr) -> void`.
///
/// `slot` is the address of a pointer-typed local's `alloca` slot (a `ptr`
/// that holds a `ptr`). The GC dereferences it during the mark phase to
/// find the live object. Passing the alloca address makes it escape the
/// function, intentionally preventing mem2reg from promoting the slot.
pub fn declare_shadow_push<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_shadow_push") {
        return f;
    }
    let ptr_type = ctx.context.ptr_type(Default::default());
    let void_type = ctx.context.void_type();
    // Signature: (slot: *mut ptr) -> void
    let fn_type = void_type.fn_type(&[ptr_type.into()], false);
    ctx.module.add_function("hulk_rt_shadow_push", fn_type, None)
}

/// Declares `hulk_rt_shadow_pop() -> void`.
///
/// Removes the most-recently-pushed entry from the shadow stack. Must be
/// called once for every prior `hulk_rt_shadow_push` call, in reverse
/// (LIFO) order, exactly mirroring scope exit.
pub fn declare_shadow_pop<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_shadow_pop") {
        return f;
    }
    let void_type = ctx.context.void_type();
    let fn_type = void_type.fn_type(&[], false);
    ctx.module.add_function("hulk_rt_shadow_pop", fn_type, None)
}

/// Declares `hulk_rt_gc_collect() -> void`.
///
/// Runs a full mark-sweep cycle. The runtime calls this automatically from
/// `hulk_rt_alloc` once the allocation byte-counter crosses the threshold;
/// this declaration exists so the function can also be called explicitly
/// in tests or for forced collection at program exit.
pub fn declare_gc_collect<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("hulk_rt_gc_collect") {
        return f;
    }
    let void_type = ctx.context.void_type();
    let fn_type = void_type.fn_type(&[], false);
    ctx.module.add_function("hulk_rt_gc_collect", fn_type, None)
}

// ─── Group Declarations ──────────────────────────────────────────────────────

/// Returns the existing declaration for `name` if this module already has
/// one, or declares it now (caching the result) if not.
///
/// Several runtime symbols are only needed by some HULK programs.
/// This mirrors the reachability pruning in `itables::build_itables`.
pub fn ensure_decl<'ctx>(
    ctx: &mut CodegenCtx<'ctx>,
    name: &str,
) -> Result<inkwell::values::FunctionValue<'ctx>, CodegenError> {
    if let Some(f) = ctx.functions.get(name) {
        return Ok(*f);
    }
    let f = match name {
        "hulk_rt_match_fail" => declare_match_fail(ctx),
        "hulk_rt_downcast_check" => declare_downcast_check(ctx),
        "hulk_rt_downcast_fail" => declare_downcast_fail(ctx),
        "hulk_rt_internal_error" => declare_internal_error(ctx),
        "hulk_rt_string_equals" => declare_string_equals(ctx),
        "hulk_rt_dynamic_vector_new" => declare_dynamic_vector_new(ctx),
        "hulk_rt_dynamic_vector_append" => declare_dynamic_vector_append(ctx),
        "hulk_rt_dynamic_vector_to_vector" => declare_dynamic_vector_to_vector(ctx),
        other => {
            return Err(CodegenError::unsupported(
                format!("no on-demand declaration recipe for `{other}`"),
                None,
            ));
        }
    };
    ctx.functions.insert(name.to_string(), f);
    Ok(f)
}

/// Declares all standard runtime functions and inserts them into `ctx.functions`.
///
/// Each function is inserted under two names where applicable:
/// - The qualified name `"Type::method"` for itable/vtable dispatch.
/// - The raw runtime symbol name `"hulk_rt_*"` for direct calls.
pub fn declare_all(ctx: &mut CodegenCtx) {
    // ─── Memory management ──────────────────────────────────────────

    let alloc = declare_alloc(ctx);
    ctx.functions.insert("hulk_rt_alloc".to_string(), alloc);

    let retain = declare_retain(ctx);
    ctx.functions.insert("hulk_rt_retain".to_string(), retain);

    let release = declare_release(ctx);
    ctx.functions.insert("hulk_rt_release".to_string(), release);

    // ─── Basic runtime functions ──────────────────────────────────────────

    let concat = declare_string_concat(ctx);
    ctx.functions
        .insert("hulk_rt_string_concat".to_string(), concat);

    let concat_space = declare_string_concat_space(ctx);
    ctx.functions
        .insert("hulk_rt_string_concat_space".to_string(), concat_space);

    let num_to_str = declare_number_to_string(ctx);
    ctx.functions
        .insert("hulk_rt_number_to_string".to_string(), num_to_str);

    let bool_to_str = declare_bool_to_string(ctx);
    ctx.functions
        .insert("hulk_rt_bool_to_string".to_string(), bool_to_str);

    let str_eq = declare_string_equals(ctx);
    ctx.functions
        .insert("hulk_rt_string_equals".to_string(), str_eq);

    let internal_error = declare_internal_error(ctx);
    ctx.functions.insert("hulk_rt_internal_error".to_string(), internal_error);

    // ─── Vector builtin methods ───────────────────────────────────────────

    let vec_new = declare_vector_new(ctx);
    ctx.functions.insert("Vector::new".to_string(), vec_new);
    ctx.functions
        .insert("hulk_rt_vector_new".to_string(), vec_new);

    let vec_size = declare_vector_size(ctx);
    ctx.functions.insert("Vector::size".to_string(), vec_size);
    ctx.functions
        .insert("hulk_rt_vector_size".to_string(), vec_size);

    let vec_get = declare_vector_get(ctx);
    ctx.functions.insert("Vector::get".to_string(), vec_get);
    ctx.functions
        .insert("hulk_rt_vector_get".to_string(), vec_get);

    let vec_set = declare_vector_set(ctx);
    ctx.functions.insert("Vector::set".to_string(), vec_set);
    ctx.functions
        .insert("hulk_rt_vector_set".to_string(), vec_set);

    let vec_next = declare_vector_next(ctx);
    ctx.functions.insert("Vector::next".to_string(), vec_next);
    ctx.functions
        .insert("hulk_rt_vector_next".to_string(), vec_next);

    let vec_current = declare_vector_current(ctx);
    ctx.functions
        .insert("Vector::current".to_string(), vec_current);
    ctx.functions
        .insert("hulk_rt_vector_current".to_string(), vec_current);

    // ─── Range builtin methods ────────────────────────────────────────────

    let range_new = declare_range_new(ctx);
    ctx.functions
        .insert("hulk_rt_range_new".to_string(), range_new);
    ctx.functions.insert("Range::new".to_string(), range_new); // optional for consistency

    let range_next = declare_range_next(ctx);
    ctx.functions.insert("Range::next".to_string(), range_next);
    ctx.functions
        .insert("hulk_rt_range_next".to_string(), range_next);

    let range_current = declare_range_current(ctx);
    ctx.functions
        .insert("Range::current".to_string(), range_current);
    ctx.functions
        .insert("hulk_rt_range_current".to_string(), range_current);

    // ─── Closure environments ─────────────────────────────────────────────────────

    let env_new = declare_env_new(ctx);
    ctx.functions.insert("hulk_rt_env_new".to_string(), env_new);

    // ─── Print ─────────────────────────────────────────────────────────────

    let print_fn = declare_print(ctx);
    ctx.functions.insert("hulk_rt_print".to_string(), print_fn);

    // ─── Math functions ──────────────────────────────────────────────

    let sqrt_fn = declare_sqrt(ctx);
    ctx.functions.insert("hulk_rt_sqrt".to_string(), sqrt_fn);

    let sin_fn = declare_sin(ctx);
    ctx.functions.insert("hulk_rt_sin".to_string(), sin_fn);

    let cos_fn = declare_cos(ctx);
    ctx.functions.insert("hulk_rt_cos".to_string(), cos_fn);

    let exp_fn = declare_exp(ctx);
    ctx.functions.insert("hulk_rt_exp".to_string(), exp_fn);

    let log_fn = declare_log(ctx);
    ctx.functions.insert("hulk_rt_log".to_string(), log_fn);

    let rand_fn = declare_rand(ctx);
    ctx.functions.insert("hulk_rt_rand".to_string(), rand_fn);

    // ─── GC shadow stack ─────────────────────────────────────────────────────

    let shadow_push = declare_shadow_push(ctx);
    ctx.functions
        .insert("hulk_rt_shadow_push".to_string(), shadow_push);

    let shadow_pop = declare_shadow_pop(ctx);
    ctx.functions
        .insert("hulk_rt_shadow_pop".to_string(), shadow_pop);

    let gc_collect = declare_gc_collect(ctx);
    ctx.functions
        .insert("hulk_rt_gc_collect".to_string(), gc_collect);
}
