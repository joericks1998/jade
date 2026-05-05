use inkwell::{
    types::BasicMetadataTypeEnum,
    values::{
        AnyValue, AnyValueEnum, BasicMetadataValueEnum, BasicValueEnum,
        CallSiteValue, IntValue, PointerValue,
    },
    AddressSpace, FloatPredicate, IntPredicate,
};

use crate::interpreter::ast::{BinOpKind, UnaryOpKind};
use crate::compiler::tir::{JadeType, TExpr, TExprKind, TFStrPart};

use super::{stmt, types, CodegenCtx};

/// Emit LLVM IR for `expr`, returning the resulting LLVM value.
pub fn emit_expr<'ctx>(
    expr: &TExpr,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    use TExprKind::*;

    match &expr.kind {
        // ── Scalar literals ───────────────────────────────────────────────────
        Integer(n) => Ok(ctx.context.i64_type().const_int(*n as u64, true).into()),

        Float(f) => Ok(ctx.context.f64_type().const_float(*f).into()),

        Bool(b) => Ok(ctx.context.bool_type().const_int(*b as u64, false).into()),

        Str(s) => {
            let global = ctx
                .builder
                .build_global_string_ptr(s, "str_lit")
                .map_err(|e| e.to_string())?;
            Ok(global.as_pointer_value().into())
        }

        // ── Variable reference ────────────────────────────────────────────────
        Identifier(name) => {
            if name == "nil" {
                return Ok(ctx.context.i64_type().const_int(0, false).into());
            }
            // Local variable in scope (covers closures, params, let-bindings).
            if let Some((ptr, ty)) = ctx.lookup(name) {
                let llvm_ty = types::jade_to_llvm(&ty, ctx.context);
                return ctx.builder
                    .build_load(llvm_ty, ptr, name)
                    .map_err(|e| e.to_string());
            }
            // Named function referenced as a first-class value (not a direct call).
            if ctx.fn_info.contains_key(name.as_str()) {
                return emit_fn_as_value(name, ctx);
            }
            Err(format!("undefined variable: {name}"))
        }

        // ── Binary operations ─────────────────────────────────────────────────
        BinOp { op, left, right } => {
            match op {
                BinOpKind::And => return emit_and(left, right, ctx),
                BinOpKind::Or  => return emit_or(left, right, ctx),
                _ => {}
            }
            let lhs = emit_expr(left, ctx)?;
            let rhs = emit_expr(right, ctx)?;
            // Use the actual LLVM value type to resolve Unknown/mismatched TIR types.
            let lty = actual_ty(&left.ty, &lhs);
            let rty = actual_ty(&right.ty, &rhs);
            emit_binop(op, &lty, &rty, lhs, rhs, ctx)
        }

        // ── Unary operations ──────────────────────────────────────────────────
        UnaryOp { op, operand } => {
            let val = emit_expr(operand, ctx)?;
            emit_unaryop(op, &operand.ty, val, ctx)
        }

        // ── Function / built-in calls ─────────────────────────────────────────
        Call { callee, args } => emit_call(callee, args, &expr.ty, ctx),

        // ── Heap collections ──────────────────────────────────────────────────
        Array { elements } => emit_array(elements, ctx),

        Index { object, index } => emit_index(object, index, &expr.ty, ctx),

        Dict { entries } => emit_dict(entries, ctx),

        StructLiteral { type_name, fields } => emit_struct_literal(type_name, fields, ctx),

        FieldAccess { object, field } => emit_field_access(object, field, &expr.ty, ctx),

        FStr { parts } => emit_fstr(parts, ctx),

        // ── First-class functions and closures ────────────────────────────────
        Closure { params, body, captures } => emit_closure(params, body, captures, ctx),

        // ── Async: await expr ────────────────────────────────────────────────
        Await { expr } => {
            let fut_val = emit_expr(expr, ctx)?;
            let fut_ptr = as_pointer(fut_val, ctx)?;
            ctx.uses_async = true;
            ctx.call_rv(ctx.jade_await_fn, &[fut_ptr.into()], "await_res")
        }

        // ── Unsupported in this backend ───────────────────────────────────────
        PromptDeref { .. } => Err("prompt dereference is not supported in the LLVM backend".into()),
    }
}

// ── Array creation ────────────────────────────────────────────────────────────

fn emit_array<'ctx>(
    elements: &[TExpr],
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let i64_ty = ctx.context.i64_type();
    let n = elements.len() as u64;

    // Allocate the jade.array header (24 bytes: ptr + i64 + i64)
    let header_ptr = ctx.malloc_ptr(i64_ty.const_int(24, false), "arr_hdr")?;

    // Allocate data slots (at least 8 bytes even for empty arrays)
    let data_bytes = if n > 0 { n * 8 } else { 8 };
    let data_ptr = ctx.malloc_ptr(i64_ty.const_int(data_bytes, false), "arr_data")?;

    // Store data ptr in field 0
    let f0 = ctx.builder
        .build_struct_gep(ctx.array_ty, header_ptr, 0, "arr_f0")
        .map_err(|e| e.to_string())?;
    ctx.builder.build_store(f0, data_ptr).map_err(|e| e.to_string())?;

    // Store len in field 1
    let f1 = ctx.builder
        .build_struct_gep(ctx.array_ty, header_ptr, 1, "arr_f1")
        .map_err(|e| e.to_string())?;
    ctx.builder.build_store(f1, i64_ty.const_int(n, false)).map_err(|e| e.to_string())?;

    // Store cap in field 2 (= len for now)
    let f2 = ctx.builder
        .build_struct_gep(ctx.array_ty, header_ptr, 2, "arr_f2")
        .map_err(|e| e.to_string())?;
    ctx.builder.build_store(f2, i64_ty.const_int(n, false)).map_err(|e| e.to_string())?;

    // Store each element as i64 bits
    for (i, elem) in elements.iter().enumerate() {
        let val = emit_expr(elem, ctx)?;
        let as_i64 = value_to_i64(val, &elem.ty, ctx)?;
        let slot = ctx.gep(i64_ty, data_ptr, &[i64_ty.const_int(i as u64, false)], "arr_slot")?;
        ctx.builder.build_store(slot, as_i64).map_err(|e| e.to_string())?;
    }

    Ok(header_ptr.into())
}

// ── Array indexing ────────────────────────────────────────────────────────────

fn emit_index<'ctx>(
    object: &TExpr,
    index: &TExpr,
    result_ty: &JadeType,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    // Dict indexing — key is always a string (char*)
    if matches!(&object.ty, JadeType::Dict) {
        ctx.uses_dicts = true;
        let dict_ptr = as_pointer(emit_expr(object, ctx)?, ctx)?;
        let key_ptr  = as_pointer(emit_expr(index, ctx)?, ctx)?;
        let raw = ctx.call_rv(
            ctx.jade_dict_get_fn,
            &[dict_ptr.into(), key_ptr.into()],
            "dict_get",
        )?.into_int_value();
        return i64_to_value(raw, result_ty, ctx);
    }

    let i64_ty = ctx.context.i64_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());

    let arr = as_pointer(emit_expr(object, ctx)?, ctx)?;
    let idx = emit_expr(index, ctx)?.into_int_value();

    // Load data ptr from field 0
    let f0 = ctx.builder
        .build_struct_gep(ctx.array_ty, arr, 0, "idx_f0")
        .map_err(|e| e.to_string())?;
    let data_ptr = ctx.builder
        .build_load(ptr_ty, f0, "idx_data")
        .map_err(|e| e.to_string())?
        .into_pointer_value();

    let slot = ctx.gep(i64_ty, data_ptr, &[idx], "idx_slot")?;
    let raw = ctx.builder
        .build_load(i64_ty, slot, "idx_raw")
        .map_err(|e| e.to_string())?
        .into_int_value();

    i64_to_value(raw, result_ty, ctx)
}

// ── Struct literal ────────────────────────────────────────────────────────────

