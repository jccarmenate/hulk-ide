//! Lowering of literal expressions to LLVM constants.
//!
//! This module handles the three kinds of literals in HULK:
//! - `Number`: 64‑bit floating‑point constants (`f64`).
//! - `Boolean`: `true` and `false` as `i1` (1‑bit integer) values.
//! - `String`: emitted as a pair of global constants:
//!   1. A global byte array containing the string data.
//!   2. A global `HulkString` struct (`{ len: i64, data: ptr }`) referencing that data.
//!
//! String literals are immutable and are never modified at runtime; they are
//! emitted once per unique string in the module.

use std::u64;

use inkwell::module::Linkage;

use hulk_ast::Literal;

use crate::error::CodegenError;
use crate::lower::LowerCtx;
use hulk_rt::TAG_LITERAL_STRING;

/// Lowers a literal expression to an LLVM constant value.
///
/// # Parameters
/// - `ctx`: the lowering context.
/// - `lit`: the literal AST node.
///
/// # Returns
/// - `Number`: an `f64` constant.
/// - `Boolean`: an `i1` constant (1 for `true`, 0 for `false`).
/// - `String`: a pointer to a global `HulkString` struct.
///
/// # Errors
/// This function never fails for valid literals; all errors are from LLVM
/// constant construction (which is infallible in this context).
pub fn lower_literal<'ctx>(
    ctx: &mut LowerCtx<'_, 'ctx>,
    lit: &Literal,
) -> Result<inkwell::values::BasicValueEnum<'ctx>, CodegenError> {
    match lit {
        Literal::Number(n) => {
            let float = ctx.codegen.context.f64_type().const_float(*n);
            Ok(float.into())
        }
        Literal::Boolean(b) => {
            let bool_val = ctx
                .codegen
                .context
                .bool_type()
                .const_int(if *b { 1 } else { 0 }, false);
            Ok(bool_val.into())
        }
        Literal::String(s) => {
            // ─── String data global ──────────────────────────────────────
            let id = ctx.codegen.next_string_literal_id();
            let data_bytes = s.as_bytes();
            let data_const = ctx.codegen.context.const_string(data_bytes, false);
            let data_global = ctx.codegen.module.add_global(
                data_const.get_type(),
                None,
                &format!("str_data_{id}"),
            );
            data_global.set_initializer(&data_const);
            data_global.set_constant(true);
            data_global.set_linkage(Linkage::Private);
            data_global.set_unnamed_addr(true);

            // ─── HulkString header global ────────────────────────────────────
            // Must be byte-for-byte compatible with `hulk_rt::HulkString`
            // (ObjHeader{ref_count:i64, gc_mark:u8, type_tag:u8, prev:ptr, next:ptr,
            // vtable:ptr} = 40 bytes, followed by len:i64, data:ptr — 56 bytes total).

            let i64_type = ctx.codegen.context.i64_type();
            let i8_type = ctx.codegen.context.i8_type();
            let ptr_type = ctx.codegen.context.ptr_type(Default::default());

            let header_ty = ctx.codegen.context.struct_type(
                &[
                    i64_type.into(),  // ref_count  – offset 0
                    i8_type.into(),   // gc_mark    – offset 8
                    i8_type.into(),   // type_tag   – offset 9  [+ 6 pad]
                    ptr_type.into(),  // prev       – offset 16
                    ptr_type.into(),  // next       – offset 24
                    ptr_type.into(),  // vtable     – offset 32
                ],
                false,
            );
            let header_const = ctx.codegen.context.const_struct(
                &[
                    i64_type.const_int(u64::MAX, false).into(), // ref_count
                    i8_type.const_int(0, false).into(),  // gc_mark
                    i8_type.const_int(TAG_LITERAL_STRING as u64, false).into(), // type_tag
                    ptr_type.const_null().into(),        // prev
                    ptr_type.const_null().into(),        // next
                    ptr_type.const_null().into(),        // vtable
                ],
                false,
            );

            let struct_type = ctx
                .codegen
                .context
                .struct_type(&[header_ty.into(), i64_type.into(), ptr_type.into()], false);
            let len_const = i64_type.const_int(s.len() as u64, false);
            let data_ptr: inkwell::values::BasicValueEnum = data_global.as_pointer_value().into();
            let struct_const = ctx
                .codegen
                .context
                .const_struct(&[header_const.into(), len_const.into(), data_ptr], false);

            let global = ctx
                .codegen
                .module
                .add_global(struct_type, None, &format!("str_{id}"));
            global.set_initializer(&struct_const);
            global.set_constant(true); // immutable string literal. Must be handled in retain and release as a no-op.
            global.set_linkage(Linkage::Private);
            global.set_unnamed_addr(true);

            Ok(global.as_pointer_value().into())
        }
    }
}