fn emit_struct_literal<'ctx>(
    type_name: &str,
    fields: &[(String, TExpr, bool)],
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let i64_ty = ctx.context.i64_type();
    let n = fields.len() as u64;

    // malloc n * 8 bytes for uniform i64 slots
    let struct_ptr = ctx.malloc_ptr(i64_ty.const_int(n * 8 + 8, false), "struct_ptr")?;

    let field_names = ctx.struct_field_order
        .get(type_name)
        .cloned()
        .ok_or_else(|| format!("unknown struct type: {type_name}"))?;

    for (field_name, field_expr, _) in fields {
        let idx = field_names
            .iter()
            .position(|n| n == field_name)
            .ok_or_else(|| format!("unknown field '{field_name}' on struct '{type_name}'"))?;

        let val = emit_expr(field_expr, ctx)?;
        let as_i64 = value_to_i64(val, &field_expr.ty, ctx)?;
        let slot = ctx.gep(i64_ty, struct_ptr, &[i64_ty.const_int(idx as u64, false)], "sf_slot")?;
        ctx.builder.build_store(slot, as_i64).map_err(|e| e.to_string())?;
    }

    Ok(struct_ptr.into())
}

// ── Field access ──────────────────────────────────────────────────────────────

fn emit_field_access<'ctx>(
    object: &TExpr,
    field: &str,
    result_ty: &JadeType,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let type_name = match &object.ty {
        JadeType::Struct(n) => n.clone(),
        _ => return Err(format!("field access on non-struct: {:?}", object.ty)),
    };

    let field_names = ctx.struct_field_order
        .get(&type_name)
        .cloned()
        .ok_or_else(|| format!("unknown struct: {type_name}"))?;

    let idx = field_names
        .iter()
        .position(|n| n == field)
        .ok_or_else(|| format!("unknown field '{field}' on '{type_name}'"))?;

    // Use the recorded field type (from StructLiteral pre-pass) if known;
    // fall back to result_ty (usually Unknown) otherwise.
    let field_ty = ctx.struct_field_types
        .get(&type_name)
        .and_then(|m| m.get(field))
        .cloned()
        .unwrap_or_else(|| result_ty.clone());

    let i64_ty = ctx.context.i64_type();
    let struct_ptr = as_pointer(emit_expr(object, ctx)?, ctx)?;
    let slot = ctx.gep(i64_ty, struct_ptr, &[i64_ty.const_int(idx as u64, false)], "fa_slot")?;
    let raw = ctx.builder
        .build_load(i64_ty, slot, "fa_raw")
        .map_err(|e| e.to_string())?
        .into_int_value();

    i64_to_value(raw, &field_ty, ctx)
}

// ── F-string ──────────────────────────────────────────────────────────────────

fn emit_fstr<'ctx>(
    parts: &[TFStrPart],
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let i8_ty  = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();

    // 4096-byte stack buffer
    let buf_ptr = ctx.builder
        .build_array_alloca(i8_ty, i64_ty.const_int(4096, false), "fstr_buf")
        .map_err(|e| e.to_string())?;

    // Current write offset
    let pos_slot = ctx.builder
        .build_alloca(i64_ty, "fstr_pos")
        .map_err(|e| e.to_string())?;
    ctx.builder
        .build_store(pos_slot, i64_ty.const_int(0, false))
        .map_err(|e| e.to_string())?;

    for part in parts {
        let pos = ctx.builder
            .build_load(i64_ty, pos_slot, "fstr_pos_v")
            .map_err(|e| e.to_string())?
            .into_int_value();
        let write_ptr = ctx.gep(i8_ty, buf_ptr, &[pos], "fstr_wptr")?;

        let written = match part {
            TFStrPart::Literal(s) => {
                let lit = ctx.builder
                    .build_global_string_ptr(s, "fstr_lit")
                    .map_err(|e| e.to_string())?
                    .as_pointer_value();
                let fmt = ctx.builder
                    .build_global_string_ptr("%s", "fstr_sfmt")
                    .map_err(|e| e.to_string())?
                    .as_pointer_value();
                let call = ctx.builder
                    .build_call(ctx.sprintf_fn, &[write_ptr.into(), fmt.into(), lit.into()], "sp_lit")
                    .map_err(|e| e.to_string())?;
                extract_i32_from_call(call, ctx)?
            }
            TFStrPart::Expr(e) => {
                let val = emit_expr(e, ctx)?;
                emit_sprintf_value(val, &e.ty, write_ptr, ctx)?
            }
        };

        let written_i64 = ctx.builder
            .build_int_z_extend(written, i64_ty, "fstr_ext")
            .map_err(|e| e.to_string())?;
        let new_pos = ctx.builder
            .build_int_add(pos, written_i64, "fstr_newpos")
            .map_err(|e| e.to_string())?;
        ctx.builder.build_store(pos_slot, new_pos).map_err(|e| e.to_string())?;
    }

    Ok(buf_ptr.into())
}

fn emit_sprintf_value<'ctx>(
    val: BasicValueEnum<'ctx>,
    ty: &JadeType,
    write_ptr: PointerValue<'ctx>,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    let mk = |s: &str, ctx: &mut CodegenCtx<'ctx>| -> Result<PointerValue<'ctx>, String> {
        ctx.builder
            .build_global_string_ptr(s, "spfmt")
            .map_err(|e| e.to_string())
            .map(|g| g.as_pointer_value())
    };

    let call = match effective_ty(ty) {
        JadeType::Int => {
            let fmt = mk("%lld", ctx)?;
            ctx.builder.build_call(ctx.sprintf_fn, &[write_ptr.into(), fmt.into(), val.into()], "sp_int")
                .map_err(|e| e.to_string())?
        }
        JadeType::Float => {
            let fmt = mk("%g", ctx)?;
            ctx.builder.build_call(ctx.sprintf_fn, &[write_ptr.into(), fmt.into(), val.into()], "sp_flt")
                .map_err(|e| e.to_string())?
        }
        JadeType::Bool => {
            let t = mk("true", ctx)?;
            let f = mk("false", ctx)?;
            let sel = ctx.builder
                .build_select(val.into_int_value(), t, f, "sp_bsel")
                .map_err(|e| e.to_string())?;
            let fmt = mk("%s", ctx)?;
            ctx.builder.build_call(ctx.sprintf_fn, &[write_ptr.into(), fmt.into(), sel.into()], "sp_bool")
                .map_err(|e| e.to_string())?
        }
        JadeType::Str => {
            let fmt = mk("%s", ctx)?;
            ctx.builder.build_call(ctx.sprintf_fn, &[write_ptr.into(), fmt.into(), val.into()], "sp_str")
                .map_err(|e| e.to_string())?
        }
        _ => {
            let fmt = mk("%lld", ctx)?;
            ctx.builder.build_call(ctx.sprintf_fn, &[write_ptr.into(), fmt.into(), val.into()], "sp_unk")
                .map_err(|e| e.to_string())?
        }
    };
    extract_i32_from_call(call, ctx)
}

fn extract_i32_from_call<'ctx>(
    call: CallSiteValue<'ctx>,
    ctx: &CodegenCtx<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    match call.as_any_value_enum() {
        AnyValueEnum::IntValue(v) => Ok(v),
        _ => Ok(ctx.context.i32_type().const_int(0, false)),
    }
}

// ── String concatenation ──────────────────────────────────────────────────────

fn emit_str_concat<'ctx>(
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let i64_ty = ctx.context.i64_type();
    let lp = lhs.into_pointer_value();
    let rp = rhs.into_pointer_value();

    let ll = ctx.call_rv(ctx.strlen_fn, &[lp.into()], "llen")?.into_int_value();
    let rl = ctx.call_rv(ctx.strlen_fn, &[rp.into()], "rlen")?.into_int_value();
    let total = ctx.builder.build_int_add(ll, rl, "concat_len").map_err(|e| e.to_string())?;
    let total1 = ctx.builder
        .build_int_add(total, i64_ty.const_int(1, false), "concat_len1")
        .map_err(|e| e.to_string())?;

    let buf = ctx.malloc_ptr(total1, "concat_buf")?;
    let fmt = ctx.builder
        .build_global_string_ptr("%s%s", "concat_fmt")
        .map_err(|e| e.to_string())?
        .as_pointer_value();
    ctx.builder
        .build_call(ctx.sprintf_fn, &[buf.into(), fmt.into(), lp.into(), rp.into()], "")
        .map_err(|e| e.to_string())?;

    Ok(buf.into())
}

// ── len() built-in ────────────────────────────────────────────────────────────

fn emit_len<'ctx>(
    args: &[TExpr],
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    if args.len() != 1 {
        return Err(format!("len() takes 1 argument, got {}", args.len()));
    }
    let arg = &args[0];
    let val = emit_expr(arg, ctx)?;

    match &arg.ty {
        JadeType::Str => {
            ctx.call_rv(ctx.strlen_fn, &[val.into_pointer_value().into()], "str_len")
        }
        JadeType::Array(_) => {
            let i64_ty = ctx.context.i64_type();
            let arr_ptr = val.into_pointer_value();
            let f1 = ctx.builder
                .build_struct_gep(ctx.array_ty, arr_ptr, 1, "len_f1")
                .map_err(|e| e.to_string())?;
            ctx.builder
                .build_load(i64_ty, f1, "arr_len")
                .map_err(|e| e.to_string())
        }
        JadeType::Dict => {
            ctx.uses_dicts = true;
            ctx.call_rv(ctx.jade_dict_len_fn, &[val.into_pointer_value().into()], "dict_len")
        }
        _ => Err(format!("len() not supported for type {:?}", arg.ty)),
    }
}

// ── join() built-in ───────────────────────────────────────────────────────────

fn emit_join<'ctx>(
    args: &[crate::compiler::tir::TExpr],
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let n = args.len();
    let i64_ty = ctx.context.i64_type();
    let ptr_ty = ctx.context.ptr_type(inkwell::AddressSpace::default());
    let i32_ty = ctx.context.i32_type();

    // Evaluate each future argument to a pointer.
    let mut future_ptrs: Vec<PointerValue<'ctx>> = Vec::with_capacity(n);
    for arg in args {
        let val = emit_expr(arg, ctx)?;
        future_ptrs.push(as_pointer(val, ctx)?);
    }

    let alloc_n = i64_ty.const_int(n.max(1) as u64, false);

    // Stack-alloc array of future pointers: [n x ptr]
    let fut_arr = ctx.builder
        .build_array_alloca(ptr_ty, alloc_n, "join_futs")
        .map_err(|e| e.to_string())?;

    for (i, fut_ptr) in future_ptrs.iter().enumerate() {
        let slot = ctx.gep(
            ptr_ty,
            fut_arr,
            &[i64_ty.const_int(i as u64, false)],
            &format!("jf{i}_slot"),
        )?;
        ctx.builder.build_store(slot, *fut_ptr).map_err(|e| e.to_string())?;
    }

    // Stack-alloc results array: [n x i64]
    let res_arr = ctx.builder
        .build_array_alloca(i64_ty, alloc_n, "join_res")
        .map_err(|e| e.to_string())?;

    // jade_join(fut_arr, n, res_arr)
    let n_i32 = i32_ty.const_int(n as u64, false);
    ctx.call_void(ctx.jade_join_fn, &[fut_arr.into(), n_i32.into(), res_arr.into()])?;
    ctx.uses_async = true;

    // Build a jade array from the results.
    let header_ptr = ctx.malloc_ptr(i64_ty.const_int(24, false), "join_hdr")?;
    let data_bytes = (n.max(1) * 8) as u64;
    let data_ptr   = ctx.malloc_ptr(i64_ty.const_int(data_bytes, false), "join_data")?;

    let f0 = ctx.builder
        .build_struct_gep(ctx.array_ty, header_ptr, 0, "join_f0")
        .map_err(|e| e.to_string())?;
    ctx.builder.build_store(f0, data_ptr).map_err(|e| e.to_string())?;

    let f1 = ctx.builder
        .build_struct_gep(ctx.array_ty, header_ptr, 1, "join_f1")
        .map_err(|e| e.to_string())?;
    ctx.builder
        .build_store(f1, i64_ty.const_int(n as u64, false))
        .map_err(|e| e.to_string())?;

    let f2 = ctx.builder
        .build_struct_gep(ctx.array_ty, header_ptr, 2, "join_f2")
        .map_err(|e| e.to_string())?;
    ctx.builder
        .build_store(f2, i64_ty.const_int(n as u64, false))
        .map_err(|e| e.to_string())?;

    // Copy each result from res_arr into the jade array data.
    for i in 0..n {
        let src = ctx.gep(
            i64_ty,
            res_arr,
            &[i64_ty.const_int(i as u64, false)],
            &format!("jr{i}_src"),
        )?;
        let raw = ctx.builder
            .build_load(i64_ty, src, &format!("jr{i}_raw"))
            .map_err(|e| e.to_string())?
            .into_int_value();
        let dst = ctx.gep(
            i64_ty,
            data_ptr,
            &[i64_ty.const_int(i as u64, false)],
            &format!("jr{i}_dst"),
        )?;
        ctx.builder.build_store(dst, raw).map_err(|e| e.to_string())?;
    }

    Ok(header_ptr.into())
}

// ── Dict creation ─────────────────────────────────────────────────────────────

fn emit_dict<'ctx>(
    entries: &[(TExpr, TExpr)],
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    ctx.uses_dicts = true;
    let dict_ptr = ctx
        .call_rv(ctx.jade_dict_create_fn, &[], "dict_ptr")?
        .into_pointer_value();

    for (key_expr, val_expr) in entries {
        let key = emit_expr(key_expr, ctx)?;
        let val = emit_expr(val_expr, ctx)?;
        let key_ptr = as_pointer(key, ctx)?;
        let val_i64 = value_to_i64(val, &val_expr.ty, ctx)?;
        ctx.call_void(
            ctx.jade_dict_set_fn,
            &[dict_ptr.into(), key_ptr.into(), val_i64.into()],
        )?;
    }
    Ok(dict_ptr.into())
}

// ── Named function as first-class value ───────────────────────────────────────

/// Wrap the named function `name` in a `jade_fn_t` fat pointer so it can be
/// stored in a variable or passed as an argument.
///
/// Generates a thin wrapper `name__callable(i64..., ptr env) -> i64` that
/// unpacks i64 arguments, calls the real function, and packs the result as i64.
/// The wrapper is deduplicated — emitted at most once per named function.
fn emit_fn_as_value<'ctx>(
    name: &str,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let i64_ty = ctx.context.i64_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());

    let (fn_val, param_tys, fn_ret_ty) = ctx
        .fn_info
        .get(name)
        .ok_or_else(|| format!("no function named '{name}'"))?
        .clone();

    // Emit the wrapper function if it doesn't exist yet.
    let wrapper_name = format!("{name}__callable");
    let wrapper_fn = if let Some(existing) = ctx.module.get_function(&wrapper_name) {
        existing
    } else {
        // Signature: i64(i64..., ptr env)
        let mut wrapper_param_tys: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> =
            param_tys.iter().map(|_| i64_ty.into()).collect();
        wrapper_param_tys.push(ptr_ty.into());
        let wrapper_fn_ty = i64_ty.fn_type(&wrapper_param_tys, false);
        let wrapper_fn = ctx.module.add_function(&wrapper_name, wrapper_fn_ty, None);

        let restore_bb = ctx.builder.get_insert_block();
        let entry = ctx.context.append_basic_block(wrapper_fn, "entry");
        ctx.builder.position_at_end(entry);

        // Unpack i64 args → real types, call the real function.
        let mut real_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        for (i, param_ty) in param_tys.iter().enumerate() {
            let raw = wrapper_fn.get_nth_param(i as u32).unwrap().into_int_value();
            let val = i64_to_value(raw, param_ty, ctx)?;
            real_args.push(val.into());
        }
        let call_site = ctx.builder
            .build_call(fn_val, &real_args, "wcall")
            .map_err(|e| e.to_string())?;

        let result_i64 = match &fn_ret_ty {
            JadeType::Nil => i64_ty.const_int(0, false),
            _ => {
                let result_val: BasicValueEnum = match call_site.as_any_value_enum() {
                    AnyValueEnum::IntValue(v)     => v.into(),
                    AnyValueEnum::FloatValue(v)   => v.into(),
                    AnyValueEnum::PointerValue(v) => v.into(),
                    _ => i64_ty.const_int(0, false).into(),
                };
                value_to_i64(result_val, &fn_ret_ty, ctx)?
            }
        };
        ctx.builder.build_return(Some(&result_i64)).map_err(|e| e.to_string())?;

        if let Some(bb) = restore_bb {
            ctx.builder.position_at_end(bb);
        }
        wrapper_fn
    };

    // Allocate a jade_fn_t: { &wrapper_fn, null }
    let jade_fn_ptr = ctx.malloc_ptr(i64_ty.const_int(16, false), "named_fn_val")?;
    let f0 = ctx.builder
        .build_struct_gep(ctx.jade_fn_ty, jade_fn_ptr, 0, "nfv_f0")
        .map_err(|e| e.to_string())?;
    ctx.builder
        .build_store(f0, wrapper_fn.as_global_value().as_pointer_value())
        .map_err(|e| e.to_string())?;
    let f1 = ctx.builder
        .build_struct_gep(ctx.jade_fn_ty, jade_fn_ptr, 1, "nfv_f1")
        .map_err(|e| e.to_string())?;
    ctx.builder
        .build_store(f1, ptr_ty.const_null())
        .map_err(|e| e.to_string())?;

    Ok(jade_fn_ptr.into())
}

// ── Closure emission ──────────────────────────────────────────────────────────

/// Emit a closure expression, returning a `jade_fn_t*` fat pointer.
///
/// Three steps:
///   1. Allocate an env struct on the heap and store each captured variable.
///   2. Emit `closure_N(i64 a0, …, ptr env) -> i64` as a new LLVM function.
///   3. Allocate a `jade_fn_t` and fill it with `{ &closure_N, env_ptr }`.
fn emit_closure<'ctx>(
    params: &[String],
    body: &[crate::compiler::tir::TStmt],
    captures: &[(String, JadeType)],
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let i64_ty = ctx.context.i64_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());

    // ── 1. Allocate env struct and store captured values ───────────────────
    // (done NOW, in the outer function, while captured locals are still in scope)
    let env_ptr: PointerValue<'ctx> = if captures.is_empty() {
        ptr_ty.const_null()
    } else {
        let n = captures.len() as u64;
        let ep = ctx.malloc_ptr(i64_ty.const_int(n * 8, false), "cl_env")?;
        for (i, (cap_name, cap_ty)) in captures.iter().enumerate() {
            let (slot, slot_ty) = ctx
                .lookup(cap_name)
                .ok_or_else(|| format!("captured variable '{cap_name}' not found in scope"))?;
            let llvm_ty = types::jade_to_llvm(&slot_ty, ctx.context);
            let val = ctx.builder
                .build_load(llvm_ty, slot, cap_name)
                .map_err(|e| e.to_string())?;
            let as_i64 = value_to_i64(val, cap_ty, ctx)?;
            let env_slot = ctx.gep(
                i64_ty, ep,
                &[i64_ty.const_int(i as u64, false)],
                &format!("cap{i}_slot"),
            )?;
            ctx.builder.build_store(env_slot, as_i64).map_err(|e| e.to_string())?;
        }
        ep
    };

    // ── 2. Emit the closure body as a new LLVM function ────────────────────
    let closure_id = ctx.closure_counter;
    ctx.closure_counter += 1;
    let body_name = format!("closure_{closure_id}");

    // Signature: i64(i64 a0, …, ptr env)
    let mut cl_param_tys: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> =
        params.iter().map(|_| i64_ty.into()).collect();
    cl_param_tys.push(ptr_ty.into());
    let cl_fn_ty = i64_ty.fn_type(&cl_param_tys, false);
    let cl_fn = ctx.module.add_function(&body_name, cl_fn_ty, None);

    let restore_bb = ctx.builder.get_insert_block();
    let entry_bb = ctx.context.append_basic_block(cl_fn, "entry");
    ctx.builder.position_at_end(entry_bb);

    ctx.push_scope();
    let saved_ret   = ctx.current_ret_ty.take();
    let saved_async = ctx.async_body_ret_ty.take();
    // Signal TStmt::Return to pack return values as i64 (same as async bodies).
    ctx.async_body_ret_ty = Some(JadeType::Unknown);

    // Bind parameters from i64 LLVM args.
    for (i, param_name) in params.iter().enumerate() {
        let alloca = ctx.builder
            .build_alloca(i64_ty, param_name)
            .map_err(|e| e.to_string())?;
        let arg = cl_fn.get_nth_param(i as u32).unwrap();
        ctx.builder.build_store(alloca, arg).map_err(|e| e.to_string())?;
        ctx.define(param_name.clone(), alloca, JadeType::Unknown);
    }

    // Restore captured variables from the env struct.
    let env_param = cl_fn.get_nth_param(params.len() as u32).unwrap().into_pointer_value();
    for (i, (cap_name, cap_ty)) in captures.iter().enumerate() {
        let env_slot = ctx.gep(
            i64_ty, env_param,
            &[i64_ty.const_int(i as u64, false)],
            &format!("env{i}"),
        )?;
        let raw = ctx.builder
            .build_load(i64_ty, env_slot, &format!("env_{cap_name}_raw"))
            .map_err(|e| e.to_string())?
            .into_int_value();
        let val = i64_to_value(raw, cap_ty, ctx)?;
        let llvm_ty = types::jade_to_llvm(cap_ty, ctx.context);
        let alloca = ctx.builder
            .build_alloca(llvm_ty, cap_name)
            .map_err(|e| e.to_string())?;
        ctx.builder.build_store(alloca, val).map_err(|e| e.to_string())?;
        ctx.define(cap_name.clone(), alloca, cap_ty.clone());
    }

    stmt::emit_stmts(ctx, body)?;

    if !ctx.is_terminated() {
        ctx.builder
            .build_return(Some(&i64_ty.const_int(0, false)))
            .map_err(|e| e.to_string())?;
    }

    ctx.pop_scope();
    ctx.current_ret_ty   = saved_ret;
    ctx.async_body_ret_ty = saved_async;

    if let Some(bb) = restore_bb {
        ctx.builder.position_at_end(bb);
    }

    // ── 3. Allocate jade_fn_t { &closure_N, env_ptr } ─────────────────────
    let jade_fn_ptr = ctx.malloc_ptr(i64_ty.const_int(16, false), "jade_fn")?;
    let f0 = ctx.builder
        .build_struct_gep(ctx.jade_fn_ty, jade_fn_ptr, 0, "cl_f0")
        .map_err(|e| e.to_string())?;
    ctx.builder
        .build_store(f0, cl_fn.as_global_value().as_pointer_value())
        .map_err(|e| e.to_string())?;
    let f1 = ctx.builder
        .build_struct_gep(ctx.jade_fn_ty, jade_fn_ptr, 1, "cl_f1")
        .map_err(|e| e.to_string())?;
    ctx.builder.build_store(f1, env_ptr).map_err(|e| e.to_string())?;

    Ok(jade_fn_ptr.into())
}

// ── Short-circuit logical operators ──────────────────────────────────────────

fn emit_and<'ctx>(
    left: &TExpr,
    right: &TExpr,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let fn_val = ctx
        .builder
        .get_insert_block()
        .and_then(|bb| bb.get_parent())
        .ok_or("&& outside function")?;

    let lhs = emit_expr(left, ctx)?.into_int_value();
    let rhs_bb   = ctx.context.append_basic_block(fn_val, "and_rhs");
    let merge_bb = ctx.context.append_basic_block(fn_val, "and_merge");

    ctx.builder
        .build_conditional_branch(lhs, rhs_bb, merge_bb)
        .map_err(|e| e.to_string())?;
    let lhs_end = ctx.builder.get_insert_block()
        .ok_or_else(|| "&&: builder lost insert block after lhs branch".to_string())?;

    ctx.builder.position_at_end(rhs_bb);
    let rhs = emit_expr(right, ctx)?.into_int_value();
    ctx.builder.build_unconditional_branch(merge_bb).map_err(|e| e.to_string())?;
    let rhs_end = ctx.builder.get_insert_block()
        .ok_or_else(|| "&&: builder lost insert block after rhs branch".to_string())?;

    ctx.builder.position_at_end(merge_bb);
    let phi = ctx
        .builder
        .build_phi(ctx.context.bool_type(), "and_result")
        .map_err(|e| e.to_string())?;
    phi.add_incoming(&[
        (&ctx.context.bool_type().const_int(0, false), lhs_end),
        (&rhs, rhs_end),
    ]);
    Ok(phi.as_basic_value())
}

fn emit_or<'ctx>(
    left: &TExpr,
    right: &TExpr,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let fn_val = ctx
        .builder
        .get_insert_block()
        .and_then(|bb| bb.get_parent())
        .ok_or("|| outside function")?;

    let lhs = emit_expr(left, ctx)?.into_int_value();
    let rhs_bb   = ctx.context.append_basic_block(fn_val, "or_rhs");
    let merge_bb = ctx.context.append_basic_block(fn_val, "or_merge");

    ctx.builder
        .build_conditional_branch(lhs, merge_bb, rhs_bb)
        .map_err(|e| e.to_string())?;
    let lhs_end = ctx.builder.get_insert_block()
        .ok_or_else(|| "||: builder lost insert block after lhs branch".to_string())?;

    ctx.builder.position_at_end(rhs_bb);
    let rhs = emit_expr(right, ctx)?.into_int_value();
    ctx.builder.build_unconditional_branch(merge_bb).map_err(|e| e.to_string())?;
    let rhs_end = ctx.builder.get_insert_block()
        .ok_or_else(|| "||: builder lost insert block after rhs branch".to_string())?;

    ctx.builder.position_at_end(merge_bb);
    let phi = ctx
        .builder
        .build_phi(ctx.context.bool_type(), "or_result")
        .map_err(|e| e.to_string())?;
    phi.add_incoming(&[
        (&ctx.context.bool_type().const_int(1, false), lhs_end),
        (&rhs, rhs_end),
    ]);
    Ok(phi.as_basic_value())
}

// ── Binary operator dispatch ──────────────────────────────────────────────────

fn emit_binop<'ctx>(
    op: &BinOpKind,
    lty: &JadeType,
    rty: &JadeType,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    use BinOpKind::*;

    let elty = effective_ty(lty);
    let erty = effective_ty(rty);
    let b = &ctx.builder;

    match (&elty, op, &erty) {
        // ── Int × Int arithmetic ──────────────────────────────────────────────
        (JadeType::Int, Add, JadeType::Int) =>
            Ok(b.build_int_add(lhs.into_int_value(), rhs.into_int_value(), "iadd").map_err(|e| e.to_string())?.into()),
        (JadeType::Int, Sub, JadeType::Int) =>
            Ok(b.build_int_sub(lhs.into_int_value(), rhs.into_int_value(), "isub").map_err(|e| e.to_string())?.into()),
        (JadeType::Int, Mul, JadeType::Int) =>
            Ok(b.build_int_mul(lhs.into_int_value(), rhs.into_int_value(), "imul").map_err(|e| e.to_string())?.into()),
        (JadeType::Int, Div, JadeType::Int) =>
            Ok(b.build_int_signed_div(lhs.into_int_value(), rhs.into_int_value(), "idiv").map_err(|e| e.to_string())?.into()),
        (JadeType::Int, Mod, JadeType::Int) =>
            Ok(b.build_int_signed_rem(lhs.into_int_value(), rhs.into_int_value(), "irem").map_err(|e| e.to_string())?.into()),

        // ── Float × Float arithmetic ──────────────────────────────────────────
        (JadeType::Float, Add, JadeType::Float) =>
            Ok(b.build_float_add(lhs.into_float_value(), rhs.into_float_value(), "fadd").map_err(|e| e.to_string())?.into()),
        (JadeType::Float, Sub, JadeType::Float) =>
            Ok(b.build_float_sub(lhs.into_float_value(), rhs.into_float_value(), "fsub").map_err(|e| e.to_string())?.into()),
        (JadeType::Float, Mul, JadeType::Float) =>
            Ok(b.build_float_mul(lhs.into_float_value(), rhs.into_float_value(), "fmul").map_err(|e| e.to_string())?.into()),
        (JadeType::Float, Div, JadeType::Float) =>
            Ok(b.build_float_div(lhs.into_float_value(), rhs.into_float_value(), "fdiv").map_err(|e| e.to_string())?.into()),

        // ── Mixed int/float: promote int → float ──────────────────────────────
        (JadeType::Int, Add | Sub | Mul | Div, JadeType::Float) => {
            let lf = b.build_signed_int_to_float(lhs.into_int_value(), ctx.context.f64_type(), "itof").map_err(|e| e.to_string())?;
            emit_binop(op, &JadeType::Float, &JadeType::Float, lf.into(), rhs, ctx)
        }
        (JadeType::Float, Add | Sub | Mul | Div, JadeType::Int) => {
            let rf = b.build_signed_int_to_float(rhs.into_int_value(), ctx.context.f64_type(), "itof").map_err(|e| e.to_string())?;
            emit_binop(op, &JadeType::Float, &JadeType::Float, lhs, rf.into(), ctx)
        }

        // ── String concatenation ──────────────────────────────────────────────
        (JadeType::Str, Add, JadeType::Str) => emit_str_concat(lhs, rhs, ctx),

        // ── Bitwise (integers only) ───────────────────────────────────────────
        (JadeType::Int, BitAnd, JadeType::Int) =>
            Ok(b.build_and(lhs.into_int_value(), rhs.into_int_value(), "band").map_err(|e| e.to_string())?.into()),
        (JadeType::Int, BitOr, JadeType::Int) =>
            Ok(b.build_or(lhs.into_int_value(), rhs.into_int_value(), "bor").map_err(|e| e.to_string())?.into()),
        (JadeType::Int, BitXor, JadeType::Int) =>
            Ok(b.build_xor(lhs.into_int_value(), rhs.into_int_value(), "bxor").map_err(|e| e.to_string())?.into()),
        (JadeType::Int, Shl, JadeType::Int) =>
            Ok(b.build_left_shift(lhs.into_int_value(), rhs.into_int_value(), "shl").map_err(|e| e.to_string())?.into()),
        (JadeType::Int, Shr, JadeType::Int) =>
            Ok(b.build_right_shift(lhs.into_int_value(), rhs.into_int_value(), true, "shr").map_err(|e| e.to_string())?.into()),

        // ── Integer comparisons ───────────────────────────────────────────────
        (JadeType::Int, Eq, JadeType::Int) => icmp(IntPredicate::EQ,  lhs, rhs, ctx),
        (JadeType::Int, Ne, JadeType::Int) => icmp(IntPredicate::NE,  lhs, rhs, ctx),
        (JadeType::Int, Lt, JadeType::Int) => icmp(IntPredicate::SLT, lhs, rhs, ctx),
        (JadeType::Int, Gt, JadeType::Int) => icmp(IntPredicate::SGT, lhs, rhs, ctx),
        (JadeType::Int, Le, JadeType::Int) => icmp(IntPredicate::SLE, lhs, rhs, ctx),
        (JadeType::Int, Ge, JadeType::Int) => icmp(IntPredicate::SGE, lhs, rhs, ctx),

        // ── Float comparisons ─────────────────────────────────────────────────
        (JadeType::Float, Eq, JadeType::Float) => fcmp(FloatPredicate::OEQ, lhs, rhs, ctx),
        (JadeType::Float, Ne, JadeType::Float) => fcmp(FloatPredicate::ONE, lhs, rhs, ctx),
        (JadeType::Float, Lt, JadeType::Float) => fcmp(FloatPredicate::OLT, lhs, rhs, ctx),
        (JadeType::Float, Gt, JadeType::Float) => fcmp(FloatPredicate::OGT, lhs, rhs, ctx),
        (JadeType::Float, Le, JadeType::Float) => fcmp(FloatPredicate::OLE, lhs, rhs, ctx),
        (JadeType::Float, Ge, JadeType::Float) => fcmp(FloatPredicate::OGE, lhs, rhs, ctx),

        // ── Mixed int/float comparisons ───────────────────────────────────────
        (JadeType::Int, Eq | Ne | Lt | Gt | Le | Ge, JadeType::Float) => {
            let lf = b.build_signed_int_to_float(lhs.into_int_value(), ctx.context.f64_type(), "itof").map_err(|e| e.to_string())?;
            emit_binop(op, &JadeType::Float, &JadeType::Float, lf.into(), rhs, ctx)
        }
        (JadeType::Float, Eq | Ne | Lt | Gt | Le | Ge, JadeType::Int) => {
            let rf = b.build_signed_int_to_float(rhs.into_int_value(), ctx.context.f64_type(), "itof").map_err(|e| e.to_string())?;
            emit_binop(op, &JadeType::Float, &JadeType::Float, lhs, rf.into(), ctx)
        }

        // ── Bool comparisons ──────────────────────────────────────────────────
        (JadeType::Bool, Eq, JadeType::Bool) => icmp(IntPredicate::EQ,  lhs, rhs, ctx),
        (JadeType::Bool, Ne, JadeType::Bool) => icmp(IntPredicate::NE,  lhs, rhs, ctx),
        (JadeType::Bool, Lt, JadeType::Bool) => icmp(IntPredicate::ULT, lhs, rhs, ctx),
        (JadeType::Bool, Gt, JadeType::Bool) => icmp(IntPredicate::UGT, lhs, rhs, ctx),
        (JadeType::Bool, Le, JadeType::Bool) => icmp(IntPredicate::ULE, lhs, rhs, ctx),
        (JadeType::Bool, Ge, JadeType::Bool) => icmp(IntPredicate::UGE, lhs, rhs, ctx),

        _ => Err(format!(
            "unsupported binary op {:?} for types {:?} × {:?} in LLVM backend",
            op, lty, rty
        )),
    }
}

fn icmp<'ctx>(
    pred: IntPredicate,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    Ok(ctx
        .builder
        .build_int_compare(pred, lhs.into_int_value(), rhs.into_int_value(), "icmp")
        .map_err(|e| e.to_string())?
        .into())
}

fn fcmp<'ctx>(
    pred: FloatPredicate,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    Ok(ctx
        .builder
        .build_float_compare(pred, lhs.into_float_value(), rhs.into_float_value(), "fcmp")
        .map_err(|e| e.to_string())?
        .into())
}

// ── Unary operator dispatch ───────────────────────────────────────────────────

fn emit_unaryop<'ctx>(
    op: &UnaryOpKind,
    ty: &JadeType,
    val: BasicValueEnum<'ctx>,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    match op {
        UnaryOpKind::Neg => match effective_ty(ty) {
            JadeType::Int =>
                Ok(ctx.builder.build_int_neg(val.into_int_value(), "ineg").map_err(|e| e.to_string())?.into()),
            JadeType::Float =>
                Ok(ctx.builder.build_float_neg(val.into_float_value(), "fneg").map_err(|e| e.to_string())?.into()),
            _ => Err(format!("cannot negate {:?} in LLVM backend", ty)),
        },
        UnaryOpKind::Not =>
            Ok(ctx.builder.build_not(val.into_int_value(), "not").map_err(|e| e.to_string())?.into()),
        UnaryOpKind::BitNot =>
            Ok(ctx.builder.build_not(val.into_int_value(), "bitnot").map_err(|e| e.to_string())?.into()),
    }
}

// ── Function calls ────────────────────────────────────────────────────────────

fn emit_call<'ctx>(
    callee: &TExpr,
    args: &[TExpr],
    ret_ty: &JadeType,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    // ── Built-in functions ────────────────────────────────────────────────────
    if let TExprKind::Identifier(name) = &callee.kind {
        match name.as_str() {
            "print" => return emit_print(args, ctx),
            "len"   => return emit_len(args, ctx),
            "join"  => return emit_join(args, ctx),
            _ => {}
        }
    }

    // ── Direct named function call ─────────────────────────────────────────
    if let TExprKind::Identifier(fn_name) = &callee.kind {
        if let Some((fn_val, param_tys, fn_ret_ty)) = ctx.fn_info.get(fn_name.as_str()).cloned() {
            let mut llvm_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(args.len());
            for (i, arg_expr) in args.iter().enumerate() {
                let arg_val = emit_expr(arg_expr, ctx)?;
                let param_ty = param_tys.get(i).unwrap_or(&JadeType::Unknown);
                let coerced = coerce(arg_val, &arg_expr.ty, param_ty, ctx)?;
                llvm_args.push(coerced.into());
            }
            let call_site = ctx.builder
                .build_call(fn_val, &llvm_args, "call")
                .map_err(|e| e.to_string())?;
            return match fn_ret_ty {
                JadeType::Nil => Ok(ctx.context.i64_type().const_int(0, false).into()),
                _ => match call_site.as_any_value_enum() {
                    AnyValueEnum::IntValue(v)     => Ok(v.into()),
                    AnyValueEnum::FloatValue(v)   => Ok(v.into()),
                    AnyValueEnum::PointerValue(v) => Ok(v.into()),
                    AnyValueEnum::StructValue(v)  => Ok(v.into()),
                    AnyValueEnum::ArrayValue(v)   => Ok(v.into()),
                    AnyValueEnum::VectorValue(v)  => Ok(v.into()),
                    _ => Ok(ctx.context.i64_type().const_int(0, false).into()),
                },
            };
        }
    }

    // ── Indirect call through jade_fn_t fat pointer ───────────────────────────
    emit_indirect_call(callee, args, ret_ty, ctx)
}

/// Call a first-class function value (closure or named-fn-as-value) stored as a
/// `jade_fn_t*`.  All arguments are packed as `i64`; the env pointer is the last
/// argument.  The result is returned as an `i64` and then reinterpreted as `ret_ty`.
fn emit_indirect_call<'ctx>(
    callee: &TExpr,
    args: &[TExpr],
    ret_ty: &JadeType,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let i64_ty = ctx.context.i64_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());

    let jade_fn_ptr = as_pointer(emit_expr(callee, ctx)?, ctx)?;

    // Load fn_ptr (field 0) and env_ptr (field 1) from the fat pointer.
    let fp_slot = ctx.builder
        .build_struct_gep(ctx.jade_fn_ty, jade_fn_ptr, 0, "fp_slot")
        .map_err(|e| e.to_string())?;
    let fn_ptr = ctx.builder
        .build_load(ptr_ty, fp_slot, "fn_ptr")
        .map_err(|e| e.to_string())?
        .into_pointer_value();

    let ep_slot = ctx.builder
        .build_struct_gep(ctx.jade_fn_ty, jade_fn_ptr, 1, "ep_slot")
        .map_err(|e| e.to_string())?;
    let env_ptr = ctx.builder
        .build_load(ptr_ty, ep_slot, "env_ptr")
        .map_err(|e| e.to_string())?
        .into_pointer_value();

    // Coerce all arguments to i64, append env_ptr.
    let mut llvm_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(args.len() + 1);
    for arg_expr in args {
        let val = emit_expr(arg_expr, ctx)?;
        llvm_args.push(value_to_i64(val, &arg_expr.ty, ctx)?.into());
    }
    llvm_args.push(env_ptr.into());

    // Build the uniform function type: i64(i64..., ptr).
    let mut param_tys: Vec<BasicMetadataTypeEnum<'ctx>> =
        args.iter().map(|_| BasicMetadataTypeEnum::IntType(i64_ty)).collect();
    param_tys.push(BasicMetadataTypeEnum::PointerType(ptr_ty));
    let indirect_fn_ty = i64_ty.fn_type(&param_tys, false);

    let call_site = ctx.builder
        .build_indirect_call(indirect_fn_ty, fn_ptr, &llvm_args, "icall")
        .map_err(|e| e.to_string())?;

    let raw = match call_site.as_any_value_enum() {
        AnyValueEnum::IntValue(v) => v,
        _ => i64_ty.const_int(0, false),
    };
    i64_to_value(raw, ret_ty, ctx)
}

// ── print() built-in ─────────────────────────────────────────────────────────

fn emit_print<'ctx>(
    args: &[TExpr],
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let n = args.len();
    for (i, arg) in args.iter().enumerate() {
        let val = emit_expr(arg, ctx)?;
        let suffix = if i == n - 1 { "\n" } else { " " };
        emit_printf_value(val, &arg.ty, suffix, ctx)?;
    }
    Ok(ctx.context.i64_type().const_int(0, false).into())
}

fn emit_printf_value<'ctx>(
    val: BasicValueEnum<'ctx>,
    ty: &JadeType,
    suffix: &str,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<(), String> {
    let sp = |s: &str, ctx: &mut CodegenCtx<'ctx>| -> Result<PointerValue<'ctx>, String> {
        ctx.builder
            .build_global_string_ptr(s, "pfmt")
            .map_err(|e| e.to_string())
            .map(|g| g.as_pointer_value())
    };

    match actual_ty(ty, &val) {
        JadeType::Int => {
            let fmt = sp(&format!("%lld{suffix}"), ctx)?;
            ctx.builder.build_call(ctx.printf_fn, &[fmt.into(), val.into()], "").map_err(|e| e.to_string())?;
        }
        JadeType::Float => {
            let fmt = sp(&format!("%g{suffix}"), ctx)?;
            ctx.builder.build_call(ctx.printf_fn, &[fmt.into(), val.into()], "").map_err(|e| e.to_string())?;
        }
        JadeType::Bool => {
            let t = sp(&format!("true{suffix}"), ctx)?;
            let f = sp(&format!("false{suffix}"), ctx)?;
            let sel = ctx.builder
                .build_select(val.into_int_value(), t, f, "bstr")
                .map_err(|e| e.to_string())?;
            let fmt = sp("%s", ctx)?;
            ctx.builder.build_call(ctx.printf_fn, &[fmt.into(), sel.into()], "").map_err(|e| e.to_string())?;
        }
        JadeType::Str => {
            let fmt = sp(&format!("%s{suffix}"), ctx)?;
            ctx.builder.build_call(ctx.printf_fn, &[fmt.into(), val.into()], "").map_err(|e| e.to_string())?;
        }
        JadeType::Nil => {
            let fmt = sp(&format!("nil{suffix}"), ctx)?;
            ctx.builder.build_call(ctx.printf_fn, &[fmt.into()], "").map_err(|e| e.to_string())?;
        }
        JadeType::Array(ref inner) => {
            emit_print_array(val.into_pointer_value(), inner, suffix, ctx)?;
        }
        JadeType::Struct(_) => {
            let pi = ctx.builder
                .build_ptr_to_int(val.into_pointer_value(), ctx.context.i64_type(), "spi")
                .map_err(|e| e.to_string())?;
            let fmt = sp(&format!("<struct:0x%llx>{suffix}"), ctx)?;
            ctx.builder.build_call(ctx.printf_fn, &[fmt.into(), pi.into()], "").map_err(|e| e.to_string())?;
        }
        JadeType::Dict => {
            let pi = ctx.builder
                .build_ptr_to_int(val.into_pointer_value(), ctx.context.i64_type(), "dpi")
                .map_err(|e| e.to_string())?;
            let fmt = sp(&format!("<dict:0x%llx>{suffix}"), ctx)?;
            ctx.builder.build_call(ctx.printf_fn, &[fmt.into(), pi.into()], "").map_err(|e| e.to_string())?;
        }
        JadeType::Fn { .. } | JadeType::AsyncFn { .. } => {
            let pi = ctx.builder
                .build_ptr_to_int(val.into_pointer_value(), ctx.context.i64_type(), "fpi")
                .map_err(|e| e.to_string())?;
            let fmt = sp(&format!("<fn:0x%llx>{suffix}"), ctx)?;
            ctx.builder.build_call(ctx.printf_fn, &[fmt.into(), pi.into()], "").map_err(|e| e.to_string())?;
        }
        _ => {
            let fmt = sp(&format!("%lld{suffix}"), ctx)?;
            ctx.builder.build_call(ctx.printf_fn, &[fmt.into(), val.into()], "").map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn emit_print_array<'ctx>(
    arr_ptr: PointerValue<'ctx>,
    elem_ty: &JadeType,
    suffix: &str,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<(), String> {
    let i64_ty = ctx.context.i64_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());

    // Print "["
    let open = ctx.builder
        .build_global_string_ptr("[", "arr_open")
        .map_err(|e| e.to_string())?
        .as_pointer_value();
    ctx.builder.build_call(ctx.printf_fn, &[open.into()], "").map_err(|e| e.to_string())?;

    // Load len
    let f1 = ctx.builder
        .build_struct_gep(ctx.array_ty, arr_ptr, 1, "pa_f1")
        .map_err(|e| e.to_string())?;
    let arr_len = ctx.builder
        .build_load(i64_ty, f1, "pa_len")
        .map_err(|e| e.to_string())?
        .into_int_value();

    // Load data ptr
    let f0 = ctx.builder
        .build_struct_gep(ctx.array_ty, arr_ptr, 0, "pa_f0")
        .map_err(|e| e.to_string())?;
    let data_ptr = ctx.builder
        .build_load(ptr_ty, f0, "pa_data")
        .map_err(|e| e.to_string())?
        .into_pointer_value();

    let fn_val = ctx.builder
        .get_insert_block()
        .and_then(|bb| bb.get_parent())
        .ok_or("print array outside function")?;

    let loop_i = ctx.build_entry_alloca(i64_ty.into(), "pa_i")?;
    ctx.builder
        .build_store(loop_i, i64_ty.const_int(0, false))
        .map_err(|e| e.to_string())?;

    let cond_bb = ctx.context.append_basic_block(fn_val, "pa_cond");
    let body_bb = ctx.context.append_basic_block(fn_val, "pa_body");
    let sep_bb  = ctx.context.append_basic_block(fn_val, "pa_sep");
    let exit_bb = ctx.context.append_basic_block(fn_val, "pa_exit");

    ctx.builder.build_unconditional_branch(cond_bb).map_err(|e| e.to_string())?;

    // Condition
    ctx.builder.position_at_end(cond_bb);
    let i_val = ctx.builder.build_load(i64_ty, loop_i, "pa_i_v").map_err(|e| e.to_string())?.into_int_value();
    let cond = ctx.builder
        .build_int_compare(IntPredicate::SLT, i_val, arr_len, "pa_lt")
        .map_err(|e| e.to_string())?;
    ctx.builder.build_conditional_branch(cond, body_bb, exit_bb).map_err(|e| e.to_string())?;

    // Body: load element, print, increment, check for separator
    ctx.builder.position_at_end(body_bb);
    let i_val = ctx.builder.build_load(i64_ty, loop_i, "pa_i_v2").map_err(|e| e.to_string())?.into_int_value();
    let slot = ctx.gep(i64_ty, data_ptr, &[i_val], "pa_slot")?;
    let raw = ctx.builder
        .build_load(i64_ty, slot, "pa_raw")
        .map_err(|e| e.to_string())?
        .into_int_value();
    let elem = i64_to_value(raw, elem_ty, ctx)?;
    emit_printf_value(elem, elem_ty, "", ctx)?;

    let next = ctx.builder
        .build_int_add(i_val, i64_ty.const_int(1, false), "pa_next")
        .map_err(|e| e.to_string())?;
    ctx.builder.build_store(loop_i, next).map_err(|e| e.to_string())?;

    let more = ctx.builder
        .build_int_compare(IntPredicate::SLT, next, arr_len, "pa_more")
        .map_err(|e| e.to_string())?;
    ctx.builder.build_conditional_branch(more, sep_bb, cond_bb).map_err(|e| e.to_string())?;

    // Separator
    ctx.builder.position_at_end(sep_bb);
    let sep = ctx.builder
        .build_global_string_ptr(", ", "arr_sep")
        .map_err(|e| e.to_string())?
        .as_pointer_value();
    ctx.builder.build_call(ctx.printf_fn, &[sep.into()], "").map_err(|e| e.to_string())?;
    ctx.builder.build_unconditional_branch(cond_bb).map_err(|e| e.to_string())?;

    // Exit: print "]" + suffix
    ctx.builder.position_at_end(exit_bb);
    let close = ctx.builder
        .build_global_string_ptr(&format!("]{suffix}"), "arr_close")
        .map_err(|e| e.to_string())?
        .as_pointer_value();
    ctx.builder.build_call(ctx.printf_fn, &[close.into()], "").map_err(|e| e.to_string())?;

    Ok(())
}

// ── Uniform i64 slot conversion ───────────────────────────────────────────────

/// Convert any Jade value to an i64 for storage in a uniform heap slot.
pub fn value_to_i64<'ctx>(
    val: BasicValueEnum<'ctx>,
    ty: &JadeType,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    let i64_ty = ctx.context.i64_type();
    match effective_ty(ty) {
        JadeType::Float => ctx.float_to_i64_bits(val.into_float_value()),
        JadeType::Bool => ctx.builder
            .build_int_z_extend(val.into_int_value(), i64_ty, "b2i64")
            .map_err(|e| e.to_string()),
        JadeType::Str | JadeType::Array(_) | JadeType::Struct(_)
        | JadeType::Dict | JadeType::Fn { .. } | JadeType::AsyncFn { .. }
        | JadeType::Future(_) | JadeType::Prompt => ctx.builder
            .build_ptr_to_int(val.into_pointer_value(), i64_ty, "p2i64")
            .map_err(|e| e.to_string()),
        // For Int / Unknown / Nil: inspect the actual LLVM value type so that
        // returning a pointer or float from an Unknown-typed function still works.
        _ => match val {
            BasicValueEnum::PointerValue(p) =>
                ctx.builder.build_ptr_to_int(p, i64_ty, "p2i64_unk").map_err(|e| e.to_string()),
            BasicValueEnum::FloatValue(f) => ctx.float_to_i64_bits(f),
            BasicValueEnum::IntValue(v) => {
                if v.get_type().get_bit_width() < 64 {
                    ctx.builder.build_int_z_extend(v, i64_ty, "zext_i64").map_err(|e| e.to_string())
                } else {
                    Ok(v)
                }
            }
            _ => Err(format!("value_to_i64: unhandled LLVM value type {:?}", val)),
        },
    }
}

/// Reinterpret an i64 slot value back to the given Jade type.
pub fn i64_to_value<'ctx>(
    raw: IntValue<'ctx>,
    ty: &JadeType,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    match effective_ty(ty) {
        JadeType::Float => Ok(ctx.i64_bits_to_float(raw)?.into()),
        JadeType::Bool => {
            let b = ctx.builder
                .build_int_truncate(raw, ctx.context.bool_type(), "i2bool")
                .map_err(|e| e.to_string())?;
            Ok(b.into())
        }
        JadeType::Str | JadeType::Array(_) | JadeType::Struct(_)
        | JadeType::Dict | JadeType::Fn { .. } | JadeType::AsyncFn { .. }
        | JadeType::Future(_) | JadeType::Prompt => {
            let p = ctx.builder
                .build_int_to_ptr(raw, ptr_ty, "i2ptr")
                .map_err(|e| e.to_string())?;
            Ok(p.into())
        }
        _ => Ok(raw.into()),
    }
}

// ── Type helpers ──────────────────────────────────────────────────────────────

/// Extract a `PointerValue` from `val`.
///
/// Function parameters typed as `Unknown` are allocated as `i64` in the entry
/// block and then loaded back as `IntValue`.  When the actual runtime value is a
/// heap pointer (array header, struct, …) we need to cast the integer back to a
/// pointer before using it in GEP / struct_gep instructions.
///
/// If `val` is already a `PointerValue` this is a no-op.
/// If `val` is an `IntValue` we emit `inttoptr` to recover the pointer.
pub fn as_pointer<'ctx>(
    val: BasicValueEnum<'ctx>,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<inkwell::values::PointerValue<'ctx>, String> {
    match val {
        BasicValueEnum::PointerValue(p) => Ok(p),
        BasicValueEnum::IntValue(i) => {
            let ptr_ty = ctx.context.ptr_type(inkwell::AddressSpace::default());
            ctx.builder
                .build_int_to_ptr(i, ptr_ty, "i2ptr_coerce")
                .map_err(|e| e.to_string())
        }
        other => Err(format!(
            "expected pointer or int-as-pointer, got {:?}",
            other
        )),
    }
}

/// Collapse `Unknown` → `Int` for dispatch purposes.
fn effective_ty(ty: &JadeType) -> JadeType {
    match ty {
        JadeType::Unknown => JadeType::Int,
        other => other.clone(),
    }
}

/// Refine a declared type based on the actual LLVM value produced.
/// When struct field types differ from what the TIR says (Unknown → Float, etc.)
/// this prevents panics in emit_binop.
fn actual_ty<'ctx>(declared: &JadeType, val: &BasicValueEnum<'ctx>) -> JadeType {
    match val {
        BasicValueEnum::FloatValue(_) => JadeType::Float,
        BasicValueEnum::PointerValue(_) => match declared {
            JadeType::Str | JadeType::Array(_) | JadeType::Struct(_)
            | JadeType::Dict | JadeType::Fn { .. } | JadeType::AsyncFn { .. }
            | JadeType::Future(_) | JadeType::Prompt => declared.clone(),
            _ => JadeType::Str,
        },
        BasicValueEnum::IntValue(_) => match declared {
            JadeType::Bool => JadeType::Bool,
            JadeType::Int | JadeType::Unknown | JadeType::Nil => effective_ty(declared),
            _ => JadeType::Int,
        },
        _ => declared.clone(),
    }
}

/// Coerce `val` from `actual_ty` to `target_ty` when passing arguments.
fn coerce<'ctx>(
    val: BasicValueEnum<'ctx>,
    actual_ty: &JadeType,
    target_ty: &JadeType,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    // When the target parameter has no type annotation it is typed as `Unknown`
    // in the LLVM signature, which maps to i64.  Heap values (arrays, structs,
    // strings) are represented as pointers, so we must convert them to integers
    // before passing them across the call boundary.  The callee will cast them
    // back to pointers when it needs to dereference them.
    if matches!(target_ty, JadeType::Unknown) {
        if let BasicValueEnum::PointerValue(p) = val {
            let i = ctx.builder
                .build_ptr_to_int(p, ctx.context.i64_type(), "ptr2i_arg")
                .map_err(|e| e.to_string())?;
            return Ok(i.into());
        }
    }

    match (effective_ty(actual_ty), effective_ty(target_ty)) {
        (JadeType::Float, JadeType::Int) => {
            let v = ctx.builder
                .build_float_to_signed_int(val.into_float_value(), ctx.context.i64_type(), "f2i")
                .map_err(|e| e.to_string())?;
            Ok(v.into())
        }
        (JadeType::Int, JadeType::Float) => {
            let v = ctx.builder
                .build_signed_int_to_float(val.into_int_value(), ctx.context.f64_type(), "i2f")
                .map_err(|e| e.to_string())?;
            Ok(v.into())
        }
        (JadeType::Bool, JadeType::Int) => {
            let v = ctx.builder
                .build_int_z_extend(val.into_int_value(), ctx.context.i64_type(), "b2i")
                .map_err(|e| e.to_string())?;
            Ok(v.into())
        }
        _ => Ok(val),
    }
}
