use inkwell::{
    types::BasicMetadataTypeEnum,
    values::{
        AnyValue, AnyValueEnum, BasicMetadataValueEnum, BasicValueEnum,
        CallSiteValue, FunctionValue, IntValue, PointerValue,
    },
    AddressSpace, FloatPredicate, IntPredicate,
};

use crate::frontend::ast::{BinOpKind, UnaryOpKind};
use crate::compiler::tir::{JadeType, TExpr, TExprKind, TFStrPart};

use super::{stmt, stdlib, types, CodegenCtx};

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

        Str(s) => emit_tagged_literal(s, ctx),

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
            // Module-level global (defined at top level, stored as LLVM global).
            if let Some((global, ty)) = ctx.module_globals.get(name.as_str()).cloned() {
                let llvm_ty = types::jade_to_llvm(&ty, ctx.context);
                return ctx.builder
                    .build_load(llvm_ty, global.as_pointer_value(), name)
                    .map_err(|e| e.to_string());
            }
            // Named function referenced as a first-class value (not a direct call).
            if ctx.fn_info.contains_key(name.as_str()) {
                return emit_fn_as_value(name, ctx);
            }
            // Compiler-injected global identifiers.
            if name == "__model__" {
                // Resolve from $JADE_MODEL at runtime via the C runtime helper.
                return ctx.call_rv(ctx.jrt_get_model_fn, &[], "model_r");
            }
            if name == "__tokens__" {
                return Ok(ctx.context.i64_type().const_int(0, false).into());
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
        Call { callee, args, kwargs } => emit_call_with_kwargs(callee, args, kwargs, &expr.ty, ctx),

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

        // ── prompt <expr> ─────────────────────────────────────────────────────
        // A Prompt value is represented identically to a Str pointer at the
        // LLVM level — the type distinction is only semantic.
        PromptLiteral { body } => emit_expr(body, ctx),

        // ── ?prompt  /  ?prompt |> Type  /  ?prompt |> grammar_expr ─────────
        PromptDeref { expr: pexpr, output_type, grammar_expr } => {
            ctx.uses_prompts = true;

            // Load the prompt string pointer.
            let prompt_ptr = emit_expr(pexpr, ctx)?.into_pointer_value();

            // Model name: empty string — the runtime reads JADE_MODEL from env.
            let model_ptr = ctx.builder
                .build_global_string_ptr("", "jade_model_empty")
                .map_err(|e| e.to_string())?
                .as_pointer_value();

            // Grammar-constrained deref: jrt_prompt_grammar_ex(prompt, model, pattern, anchor, stop)
            if let Some(gexpr) = grammar_expr {
                let struct_ptr = as_pointer(emit_expr(gexpr, ctx)?, ctx)?;
                let ptr_ty = ctx.context.ptr_type(inkwell::AddressSpace::default());

                let f0 = ctx.builder.build_struct_gep(ctx.jade_grammar_ty, struct_ptr, 0, "gm_r0").map_err(|e| e.to_string())?;
                let pattern_ptr = ctx.builder.build_load(ptr_ty, f0, "gm_pattern").map_err(|e| e.to_string())?.into_pointer_value();

                let f1 = ctx.builder.build_struct_gep(ctx.jade_grammar_ty, struct_ptr, 1, "gm_r1").map_err(|e| e.to_string())?;
                let anchor_ptr = ctx.builder.build_load(ptr_ty, f1, "gm_anchor").map_err(|e| e.to_string())?.into_pointer_value();

                let f2 = ctx.builder.build_struct_gep(ctx.jade_grammar_ty, struct_ptr, 2, "gm_r2").map_err(|e| e.to_string())?;
                let stop_ptr = ctx.builder.build_load(ptr_ty, f2, "gm_stop").map_err(|e| e.to_string())?.into_pointer_value();

                let r = ctx.call_rv(
                    ctx.jrt_prompt_grammar_ex_fn,
                    &[prompt_ptr.into(), model_ptr.into(), pattern_ptr.into(), anchor_ptr.into(), stop_ptr.into()],
                    "infer_grammar_r",
                )?;
                let ptr = r.into_pointer_value();
                emit_null_check_and_exit(ptr, "jade: grammar-constrained ?p failed — jade-tree daemon unreachable or returned an error\n", ctx)?;
                return Ok(ptr.into());
            }

            match output_type {
                // ── Untyped: jrt_prompt(prompt, model) -> char* ──────────────
                None => {
                    let r = ctx.call_rv(ctx.jrt_prompt_fn, &[prompt_ptr.into(), model_ptr.into()], "infer_r")?;
                    let ptr = r.into_pointer_value();
                    emit_null_check_and_exit(ptr, "jade: ?p failed — jade-tree daemon unreachable or returned an error\n", ctx)?;
                    Ok(ptr.into())
                }

                // ── Typed: jrt_prompt_typed → parse to target type ────────────
                Some(type_name) => {
                    let type_ptr = ctx.builder
                        .build_global_string_ptr(type_name, "infer_type")
                        .map_err(|e| e.to_string())?
                        .as_pointer_value();
                    let max_retries = ctx.context.i32_type().const_int(3, false);

                    let result_ptr = ctx.call_rv(
                        ctx.jrt_prompt_typed_fn,
                        &[prompt_ptr.into(), model_ptr.into(), type_ptr.into(), max_retries.into()],
                        "infer_typed_r",
                    )?.into_pointer_value();

                    // Null → exhausted retries → exit(1).
                    let fn_val = ctx.builder.get_insert_block()
                        .and_then(|b| b.get_parent())
                        .ok_or("prompt deref outside function")?;
                    let ok_bb   = ctx.context.append_basic_block(fn_val, "infer_ok");
                    let fail_bb = ctx.context.append_basic_block(fn_val, "infer_fail");

                    let is_null = ctx.builder
                        .build_is_null(result_ptr, "infer_null")
                        .map_err(|e| e.to_string())?;
                    ctx.builder.build_conditional_branch(is_null, fail_bb, ok_bb)
                        .map_err(|e| e.to_string())?;

                    ctx.builder.position_at_end(fail_bb);
                    let err = ctx.builder
                        .build_global_string_ptr("jade: prompt type coercion exhausted retries\n", "infer_err")
                        .map_err(|e| e.to_string())?
                        .as_pointer_value();
                    ctx.builder.build_call(ctx.printf_fn, &[err.into()], "")
                        .map_err(|e| e.to_string())?;
                    ctx.builder.build_call(ctx.exit_fn,
                        &[ctx.context.i32_type().const_int(1, false).into()], "")
                        .map_err(|e| e.to_string())?;
                    ctx.builder.build_unreachable().map_err(|e| e.to_string())?;

                    ctx.builder.position_at_end(ok_bb);

                    // Parse the guaranteed-valid string to the target LLVM type.
                    match type_name.as_str() {
                        "int" => ctx.call_rv(ctx.atoll_fn, &[result_ptr.into()], "parsed_int"),
                        "float" => ctx.call_rv(ctx.strtod_fn,
                            &[result_ptr.into(),
                              ctx.context.ptr_type(AddressSpace::default()).const_null().into()],
                            "parsed_float"),
                        "bool" => {
                            let true_lit = ctx.builder
                                .build_global_string_ptr("true", "true_lit")
                                .map_err(|e| e.to_string())?
                                .as_pointer_value();
                            let cmp = ctx.call_rv(ctx.strcmp_fn,
                                &[result_ptr.into(), true_lit.into()], "strcmp_r")?
                                .into_int_value();
                            let is_true = ctx.builder
                                .build_int_compare(IntPredicate::EQ, cmp,
                                    ctx.context.i32_type().const_zero(), "is_true")
                                .map_err(|e| e.to_string())?;
                            Ok(is_true.into())
                        }
                        _ => Ok(result_ptr.into()), // "str" or unknown → return char*
                    }
                }
            }
        }
    }
}

/// Emit `if (ptr == NULL) { printf(msg); exit(1); }` at the current insert point,
/// positioning the builder at the success (non-NULL) block on return.
fn emit_null_check_and_exit<'ctx>(
    ptr: PointerValue<'ctx>,
    err_msg: &str,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<(), String> {
    let fn_val = ctx.builder.get_insert_block()
        .and_then(|b| b.get_parent())
        .ok_or("null check outside function")?;
    let ok_bb   = ctx.context.append_basic_block(fn_val, "rt_ok");
    let fail_bb = ctx.context.append_basic_block(fn_val, "rt_fail");

    let is_null = ctx.builder.build_is_null(ptr, "rt_isnull")
        .map_err(|e| e.to_string())?;
    ctx.builder.build_conditional_branch(is_null, fail_bb, ok_bb)
        .map_err(|e| e.to_string())?;

    ctx.builder.position_at_end(fail_bb);
    let err = ctx.builder.build_global_string_ptr(err_msg, "rt_err")
        .map_err(|e| e.to_string())?.as_pointer_value();
    ctx.builder.build_call(ctx.printf_fn, &[err.into()], "")
        .map_err(|e| e.to_string())?;
    ctx.builder.build_call(ctx.exit_fn,
        &[ctx.context.i32_type().const_int(1, false).into()], "")
        .map_err(|e| e.to_string())?;
    ctx.builder.build_unreachable().map_err(|e| e.to_string())?;

    ctx.builder.position_at_end(ok_bb);
    Ok(())
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

    // Unknown object with Str index → treat as dict (covers resp["status"], call["tool_name"], etc.)
    if matches!(&object.ty, JadeType::Unknown) && matches!(&index.ty, JadeType::Str) {
        ctx.uses_dicts = true;
        let dict_ptr = as_pointer(emit_expr(object, ctx)?, ctx)?;
        let key_ptr  = as_pointer(emit_expr(index, ctx)?, ctx)?;
        let raw = ctx.call_rv(
            ctx.jade_dict_get_fn,
            &[dict_ptr.into(), key_ptr.into()],
            "dict_idx_unk",
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

    // Look up bare name, falling back to stripping a module prefix (e.g. "tools.ToolGroup" → "ToolGroup").
    let bare_name = type_name.rsplit_once('.').map(|(_, b)| b).unwrap_or(type_name);
    let field_names = ctx.struct_field_order
        .get(type_name)
        .or_else(|| ctx.struct_field_order.get(bare_name))
        .cloned()
        .ok_or_else(|| format!("unknown struct type: {type_name}"))?;

    // Size by the struct's full field count, not the user-provided subset.
    // type_infer fills in defaults for known structs, but imports can pass
    // through a partial field list — without using the full count here we'd
    // write past the malloc'd buffer when the caller omits fields.
    let total = field_names.len().max(fields.len()) as u64;
    let struct_ptr = ctx.malloc_ptr(i64_ty.const_int((total + 1) * 8, false), "struct_ptr")?;

    // Slot 0: type name pointer (for typed catch dispatch).
    let type_name_lit = ctx.builder
        .build_global_string_ptr(type_name, "sty_name")
        .map_err(|e| e.to_string())?
        .as_pointer_value();
    let type_name_i64 = ctx.builder
        .build_ptr_to_int(type_name_lit, i64_ty, "sty_p2i")
        .map_err(|e| e.to_string())?;
    let slot0 = ctx.gep(i64_ty, struct_ptr, &[i64_ty.const_int(0, false)], "sty_slot")?;
    ctx.builder.build_store(slot0, type_name_i64).map_err(|e| e.to_string())?;

    // Zero-initialize any field slots the caller didn't supply (nil-ish).
    for idx in 0..field_names.len() {
        if fields.iter().any(|(n, _, _)| n == &field_names[idx]) { continue; }
        let slot = ctx.gep(i64_ty, struct_ptr, &[i64_ty.const_int((idx as u64) + 1, false)], "sf_zero")?;
        ctx.builder.build_store(slot, i64_ty.const_int(0, false)).map_err(|e| e.to_string())?;
    }

    for (field_name, field_expr, _) in fields {
        let idx = field_names
            .iter()
            .position(|n| n == field_name)
            .ok_or_else(|| format!("unknown field '{field_name}' on struct '{type_name}'"))?;

        let val = emit_expr(field_expr, ctx)?;
        let as_i64 = value_to_i64(val, &field_expr.ty, ctx)?;
        // +1 to skip the type_name slot at slot 0
        let slot = ctx.gep(i64_ty, struct_ptr, &[i64_ty.const_int((idx as u64) + 1, false)], "sf_slot")?;
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
    // ── Dict field access: obj["field"] via jade_dict_get ─────────────────
    if matches!(&object.ty, JadeType::Dict) {
        ctx.uses_dicts = true;
        let dict_ptr = as_pointer(emit_expr(object, ctx)?, ctx)?;
        let key_lit = ctx.builder
            .build_global_string_ptr(field, "fa_dict_key")
            .map_err(|e| e.to_string())?
            .as_pointer_value();
        let raw = ctx.call_rv(ctx.jade_dict_get_fn, &[dict_ptr.into(), key_lit.into()], "fa_dg")?
            .into_int_value();
        return i64_to_value(raw, result_ty, ctx);
    }

    let type_name = match &object.ty {
        JadeType::Struct(n) => n.clone(),
        // Inside method bodies `self` is Unknown-typed in TIR; resolve via scope.
        JadeType::Unknown => {
            if let TExprKind::Identifier(var) = &object.kind {
                match ctx.lookup(var).map(|(_, ty)| ty) {
                    Some(JadeType::Struct(n)) => n,
                    _ => {
                        // Check for magic function attributes (__name__, __params__)
                        if field == "__name__" {
                            let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
                            let fn_ptr = as_pointer(emit_expr(object, ctx)?, ctx)?;
                            let f2 = ctx.builder
                                .build_struct_gep(ctx.jade_fn_ty, fn_ptr, 2, "fn_name_slot")
                                .map_err(|e| e.to_string())?;
                            return ctx.builder.build_load(ptr_ty, f2, "fn_name")
                                .map_err(|e| e.to_string());
                        }
                        if field == "__params__" {
                            let empty = ctx.builder
                                .build_global_string_ptr("", "empty_params")
                                .map_err(|e| e.to_string())?
                                .as_pointer_value();
                            return Ok(empty.into());
                        }
                        // Treat as dict: use jade_dict_get with field name as key
                        ctx.uses_dicts = true;
                        let dict_ptr = as_pointer(emit_expr(object, ctx)?, ctx)?;
                        let key_lit = ctx.builder
                            .build_global_string_ptr(field, "fa_unk_key")
                            .map_err(|e| e.to_string())?
                            .as_pointer_value();
                        let raw = ctx.call_rv(
                            ctx.jade_dict_get_fn,
                            &[dict_ptr.into(), key_lit.into()],
                            "fa_unk_dg",
                        )?.into_int_value();
                        return i64_to_value(raw, result_ty, ctx);
                    }
                }
            } else {
                // Non-identifier Unknown expression: treat as dict
                if field == "__name__" {
                    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
                    let fn_ptr = as_pointer(emit_expr(object, ctx)?, ctx)?;
                    let f2 = ctx.builder
                        .build_struct_gep(ctx.jade_fn_ty, fn_ptr, 2, "fn_name_slot2")
                        .map_err(|e| e.to_string())?;
                    return ctx.builder.build_load(ptr_ty, f2, "fn_name2")
                        .map_err(|e| e.to_string());
                }
                ctx.uses_dicts = true;
                let dict_ptr = as_pointer(emit_expr(object, ctx)?, ctx)?;
                let key_lit = ctx.builder
                    .build_global_string_ptr(field, "fa_unk2_key")
                    .map_err(|e| e.to_string())?
                    .as_pointer_value();
                let raw = ctx.call_rv(
                    ctx.jade_dict_get_fn,
                    &[dict_ptr.into(), key_lit.into()],
                    "fa_unk2_dg",
                )?.into_int_value();
                return i64_to_value(raw, result_ty, ctx);
            }
        }
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
    // +1 to skip the type_name slot at slot 0
    let slot = ctx.gep(i64_ty, struct_ptr, &[i64_ty.const_int((idx as u64) + 1, false)], "fa_slot")?;
    let raw = ctx.builder
        .build_load(i64_ty, slot, "fa_raw")
        .map_err(|e| e.to_string())?
        .into_int_value();

    i64_to_value(raw, &field_ty, ctx)
}

// ── F-string ──────────────────────────────────────────────────────────────────

const FSTR_BUF_SIZE: u64 = 4096;

fn emit_fstr<'ctx>(
    parts: &[TFStrPart],
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let i8_ty  = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();

    // Stage into a stack buffer first; copy into tagged heap allocation at end.
    let staging = ctx.builder
        .build_array_alloca(i8_ty, i64_ty.const_int(FSTR_BUF_SIZE, false), "fstr_buf")
        .map_err(|e| e.to_string())?;

    let pos_slot = ctx.builder
        .build_alloca(i64_ty, "fstr_pos")
        .map_err(|e| e.to_string())?;
    ctx.builder
        .build_store(pos_slot, i64_ty.const_int(0, false))
        .map_err(|e| e.to_string())?;

    // Accumulate trust across any string-typed interpolated expressions.
    let trust_slot = ctx.builder
        .build_alloca(i8_ty, "fstr_trust")
        .map_err(|e| e.to_string())?;
    ctx.builder
        .build_store(trust_slot, i8_ty.const_int(JRT_TRUSTED_LIT, false))
        .map_err(|e| e.to_string())?;

    for part in parts {
        let pos = ctx.builder
            .build_load(i64_ty, pos_slot, "fstr_pos_v")
            .map_err(|e| e.to_string())?
            .into_int_value();
        let remaining = ctx.builder
            .build_int_sub(i64_ty.const_int(FSTR_BUF_SIZE, false), pos, "fstr_rem")
            .map_err(|e| e.to_string())?;
        let write_ptr = ctx.gep(i8_ty, staging, &[pos], "fstr_wptr")?;

        let written = match part {
            TFStrPart::Literal(s) => {
                // Literal strings within an f-string itself are trusted; emit
                // them as plain `%s` writes from a non-tagged global (they
                // never escape the f-string body as a standalone value).
                let lit = ctx.builder
                    .build_global_string_ptr(s, "fstr_lit")
                    .map_err(|e| e.to_string())?
                    .as_pointer_value();
                let fmt = ctx.builder
                    .build_global_string_ptr("%s", "fstr_sfmt")
                    .map_err(|e| e.to_string())?
                    .as_pointer_value();
                let call = ctx.builder
                    .build_call(ctx.snprintf_fn, &[write_ptr.into(), remaining.into(), fmt.into(), lit.into()], "snp_lit")
                    .map_err(|e| e.to_string())?;
                extract_i32_from_call(call, ctx)?
            }
            TFStrPart::Expr(e) => {
                let val = emit_expr(e, ctx)?;
                // For string-typed parts, OR in their trust byte. Strings may
                // arrive as PointerValue OR as IntValue (i64 representation of
                // a pointer — used in jade_value_t-typed paths like dict.get).
                if matches!(effective_ty(&e.ty), JadeType::Str) {
                    let p = match val {
                        BasicValueEnum::PointerValue(pv) => pv,
                        BasicValueEnum::IntValue(iv) => ctx.builder
                            .build_int_to_ptr(iv, ctx.context.ptr_type(AddressSpace::default()), "fstr_iv2p")
                            .map_err(|e| e.to_string())?,
                        _ => return Err("fstring: unexpected str value kind".into()),
                    };
                    let t = emit_jrt_trust_of(p, ctx, "fstr_part_trust")?;
                    let cur = ctx.builder
                        .build_load(i8_ty, trust_slot, "fstr_trust_v")
                        .map_err(|e| e.to_string())?
                        .into_int_value();
                    let new = ctx.builder
                        .build_or(cur, t, "fstr_trust_or")
                        .map_err(|e| e.to_string())?;
                    ctx.builder.build_store(trust_slot, new).map_err(|e| e.to_string())?;
                }
                emit_snprintf_value(val, &e.ty, write_ptr, remaining, ctx)?
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

    // Allocate a tagged heap copy of [0..pos) and memcpy from staging.
    let final_pos = ctx.builder
        .build_load(i64_ty, pos_slot, "fstr_final_pos")
        .map_err(|e| e.to_string())?
        .into_int_value();
    let final_trust = ctx.builder
        .build_load(i8_ty, trust_slot, "fstr_final_trust")
        .map_err(|e| e.to_string())?
        .into_int_value();
    let heap_buf = emit_jrt_str_new(final_pos, final_trust, ctx, "fstr_out")?;

    // memcpy(heap_buf, staging, final_pos). Use sprintf-style loop via memcpy
    // intrinsic isn't trivially available; use a small inline copy via memcpy
    // declared as an extern. For simplicity we call snprintf("%s") again,
    // which is acceptable since staging is NUL-terminated by snprintf's last
    // write. Use memcpy via printf-family is fragile — declare and use memcpy.
    // Inline-build memcpy declaration once via module:
    let memcpy_fn = match ctx.module.get_function("memcpy") {
        Some(f) => f,
        None => {
            let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
            let memcpy_ty = ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into(), i64_ty.into()], false);
            ctx.module.add_function("memcpy", memcpy_ty, None)
        }
    };
    ctx.builder
        .build_call(memcpy_fn, &[heap_buf.into(), staging.into(), final_pos.into()], "fstr_cp")
        .map_err(|e| e.to_string())?;

    Ok(heap_buf.into())
}

fn emit_snprintf_value<'ctx>(
    val: BasicValueEnum<'ctx>,
    ty: &JadeType,
    write_ptr: PointerValue<'ctx>,
    remaining: IntValue<'ctx>,
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
            ctx.builder.build_call(ctx.snprintf_fn, &[write_ptr.into(), remaining.into(), fmt.into(), val.into()], "snp_int")
                .map_err(|e| e.to_string())?
        }
        JadeType::Float => {
            let fmt = mk("%g", ctx)?;
            ctx.builder.build_call(ctx.snprintf_fn, &[write_ptr.into(), remaining.into(), fmt.into(), val.into()], "snp_flt")
                .map_err(|e| e.to_string())?
        }
        JadeType::Bool => {
            let t = mk("true", ctx)?;
            let f = mk("false", ctx)?;
            let sel = ctx.builder
                .build_select(val.into_int_value(), t, f, "snp_bsel")
                .map_err(|e| e.to_string())?;
            let fmt = mk("%s", ctx)?;
            ctx.builder.build_call(ctx.snprintf_fn, &[write_ptr.into(), remaining.into(), fmt.into(), sel.into()], "snp_bool")
                .map_err(|e| e.to_string())?
        }
        JadeType::Str | JadeType::Grammar => {
            let fmt = mk("%s", ctx)?;
            ctx.builder.build_call(ctx.snprintf_fn, &[write_ptr.into(), remaining.into(), fmt.into(), val.into()], "snp_str")
                .map_err(|e| e.to_string())?
        }
        _ => {
            let fmt = mk("%lld", ctx)?;
            ctx.builder.build_call(ctx.snprintf_fn, &[write_ptr.into(), remaining.into(), fmt.into(), val.into()], "snp_unk")
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

// ── Tagged-string helpers ─────────────────────────────────────────────────────

const JRT_TRUSTED_LIT: u64 = 0;
const JRT_TAINTED_LIT: u64 = 1;

/// Emit a string literal as a globally-allocated tagged array
/// `[trust:i8 = 0][bytes…][NUL:i8]` and return a pointer one byte past
/// the trust header (the canonical data pointer).
fn emit_tagged_literal<'ctx>(
    s: &str,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let i8_ty  = ctx.context.i8_type();
    let i32_ty = ctx.context.i32_type();

    // [trust][bytes...][nul]
    let bytes = s.as_bytes();
    let mut data: Vec<u8> = Vec::with_capacity(bytes.len() + 2);
    data.push(JRT_TRUSTED_LIT as u8);
    data.extend_from_slice(bytes);
    data.push(0);

    let arr_ty  = i8_ty.array_type(data.len() as u32);
    let const_arr = ctx.context.const_string(&data, false);
    let global = ctx.module.add_global(arr_ty, None, "str_lit_t");
    global.set_initializer(&const_arr);
    global.set_linkage(inkwell::module::Linkage::Internal);
    global.set_constant(true);

    let zero = i32_ty.const_zero();
    let one  = i32_ty.const_int(1, false);
    let data_ptr = unsafe {
        ctx.builder
            .build_in_bounds_gep(arr_ty, global.as_pointer_value(), &[zero, one], "lit_data")
            .map_err(|e| e.to_string())?
    };
    Ok(data_ptr.into())
}

/// Allocate a tagged string of `len` bytes with `trust` byte already
/// written at offset -1 and NUL at offset `len`. Returns the data pointer.
fn emit_jrt_str_new<'ctx>(
    len: IntValue<'ctx>,
    trust: IntValue<'ctx>,
    ctx: &mut CodegenCtx<'ctx>,
    name: &str,
) -> Result<PointerValue<'ctx>, String> {
    ctx.uses_runtime = true;
    let cs = ctx.builder
        .build_call(ctx.jrt_str_new_fn, &[len.into(), trust.into()], name)
        .map_err(|e| e.to_string())?;
    Ok(cs.as_any_value_enum().into_pointer_value())
}

/// Read the trust byte of a tagged string at runtime.
fn emit_jrt_trust_of<'ctx>(
    ptr: PointerValue<'ctx>,
    ctx: &mut CodegenCtx<'ctx>,
    name: &str,
) -> Result<IntValue<'ctx>, String> {
    ctx.uses_runtime = true;
    let cs = ctx.builder
        .build_call(ctx.jrt_trust_of_fn, &[ptr.into()], name)
        .map_err(|e| e.to_string())?;
    Ok(cs.as_any_value_enum().into_int_value())
}

// ── Int → heap char* (for mixed-type Add) ────────────────────────────────────

fn emit_int_to_str<'ctx>(
    val: IntValue<'ctx>,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<PointerValue<'ctx>, String> {
    let i8_ty  = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();

    // 22 chars max for i64; round up to 32. Allocate tagged TRUSTED.
    let buf = emit_jrt_str_new(
        i64_ty.const_int(32, false),
        i8_ty.const_int(JRT_TRUSTED_LIT, false),
        ctx,
        "int2s_buf",
    )?;
    let fmt = ctx.builder
        .build_global_string_ptr("%lld", "int2s_fmt")
        .map_err(|e| e.to_string())?
        .as_pointer_value();
    ctx.builder
        .build_call(ctx.snprintf_fn,
                    &[buf.into(), i64_ty.const_int(33, false).into(), fmt.into(), val.into()],
                    "int2s")
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

// ── String concatenation ──────────────────────────────────────────────────────

fn emit_str_concat<'ctx>(
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let i8_ty  = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let lp = lhs.into_pointer_value();
    let rp = rhs.into_pointer_value();

    let ll = ctx.call_rv(ctx.strlen_fn, &[lp.into()], "llen")?.into_int_value();
    let rl = ctx.call_rv(ctx.strlen_fn, &[rp.into()], "rlen")?.into_int_value();
    let total = ctx.builder.build_int_add(ll, rl, "concat_len").map_err(|e| e.to_string())?;

    // Compute trust = jrt_trust_of(lhs) | jrt_trust_of(rhs).
    let lt = emit_jrt_trust_of(lp, ctx, "ltrust")?;
    let rt = emit_jrt_trust_of(rp, ctx, "rtrust")?;
    let trust = ctx.builder.build_or(lt, rt, "concat_trust").map_err(|e| e.to_string())?;
    // OR is enough since TRUSTED=0 and TAINTED=1.
    let _ = i8_ty; // suppress unused

    let buf = emit_jrt_str_new(total, trust, ctx, "concat_buf")?;
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
        JadeType::Unknown => {
            // Unknown return (e.g. from history() or split()) — treat as jade array.
            let i64_ty = ctx.context.i64_type();
            let arr_ptr = as_pointer(val, ctx)?;
            let f1 = ctx.builder
                .build_struct_gep(ctx.array_ty, arr_ptr, 1, "len_unk_f1")
                .map_err(|e| e.to_string())?;
            ctx.builder.build_load(i64_ty, f1, "unk_arr_len").map_err(|e| e.to_string())
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
            let raw = wrapper_fn
                .get_nth_param(i as u32)
                .ok_or_else(|| format!("wrapper fn '{name}' has no param {i}"))?
                .into_int_value();
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

    // Allocate a jade_fn_t: { &wrapper_fn, null, &name_str }
    let jade_fn_ptr = ctx.malloc_ptr(i64_ty.const_int(24, false), "named_fn_val")?;
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
    let name_lit = ctx.builder
        .build_global_string_ptr(name, "fn_name_lit")
        .map_err(|e| e.to_string())?
        .as_pointer_value();
    let f2 = ctx.builder
        .build_struct_gep(ctx.jade_fn_ty, jade_fn_ptr, 2, "nfv_f2")
        .map_err(|e| e.to_string())?;
    ctx.builder
        .build_store(f2, name_lit)
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
        let arg = cl_fn
            .get_nth_param(i as u32)
            .ok_or_else(|| format!("closure body has no param {i}"))?;
        ctx.builder.build_store(alloca, arg).map_err(|e| e.to_string())?;
        ctx.define(param_name.clone(), alloca, JadeType::Unknown);
    }

    // Restore captured variables from the env struct.
    let env_param = cl_fn
        .get_nth_param(params.len() as u32)
        .ok_or_else(|| "closure environment parameter missing — LLVM codegen error".to_string())?
        .into_pointer_value();
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

    // ── 3. Allocate jade_fn_t { &closure_N, env_ptr, null } ──────────────
    let ptr_ty_loc = ctx.context.ptr_type(AddressSpace::default());
    let jade_fn_ptr = ctx.malloc_ptr(i64_ty.const_int(24, false), "jade_fn")?;
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
    let f2 = ctx.builder
        .build_struct_gep(ctx.jade_fn_ty, jade_fn_ptr, 2, "cl_f2")
        .map_err(|e| e.to_string())?;
    ctx.builder.build_store(f2, ptr_ty_loc.const_null()).map_err(|e| e.to_string())?;

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
        (JadeType::Str, Add, JadeType::Int) => {
            let s = emit_int_to_str(rhs.into_int_value(), ctx)?;
            emit_str_concat(lhs, s.into(), ctx)
        }
        (JadeType::Int, Add, JadeType::Str) => {
            let s = emit_int_to_str(lhs.into_int_value(), ctx)?;
            emit_str_concat(s.into(), rhs, ctx)
        }

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

        // ── String comparisons (via strcmp) ───────────────────────────────────
        (JadeType::Str, Eq | Ne | Lt | Gt | Le | Ge, JadeType::Str) => {
            let lp = as_pointer(lhs, ctx)?;
            let rp = as_pointer(rhs, ctx)?;
            let cmp = ctx.call_rv(ctx.strcmp_fn, &[lp.into(), rp.into()], "scmp")?.into_int_value();
            let zero = ctx.context.i32_type().const_zero();
            let pred = match op {
                Eq => IntPredicate::EQ,
                Ne => IntPredicate::NE,
                Lt => IntPredicate::SLT,
                Gt => IntPredicate::SGT,
                Le => IntPredicate::SLE,
                Ge => IntPredicate::SGE,
                _ => unreachable!(),
            };
            Ok(ctx.builder.build_int_compare(pred, cmp, zero, "scmp_r").map_err(|e| e.to_string())?.into())
        }

        // ── Nil comparisons — nil is i64(0), compare as integers ─────────────
        (_, Eq, JadeType::Nil) | (JadeType::Nil, Eq, _) => {
            let lv = value_to_i64(lhs, lty, ctx)?;
            let rv = value_to_i64(rhs, rty, ctx)?;
            icmp(IntPredicate::EQ, lv.into(), rv.into(), ctx)
        }
        (_, Ne, JadeType::Nil) | (JadeType::Nil, Ne, _) => {
            let lv = value_to_i64(lhs, lty, ctx)?;
            let rv = value_to_i64(rhs, rty, ctx)?;
            icmp(IntPredicate::NE, lv.into(), rv.into(), ctx)
        }

        // ── Mixed Int/Bool comparisons ────────────────────────────────────────
        // Dict values are stored as i64; bool values from literals are i1.
        // Extend the narrower to i64 for comparison.
        (JadeType::Int, Eq, JadeType::Bool) | (JadeType::Bool, Eq, JadeType::Int) => {
            let i64_ty = ctx.context.i64_type();
            let lv = if lhs.into_int_value().get_type().get_bit_width() < 64 {
                ctx.builder.build_int_z_extend(lhs.into_int_value(), i64_ty, "ib_ext_l")
                    .map_err(|e| e.to_string())?.into()
            } else { lhs };
            let rv = if rhs.into_int_value().get_type().get_bit_width() < 64 {
                ctx.builder.build_int_z_extend(rhs.into_int_value(), i64_ty, "ib_ext_r")
                    .map_err(|e| e.to_string())?.into()
            } else { rhs };
            icmp(IntPredicate::EQ, lv, rv, ctx)
        }
        (JadeType::Int, Ne, JadeType::Bool) | (JadeType::Bool, Ne, JadeType::Int) => {
            let i64_ty = ctx.context.i64_type();
            let lv = if lhs.into_int_value().get_type().get_bit_width() < 64 {
                ctx.builder.build_int_z_extend(lhs.into_int_value(), i64_ty, "ib_ext_l")
                    .map_err(|e| e.to_string())?.into()
            } else { lhs };
            let rv = if rhs.into_int_value().get_type().get_bit_width() < 64 {
                ctx.builder.build_int_z_extend(rhs.into_int_value(), i64_ty, "ib_ext_r")
                    .map_err(|e| e.to_string())?.into()
            } else { rhs };
            icmp(IntPredicate::NE, lv, rv, ctx)
        }

        // ── Mixed Int/Str comparisons ─────────────────────────────────────────
        // Int and Str are never equal; return a compile-time constant so code
        // that reaches this (e.g. via dict type unification) still compiles.
        (JadeType::Int, Eq, JadeType::Str) | (JadeType::Str, Eq, JadeType::Int) => {
            Ok(ctx.context.bool_type().const_int(0, false).into())
        }
        (JadeType::Int, Ne, JadeType::Str) | (JadeType::Str, Ne, JadeType::Int) => {
            Ok(ctx.context.bool_type().const_int(1, false).into())
        }

        // ── Membership (in / not in) ──────────────────────────────────────────
        (_, In, _) => emit_contains(lhs, lty.clone(), rhs, rty.clone(), ctx),
        (_, NotIn, _) => {
            let b = emit_contains(lhs, lty.clone(), rhs, rty.clone(), ctx)?.into_int_value();
            Ok(ctx.builder.build_not(b, "not_in").map_err(|e| e.to_string())?.into())
        }

        _ => Err(format!(
            "unsupported binary op {:?} for types {:?} × {:?} in LLVM backend",
            op, lty, rty
        )),
    }
}

// ── Membership: needle `in` haystack ─────────────────────────────────────────

fn emit_contains<'ctx>(
    needle: BasicValueEnum<'ctx>,
    needle_ty: JadeType,
    haystack: BasicValueEnum<'ctx>,
    haystack_ty: JadeType,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    match effective_ty(&haystack_ty) {
        // ── key in dict: jade_dict_has(dict, key) != 0 ───────────────────────
        JadeType::Dict => {
            ctx.uses_dicts = true;
            let dict_ptr = as_pointer(haystack, ctx)?;
            let key_ptr  = as_pointer(needle, ctx)?;
            let res = ctx.call_rv(
                ctx.jade_dict_has_fn,
                &[dict_ptr.into(), key_ptr.into()],
                "dict_has",
            )?.into_int_value();
            let zero = ctx.context.i32_type().const_zero();
            let found = ctx.builder
                .build_int_compare(IntPredicate::NE, res, zero, "dict_has_b")
                .map_err(|e| e.to_string())?;
            Ok(found.into())
        }

        // ── str in str: strstr(haystack, needle) != NULL ──────────────────────
        JadeType::Str => {
            let hs = as_pointer(haystack, ctx)?;
            let nd = as_pointer(needle, ctx)?;
            let res = ctx.call_rv(ctx.strstr_fn, &[hs.into(), nd.into()], "strstr_r")?
                .into_pointer_value();
            let found = ctx.builder
                .build_is_not_null(res, "in_str")
                .map_err(|e| e.to_string())?;
            Ok(found.into())
        }

        // ── x in array: linear scan ───────────────────────────────────────────
        // All array elements are stored as i64; for str elements use strcmp,
        // for Int/Float/Bool use i64 equality.
        JadeType::Array(ref elem_ty) => {
            let i64_ty = ctx.context.i64_type();
            let ptr_ty = ctx.context.ptr_type(inkwell::AddressSpace::default());
            let bool_ty = ctx.context.bool_type();
            let fn_val = ctx.builder.get_insert_block()
                .and_then(|bb| bb.get_parent())
                .ok_or("'in' outside function")?;

            let arr_ptr = as_pointer(haystack, ctx)?;
            let f1 = ctx.builder.build_struct_gep(ctx.array_ty, arr_ptr, 1, "in_f1")
                .map_err(|e| e.to_string())?;
            let arr_len = ctx.builder.build_load(i64_ty, f1, "in_len")
                .map_err(|e| e.to_string())?.into_int_value();
            let f0 = ctx.builder.build_struct_gep(ctx.array_ty, arr_ptr, 0, "in_f0")
                .map_err(|e| e.to_string())?;
            let data_ptr = ctx.builder.build_load(ptr_ty, f0, "in_data")
                .map_err(|e| e.to_string())?.into_pointer_value();

            let needle_i64 = value_to_i64(needle, &needle_ty, ctx)?;

            let result_slot = ctx.build_entry_alloca(bool_ty.into(), "in_result")?;
            let i_slot      = ctx.build_entry_alloca(i64_ty.into(), "in_i")?;
            ctx.builder.build_store(result_slot, bool_ty.const_int(0, false))
                .map_err(|e| e.to_string())?;
            ctx.builder.build_store(i_slot, i64_ty.const_int(0, false))
                .map_err(|e| e.to_string())?;

            // Blocks: cond → body → next → cond (exit on found or exhausted)
            let cond_bb  = ctx.context.append_basic_block(fn_val, "in_cond");
            let body_bb  = ctx.context.append_basic_block(fn_val, "in_body");
            let found_bb = ctx.context.append_basic_block(fn_val, "in_found");
            let next_bb  = ctx.context.append_basic_block(fn_val, "in_next");
            let exit_bb  = ctx.context.append_basic_block(fn_val, "in_exit");

            ctx.builder.build_unconditional_branch(cond_bb).map_err(|e| e.to_string())?;

            // cond: i < len
            ctx.builder.position_at_end(cond_bb);
            let i_v = ctx.builder.build_load(i64_ty, i_slot, "in_i_v")
                .map_err(|e| e.to_string())?.into_int_value();
            let lt = ctx.builder.build_int_compare(inkwell::IntPredicate::SLT, i_v, arr_len, "in_lt")
                .map_err(|e| e.to_string())?;
            ctx.builder.build_conditional_branch(lt, body_bb, exit_bb)
                .map_err(|e| e.to_string())?;

            // body: load element, compare
            ctx.builder.position_at_end(body_bb);
            let i_v2 = ctx.builder.build_load(i64_ty, i_slot, "in_i2")
                .map_err(|e| e.to_string())?.into_int_value();
            let elem_slot = ctx.gep(i64_ty, data_ptr, &[i_v2], "in_eslot")?;
            let raw = ctx.builder.build_load(i64_ty, elem_slot, "in_raw")
                .map_err(|e| e.to_string())?.into_int_value();

            let eq = match effective_ty(elem_ty) {
                JadeType::Str => {
                    let ep = ctx.builder.build_int_to_ptr(raw, ptr_ty, "in_ep")
                        .map_err(|e| e.to_string())?;
                    let np = ctx.builder.build_int_to_ptr(needle_i64, ptr_ty, "in_np")
                        .map_err(|e| e.to_string())?;
                    let cmp = ctx.call_rv(ctx.strcmp_fn, &[ep.into(), np.into()], "in_sc")?
                        .into_int_value();
                    ctx.builder.build_int_compare(inkwell::IntPredicate::EQ, cmp,
                        ctx.context.i32_type().const_zero(), "in_seq")
                        .map_err(|e| e.to_string())?
                }
                _ => ctx.builder.build_int_compare(inkwell::IntPredicate::EQ, raw, needle_i64, "in_eq")
                    .map_err(|e| e.to_string())?,
            };
            ctx.builder.build_conditional_branch(eq, found_bb, next_bb)
                .map_err(|e| e.to_string())?;

            // found: set result = true, exit
            ctx.builder.position_at_end(found_bb);
            ctx.builder.build_store(result_slot, bool_ty.const_int(1, false))
                .map_err(|e| e.to_string())?;
            ctx.builder.build_unconditional_branch(exit_bb).map_err(|e| e.to_string())?;

            // next: increment i, loop
            ctx.builder.position_at_end(next_bb);
            let i_v3 = ctx.builder.build_load(i64_ty, i_slot, "in_i3")
                .map_err(|e| e.to_string())?.into_int_value();
            let inc = ctx.builder.build_int_add(i_v3, i64_ty.const_int(1, false), "in_inc")
                .map_err(|e| e.to_string())?;
            ctx.builder.build_store(i_slot, inc).map_err(|e| e.to_string())?;
            ctx.builder.build_unconditional_branch(cond_bb).map_err(|e| e.to_string())?;

            ctx.builder.position_at_end(exit_bb);
            let result = ctx.builder.build_load(bool_ty, result_slot, "in_res_v")
                .map_err(|e| e.to_string())?;
            Ok(result)
        }

        _ => Err(format!(
            "'in' operator not supported for haystack type {:?} in LLVM backend",
            haystack_ty
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

// ── Math stdlib inline emission ───────────────────────────────────────────────

fn math_libc_fn<'ctx>(name: &str, arity: usize, ctx: &mut CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function(name) { return f; }
    let f64_ty = ctx.context.f64_type();
    let params: Vec<BasicMetadataTypeEnum<'ctx>> = (0..arity)
        .map(|_| BasicMetadataTypeEnum::FloatType(f64_ty))
        .collect();
    ctx.module.add_function(name, f64_ty.fn_type(&params, false), None)
}

fn math_promote<'ctx>(arg: &TExpr, ctx: &mut CodegenCtx<'ctx>) -> Result<BasicValueEnum<'ctx>, String> {
    let f64_ty = ctx.context.f64_type();
    let val = emit_expr(arg, ctx)?;
    match val {
        BasicValueEnum::FloatValue(_) => Ok(val),
        BasicValueEnum::IntValue(i) => Ok(ctx.builder
            .build_signed_int_to_float(i, f64_ty, "math_itof")
            .map_err(|e| e.to_string())?.into()),
        BasicValueEnum::PointerValue(p) => {
            let bits = ctx.builder.build_ptr_to_int(p, ctx.context.i64_type(), "mp2i")
                .map_err(|e| e.to_string())?;
            Ok(ctx.i64_bits_to_float(bits)?.into())
        }
        _ => Err("math: unexpected argument type".to_string()),
    }
}

fn emit_math_call<'ctx>(
    method: &str,
    args: &[TExpr],
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    match method {
        "floor" | "ceil" | "sqrt" => {
            let libc = match method { "floor" => "floor", "ceil" => "ceil", _ => "sqrt" };
            let f = math_libc_fn(libc, 1, ctx);
            let a = math_promote(args.first().ok_or_else(|| format!("math.{method}: missing arg"))?, ctx)?;
            ctx.call_rv(f, &[a.into()], "math_r")
        }
        "abs" => {
            let f = math_libc_fn("fabs", 1, ctx);
            let a = math_promote(args.first().ok_or_else(|| "math.abs: missing arg".to_string())?, ctx)?;
            ctx.call_rv(f, &[a.into()], "math_r")
        }
        "min" => {
            let f = math_libc_fn("fmin", 2, ctx);
            let a = math_promote(args.first().ok_or_else(|| "math.min: missing arg 0".to_string())?, ctx)?;
            let b = math_promote(args.get(1).ok_or_else(|| "math.min: missing arg 1".to_string())?, ctx)?;
            ctx.call_rv(f, &[a.into(), b.into()], "math_r")
        }
        "max" => {
            let f = math_libc_fn("fmax", 2, ctx);
            let a = math_promote(args.first().ok_or_else(|| "math.max: missing arg 0".to_string())?, ctx)?;
            let b = math_promote(args.get(1).ok_or_else(|| "math.max: missing arg 1".to_string())?, ctx)?;
            ctx.call_rv(f, &[a.into(), b.into()], "math_r")
        }
        "pow" => {
            let f = math_libc_fn("pow", 2, ctx);
            let a = math_promote(args.first().ok_or_else(|| "math.pow: missing arg 0".to_string())?, ctx)?;
            let b = math_promote(args.get(1).ok_or_else(|| "math.pow: missing arg 1".to_string())?, ctx)?;
            ctx.call_rv(f, &[a.into(), b.into()], "math_r")
        }
        _ => Err(format!("math.{method}: unknown method")),
    }
}

// ── Function calls ────────────────────────────────────────────────────────────

/// Resolve keyword arguments into positional order, then delegate to emit_call.
fn emit_call_with_kwargs<'ctx>(
    callee: &TExpr,
    args: &[TExpr],
    kwargs: &[(String, TExpr)],
    ret_ty: &JadeType,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    // stream() needs kwargs intact (mute_on=) — intercept before the fallback
    // flattening drops them.
    if let TExprKind::Identifier(name) = &callee.kind {
        if name == "stream" {
            return emit_stream_with_kwargs(args, kwargs, ctx);
        }
    }

    if kwargs.is_empty() {
        return emit_call(callee, args, ret_ty, ctx);
    }

    // Only positional-resolution for direct named calls that we know about.
    if let TExprKind::Identifier(fn_name) = &callee.kind {
        if let Some(param_names) = ctx.fn_param_names.get(fn_name.as_str()).cloned() {
            let n = param_names.len();
            let mut slots: Vec<Option<TExpr>> = (0..n).map(|_| None).collect();

            // Fill positional slots first.
            for (i, arg) in args.iter().enumerate() {
                if i < n { slots[i] = Some(arg.clone()); }
            }
            // Fill keyword slots.
            for (kw_name, kw_expr) in kwargs {
                if let Some(idx) = param_names.iter().position(|p| p == kw_name) {
                    slots[idx] = Some(kw_expr.clone());
                }
            }
            // Build a flat ordered arg list (missing slots get a zero literal).
            let ordered: Vec<TExpr> = slots.into_iter().map(|s| s.unwrap_or_else(|| {
                crate::compiler::tir::TExpr {
                    kind: TExprKind::Integer(0),
                    ty: JadeType::Int,
                    span: callee.span.clone(),
                }
            })).collect();
            return emit_call(callee, &ordered, ret_ty, ctx);
        }
    }

    // Fallback: append kwargs positionally after positional args.
    let mut all_args: Vec<TExpr> = args.to_vec();
    for (_, kw_expr) in kwargs { all_args.push(kw_expr.clone()); }
    emit_call(callee, &all_args, ret_ty, ctx)
}

/// Generic emitter for a table-driven C function call.
/// Handles Arg::Ptr/I64 coercion and Ret::Ptr/I64/Bool/Void/I64Typed return kinds.
fn emit_from_sig<'ctx>(
    sig: &stdlib::Sig,
    args: &[TExpr],
    ret_ty: &JadeType,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    if sig.uses_dicts { ctx.uses_dicts = true; }
    ctx.uses_runtime = true;
    let fn_val = ctx.extern_fn(sig);
    let mut llvm_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(sig.args.len());
    for (kind, arg_expr) in sig.args.iter().zip(args.iter()) {
        let v = emit_expr(arg_expr, ctx)?;
        let coerced: BasicMetadataValueEnum = match kind {
            stdlib::Arg::Ptr => as_pointer(v, ctx)?.into(),
            stdlib::Arg::I64 => value_to_i64(v, &arg_expr.ty, ctx)?.into(),
        };
        llvm_args.push(coerced);
    }
    let i64_ty = ctx.context.i64_type();
    match sig.ret {
        stdlib::Ret::Void => {
            ctx.call_void(fn_val, &llvm_args)?;
            Ok(i64_ty.const_int(0, false).into())
        }
        stdlib::Ret::Bool => {
            let r = ctx.call_rv(fn_val, &llvm_args, "sig_b")?.into_int_value();
            let b = ctx.builder.build_int_compare(
                inkwell::IntPredicate::NE, r,
                ctx.context.i32_type().const_zero(), "sig_bool"
            ).map_err(|e| e.to_string())?;
            Ok(b.into())
        }
        stdlib::Ret::I64Typed => {
            let raw = ctx.call_rv(fn_val, &llvm_args, "sig_tv")?.into_int_value();
            i64_to_value(raw, ret_ty, ctx)
        }
        _ => ctx.call_rv(fn_val, &llvm_args, "sig_r"),
    }
}

fn emit_call<'ctx>(
    callee: &TExpr,
    args: &[TExpr],
    ret_ty: &JadeType,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    // ── Grammar.new(pattern[, anchor[, stop_anchor]]) ─────────────────────────
    // Allocates a %jade.grammar = { ptr, ptr, ptr } struct on the heap.
    // Missing args (or nil) become null pointers.
    if let TExprKind::FieldAccess { object, field } = &callee.kind {
        if let TExprKind::Identifier(obj_name) = &object.kind {
            if obj_name == "Grammar" && field == "new" {
                let ptr_ty = ctx.context.ptr_type(inkwell::AddressSpace::default());
                let null_ptr = ptr_ty.const_null();

                // Helper: emit arg at index, returning null ptr if missing or nil.
                let emit_grammar_arg = |idx: usize, ctx: &mut CodegenCtx<'ctx>| -> Result<inkwell::values::PointerValue<'ctx>, String> {
                    if let Some(arg) = args.get(idx) {
                        if matches!(arg.ty, JadeType::Nil) {
                            return Ok(null_ptr);
                        }
                        let v = emit_expr(arg, ctx)?;
                        as_pointer(v, ctx)
                    } else {
                        Ok(null_ptr)
                    }
                };

                let pattern_ptr    = emit_grammar_arg(0, ctx)?;
                let anchor_ptr     = emit_grammar_arg(1, ctx)?;
                let stop_ptr       = emit_grammar_arg(2, ctx)?;

                // Allocate a %jade.grammar struct (3 ptrs = 24 bytes).
                let i64_ty = ctx.context.i64_type();
                let struct_ptr = ctx.malloc_ptr(i64_ty.const_int(24, false), "grammar_ptr")?;

                let f0 = ctx.builder.build_struct_gep(ctx.jade_grammar_ty, struct_ptr, 0, "gm_f0").map_err(|e| e.to_string())?;
                ctx.builder.build_store(f0, pattern_ptr).map_err(|e| e.to_string())?;

                let f1 = ctx.builder.build_struct_gep(ctx.jade_grammar_ty, struct_ptr, 1, "gm_f1").map_err(|e| e.to_string())?;
                ctx.builder.build_store(f1, anchor_ptr).map_err(|e| e.to_string())?;

                let f2 = ctx.builder.build_struct_gep(ctx.jade_grammar_ty, struct_ptr, 2, "gm_f2").map_err(|e| e.to_string())?;
                ctx.builder.build_store(f2, stop_ptr).map_err(|e| e.to_string())?;

                return Ok(struct_ptr.into());
            }
        }
    }

    // ── Math stdlib dispatch: math.floor(...) etc. ────────────────────────────
    if let TExprKind::FieldAccess { object, field } = &callee.kind {
        if let TExprKind::Identifier(obj_name) = &object.kind {
            if obj_name == "math" {
                return emit_math_call(field, args, ctx);
            }
        }
    }

    // ── Stdlib module dispatch (table-driven) ────────────────────────────────────
    if let TExprKind::FieldAccess { object, field } = &callee.kind {
        if let TExprKind::Identifier(obj_name) = &object.kind {
            // Special case: json.stringify has complex inline codegen.
            if obj_name == "json" && field == "stringify" {
                ctx.uses_dicts = true;
                let arg = args.get(0).ok_or("json.stringify: missing arg")?;
                return emit_json_stringify(arg, ctx);
            }
            // Special case: llm.set_max_tokens is a no-op stub.
            if obj_name == "llm" && field == "set_max_tokens" {
                return Ok(ctx.context.i64_type().const_int(0, false).into());
            }
            if let Some(sig) = stdlib::module_sig(obj_name, field) {
                return emit_from_sig(&sig, args, ret_ty, ctx);
            }
        }
    }

    // ── Built-in functions ────────────────────────────────────────────────────
    if let TExprKind::Identifier(name) = &callee.kind {
        match name.as_str() {
            "print"  => return emit_print(args, ctx),
            "write"  => return emit_write(args, ctx),
            "stream" => return emit_stream(args, ctx),
            "input"  => return emit_input(args, ctx),
            "len"    => return emit_len(args, ctx),
            "join"   => return emit_join(args, ctx),
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

    // ── Struct method call: obj.method(args…) ────────────────────────────────
    if let TExprKind::FieldAccess { object, field } = &callee.kind {
        let type_name_opt = match &object.ty {
            JadeType::Struct(n) => Some(n.clone()),
            JadeType::Unknown => {
                // Inside method bodies `self` is Unknown-typed in TIR; resolve via scope.
                if let TExprKind::Identifier(var) = &object.kind {
                    ctx.lookup(var).and_then(|(_, ty)| {
                        if let JadeType::Struct(n) = ty { Some(n) } else { None }
                    })
                    // Also check module-level globals; imported-struct lets at
                    // the top level live there and ctx.lookup only walks scopes.
                    .or_else(|| {
                        ctx.module_globals.get(var.as_str())
                            .and_then(|(_, ty)| if let JadeType::Struct(n) = ty {
                                Some(n.clone())
                            } else { None })
                    })
                } else if let TExprKind::FieldAccess { object: inner_obj, field: inner_field } = &object.kind {
                    // Handle patterns like self.tools.grammar() — resolve the inner
                    // field's struct type from struct_field_types.
                    let inner_struct_name = match &inner_obj.ty {
                        JadeType::Struct(n) => Some(n.clone()),
                        JadeType::Unknown => {
                            if let TExprKind::Identifier(var) = &inner_obj.kind {
                                ctx.lookup(var).and_then(|(_, ty)| {
                                    if let JadeType::Struct(n) = ty { Some(n) } else { None }
                                })
                                .or_else(|| {
                                    ctx.module_globals.get(var.as_str())
                                        .and_then(|(_, ty)| if let JadeType::Struct(n) = ty {
                                            Some(n.clone())
                                        } else { None })
                                })
                            } else { None }
                        }
                        _ => None,
                    };
                    inner_struct_name.and_then(|sn| {
                        ctx.struct_field_types.get(&sn)
                            .and_then(|m| m.get(inner_field.as_str()))
                            .and_then(|ft| if let JadeType::Struct(n) = ft { Some(n.clone()) } else { None })
                    })
                } else { None }
            }
            _ => None,
        };
        if let Some(type_name) = type_name_opt {
            // Strip module prefix for struct method lookup (e.g. "tools.ToolGroup" → "ToolGroup").
            let bare_ty = type_name.rsplit_once('.').map(|(_, b)| b.to_string()).unwrap_or(type_name.clone());
            let mangled = format!("{bare_ty}__{field}");
            if let Some((fn_val, param_tys, fn_ret_ty)) = ctx.fn_info.get(mangled.as_str()).cloned() {
                // Emit the receiver (self) as the first argument.
                let self_val = emit_expr(object, ctx)?;
                let mut llvm_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(args.len() + 1);
                llvm_args.push(self_val.into());
                for (i, arg_expr) in args.iter().enumerate() {
                    let arg_val = emit_expr(arg_expr, ctx)?;
                    let param_ty = param_tys.get(i + 1).unwrap_or(&JadeType::Unknown);
                    let coerced = coerce(arg_val, &arg_expr.ty, param_ty, ctx)?;
                    llvm_args.push(coerced.into());
                }
                let call_site = ctx.builder
                    .build_call(fn_val, &llvm_args, "mcall")
                    .map_err(|e| e.to_string())?;
                return match fn_ret_ty {
                    JadeType::Nil => Ok(ctx.context.i64_type().const_int(0, false).into()),
                    _ => match call_site.as_any_value_enum() {
                        AnyValueEnum::IntValue(v)     => Ok(v.into()),
                        AnyValueEnum::FloatValue(v)   => Ok(v.into()),
                        AnyValueEnum::PointerValue(v) => Ok(v.into()),
                        _ => Ok(ctx.context.i64_type().const_int(0, false).into()),
                    },
                };
            }
        }
    }

    // ── Primitive method dispatch (table-driven) ─────────────────────────────────
    if let TExprKind::FieldAccess { object, field } = &callee.kind {
        if stdlib::is_builtin_method(field) {
            return emit_primitive_method(object, field, args, ret_ty, ctx);
        }
    }

    // ── Module-alias function call: alias.fn(…) where alias is imported with `as` ─
    // The LLVM codegen imports modules flat (bare names), so `context.build(…)` should
    // resolve to the bare function `build` registered in fn_info by declare_fns.
    if let TExprKind::FieldAccess { object, field } = &callee.kind {
        if let TExprKind::Identifier(alias) = &object.kind {
            // Only treat as module-alias call when the object isn't in scope.
            let is_module_alias = ctx.lookup(alias).is_none()
                && !ctx.module_globals.contains_key(alias.as_str());
            if is_module_alias {
                if let Some((fn_val, param_tys, fn_ret_ty)) = ctx.fn_info.get(field.as_str()).cloned() {
                    let mut llvm_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(args.len());
                    for (i, arg_expr) in args.iter().enumerate() {
                        let arg_val = emit_expr(arg_expr, ctx)?;
                        let param_ty = param_tys.get(i).unwrap_or(&JadeType::Unknown);
                        let coerced = coerce(arg_val, &arg_expr.ty, param_ty, ctx)?;
                        llvm_args.push(coerced.into());
                    }
                    let call_site = ctx.builder
                        .build_call(fn_val, &llvm_args, "alias_call")
                        .map_err(|e| e.to_string())?;
                    return match fn_ret_ty {
                        JadeType::Nil => Ok(ctx.context.i64_type().const_int(0, false).into()),
                        _ => match call_site.as_any_value_enum() {
                            AnyValueEnum::IntValue(v)     => Ok(v.into()),
                            AnyValueEnum::FloatValue(v)   => Ok(v.into()),
                            AnyValueEnum::PointerValue(v) => Ok(v.into()),
                            _ => Ok(ctx.context.i64_type().const_int(0, false).into()),
                        },
                    };
                }
            }
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

// ── Primitive method dispatch ─────────────────────────────────────────────────

fn emit_primitive_method<'ctx>(
    object: &TExpr,
    method: &str,
    args: &[TExpr],
    ret_ty: &JadeType,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let obj_val = emit_expr(object, ctx)?;
    let obj_ty  = &object.ty;
    let i64_ty  = ctx.context.i64_type();

    match method {
        // ── contains: dict path uses jade_dict_has; str path uses table ──────
        "contains" => {
            let obj_ptr = as_pointer(obj_val, ctx)?;
            let needle  = as_pointer(emit_expr(args.get(0).ok_or("contains: missing arg")?, ctx)?, ctx)?;
            match effective_ty(obj_ty) {
                JadeType::Dict => {
                    ctx.uses_dicts = true;
                    let res = ctx.call_rv(ctx.jade_dict_has_fn, &[obj_ptr.into(), needle.into()], "pm_has")?.into_int_value();
                    let b = ctx.builder.build_int_compare(IntPredicate::NE, res, ctx.context.i32_type().const_zero(), "has_b").map_err(|e| e.to_string())?;
                    Ok(b.into())
                }
                _ => {
                    let sig = stdlib::str_method_sig("contains").unwrap();
                    let fn_val = ctx.extern_fn(&sig);
                    let res = ctx.call_rv(fn_val, &[obj_ptr.into(), needle.into()], "pm_contains")?.into_int_value();
                    let b = ctx.builder.build_int_compare(IntPredicate::NE, res, ctx.context.i32_type().const_zero(), "cont_b").map_err(|e| e.to_string())?;
                    Ok(b.into())
                }
            }
        }

        // ── String primitive methods (table-driven) ───────────────────────────
        "split" | "trim" | "replace" => {
            let sig = stdlib::str_method_sig(method)
                .ok_or_else(|| format!("str_method_sig: unknown method '{method}'"))?;
            // Build args: receiver ptr first, then additional args per sig
            let obj_ptr = as_pointer(obj_val, ctx)?;
            let mut llvm_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(sig.args.len());
            llvm_args.push(obj_ptr.into());
            // sig.args[0] is always Ptr (receiver), so iterate sig.args[1..] vs args
            for (kind, arg_expr) in sig.args[1..].iter().zip(args.iter()) {
                let v = emit_expr(arg_expr, ctx)?;
                let coerced: BasicMetadataValueEnum = match kind {
                    stdlib::Arg::Ptr => as_pointer(v, ctx)?.into(),
                    stdlib::Arg::I64 => value_to_i64(v, &arg_expr.ty, ctx)?.into(),
                };
                llvm_args.push(coerced);
            }
            let fn_val = ctx.extern_fn(&sig);
            ctx.call_rv(fn_val, &llvm_args, "pm_str")
        }

        // ── Array primitive methods (table-driven) ────────────────────────────
        "push" => {
            let sig = stdlib::array_method_sig("push").unwrap();
            let obj_ptr = as_pointer(obj_val, ctx)?;
            let arg = args.get(0).ok_or("push: missing arg")?;
            let val = emit_expr(arg, ctx)?;
            let val_i64 = value_to_i64(val, &arg.ty, ctx)?;
            let fn_val = ctx.extern_fn(&sig);
            ctx.call_void(fn_val, &[obj_ptr.into(), val_i64.into()])?;
            Ok(i64_ty.const_int(0, false).into())
        }
        "pop" => {
            let sig = stdlib::array_method_sig("pop").unwrap();
            let obj_ptr = as_pointer(obj_val, ctx)?;
            let fn_val = ctx.extern_fn(&sig);
            let raw = ctx.call_rv(fn_val, &[obj_ptr.into()], "pm_pop")?.into_int_value();
            i64_to_value(raw, ret_ty, ctx)
        }

        // ── Dict-specific methods (keep pre-declared fns) ─────────────────────
        "has" => {
            ctx.uses_dicts = true;
            let obj_ptr = as_pointer(obj_val, ctx)?;
            let key = as_pointer(emit_expr(args.get(0).ok_or("has: missing arg")?, ctx)?, ctx)?;
            let res = ctx.call_rv(ctx.jade_dict_has_fn, &[obj_ptr.into(), key.into()], "pm_has")?.into_int_value();
            let b = ctx.builder.build_int_compare(IntPredicate::NE, res, ctx.context.i32_type().const_zero(), "pm_has_b").map_err(|e| e.to_string())?;
            Ok(b.into())
        }
        "get" => {
            ctx.uses_dicts = true;
            let obj_ptr = as_pointer(obj_val, ctx)?;
            let key = as_pointer(emit_expr(args.get(0).ok_or("get: missing arg")?, ctx)?, ctx)?;
            let raw = ctx.call_rv(ctx.jade_dict_get_fn, &[obj_ptr.into(), key.into()], "pm_get")?.into_int_value();
            i64_to_value(raw, ret_ty, ctx)
        }

        // ── sort: stub ────────────────────────────────────────────────────────
        "sort" => {
            // Not yet implemented in the LLVM backend; return the array unchanged.
            Ok(obj_val)
        }

        // ── len: type-dispatch special case ───────────────────────────────────
        "len" => {
            match effective_ty(obj_ty) {
                JadeType::Str => {
                    let obj_ptr = as_pointer(obj_val, ctx)?;
                    ctx.call_rv(ctx.strlen_fn, &[obj_ptr.into()], "pm_slen")
                }
                JadeType::Dict => {
                    ctx.uses_dicts = true;
                    let obj_ptr = as_pointer(obj_val, ctx)?;
                    ctx.call_rv(ctx.jade_dict_len_fn, &[obj_ptr.into()], "pm_dlen")
                }
                _ => {
                    // Array or Unknown — read .len field from jade.array header
                    let arr_ptr = as_pointer(obj_val, ctx)?;
                    let f1 = ctx.builder.build_struct_gep(ctx.array_ty, arr_ptr, 1, "pm_alen_f1").map_err(|e| e.to_string())?;
                    ctx.builder.build_load(i64_ty, f1, "pm_alen").map_err(|e| e.to_string())
                }
            }
        }

        _ => Err(format!("emit_primitive_method: unknown method '{method}'")),
    }
}

// ── json.stringify (inline codegen — not table-driven) ────────────────────────

/// Best-effort lookup of a field access's effective type. Used to recover
/// from `Unknown` types on `self.<field>` inside extend methods, where the
/// TIR didn't track per-field types. Returns None if the expression isn't
/// a field access we can resolve.
fn resolve_field_ty<'ctx>(expr: &TExpr, ctx: &CodegenCtx<'ctx>) -> Option<JadeType> {
    if !matches!(expr.ty, JadeType::Unknown) {
        return Some(expr.ty.clone());
    }
    let TExprKind::FieldAccess { object, field } = &expr.kind else { return None };
    let struct_name = match &object.ty {
        JadeType::Struct(n) => n.clone(),
        JadeType::Unknown => {
            if let TExprKind::Identifier(var) = &object.kind {
                match ctx.lookup(var).map(|(_, ty)| ty)? {
                    JadeType::Struct(n) => n,
                    _ => return None,
                }
            } else {
                return None;
            }
        }
        _ => return None,
    };
    ctx.struct_field_types.get(&struct_name)?
        .get(field).cloned()
}

fn emit_json_stringify<'ctx>(
    arg: &TExpr,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    if let TExprKind::Dict { entries } = &arg.kind {
        // Seeds must be tagged literals — emit_str_concat reads the trust
        // header of both operands; a bare `build_global_string_ptr` would
        // dereference offset -1 of .rodata (undefined behaviour, often a fault).
        let mut acc: BasicValueEnum<'ctx> = emit_tagged_literal("{", ctx)?;

        for (i, (key_expr, val_expr)) in entries.iter().enumerate() {
            let key_str = match &key_expr.kind {
                TExprKind::Str(s) => s.clone(),
                _ => return Err("json.stringify: non-string dict key".to_string()),
            };
            let prefix = if i == 0 {
                format!("\"{key_str}\": ")
            } else {
                format!(", \"{key_str}\": ")
            };
            let prefix_lit = emit_tagged_literal(&prefix, ctx)?;
            acc = emit_str_concat(acc, prefix_lit, ctx)?;

            // Resolve the value's effective type. For struct field accesses
            // the TIR type is often Unknown — look it up in the recorded
            // struct field types if possible so we don't mis-dispatch a Str
            // field as an array (which would cause jrt_json_arr_dicts to
            // dereference the string bytes as an array header).
            let resolved_ty = resolve_field_ty(val_expr, ctx).unwrap_or_else(|| val_expr.ty.clone());

            let val_str: BasicValueEnum<'ctx> = match resolved_ty {
                JadeType::Str => {
                    let v = as_pointer(emit_expr(val_expr, ctx)?, ctx)?;
                    let f = ctx.get_jrt_json_esc_str();
                    ctx.call_rv(f, &[v.into()], "json_esc")?
                }
                _ => {
                    let v = emit_expr(val_expr, ctx)?;
                    let as_i64 = value_to_i64(v, &val_expr.ty, ctx)?;
                    let f = ctx.get_jrt_json_arr_dicts();
                    ctx.call_rv(f, &[as_i64.into()], "json_arr")?
                }
            };
            acc = emit_str_concat(acc, val_str, ctx)?;
        }

        let close = emit_tagged_literal("}", ctx)?;
        return emit_str_concat(acc, close, ctx);
    }

    // Non-dict: treat as array of dicts (i64-typed pointer).
    let v = emit_expr(arg, ctx)?;
    let as_i64 = value_to_i64(v, &arg.ty, ctx)?;
    let f = ctx.get_jrt_json_arr_dicts();
    ctx.call_rv(f, &[as_i64.into()], "json_stringify_r")
}

// ── write() built-in ──────────────────────────────────────────────────────────

fn emit_write<'ctx>(
    args: &[TExpr],
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    for arg in args {
        let val = emit_expr(arg, ctx)?;
        emit_printf_value(val, &arg.ty, "", ctx)?;
    }
    Ok(ctx.context.i64_type().const_int(0, false).into())
}

// ── stream() built-in ─────────────────────────────────────────────────────────
// `stream(?p)` and `stream(?p, mute_on=[grammar])` lower to a single call into
// `jrt_prompt_stream_ex`, which streams tokens token-by-token to stdout (with
// prefix-aware muting) and returns the full collected text.
//
// If the first arg is anything other than `?p` (e.g. a precomputed string),
// fall back to printing the value — no daemon round-trip.

fn emit_stream<'ctx>(
    args: &[TExpr],
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    emit_stream_with_kwargs(args, &[], ctx)
}

fn emit_stream_with_kwargs<'ctx>(
    args: &[TExpr],
    kwargs: &[(String, TExpr)],
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    ctx.uses_prompts = true;
    ctx.uses_runtime = true;
    let first = args.get(0).ok_or("stream: missing arg")?;

    // Locate mute_on kwarg and extract the first grammar element if present.
    // Supported shape: `mute_on = [grammar_expr]` (single-element array literal).
    // Any other shape is treated as "no mute" — the response still streams.
    let mute_grammar: Option<&TExpr> = kwargs.iter()
        .find(|(k, _)| k == "mute_on")
        .and_then(|(_, e)| match &e.kind {
            TExprKind::Array { elements } => elements.first(),
            _ => None,
        });

    // Fast path: arg is `?p` — bypass jrt_prompt and use the streaming runtime.
    if let TExprKind::PromptDeref { expr: pexpr, output_type: None, grammar_expr: None } = &first.kind {
        let prompt_ptr = emit_expr(pexpr, ctx)?.into_pointer_value();
        let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
        let model_ptr = ctx.builder.build_global_string_ptr("", "stream_model_empty")
            .map_err(|e| e.to_string())?.as_pointer_value();

        let (pattern_ptr, anchor_ptr, stop_ptr, start_muted) = match mute_grammar {
            Some(gexpr) => {
                let struct_ptr = as_pointer(emit_expr(gexpr, ctx)?, ctx)?;
                let f0 = ctx.builder.build_struct_gep(ctx.jade_grammar_ty, struct_ptr, 0, "gm_p")
                    .map_err(|e| e.to_string())?;
                let pattern = ctx.builder.build_load(ptr_ty, f0, "gm_pattern")
                    .map_err(|e| e.to_string())?.into_pointer_value();

                let f1 = ctx.builder.build_struct_gep(ctx.jade_grammar_ty, struct_ptr, 1, "gm_a")
                    .map_err(|e| e.to_string())?;
                let anchor = ctx.builder.build_load(ptr_ty, f1, "gm_anchor")
                    .map_err(|e| e.to_string())?.into_pointer_value();

                let f2 = ctx.builder.build_struct_gep(ctx.jade_grammar_ty, struct_ptr, 2, "gm_s")
                    .map_err(|e| e.to_string())?;
                let stop = ctx.builder.build_load(ptr_ty, f2, "gm_stop")
                    .map_err(|e| e.to_string())?.into_pointer_value();

                // start_muted = (anchor == NULL). Runtime treats anchor=NULL +
                // start_muted=1 as "mute from token 0 until stop".
                let is_null = ctx.builder.build_is_null(anchor, "gm_anchor_null")
                    .map_err(|e| e.to_string())?;
                let start_muted = ctx.builder.build_int_z_extend(is_null, ctx.context.i32_type(), "gm_start_muted")
                    .map_err(|e| e.to_string())?;

                (pattern, anchor, stop, start_muted)
            }
            None => {
                let null = ptr_ty.const_null();
                let zero = ctx.context.i32_type().const_zero();
                (null, null, null, zero)
            }
        };

        let r = ctx.call_rv(
            ctx.jrt_prompt_stream_ex_fn,
            &[prompt_ptr.into(), model_ptr.into(),
              pattern_ptr.into(), anchor_ptr.into(), stop_ptr.into(),
              start_muted.into()],
            "stream_r",
        )?;
        let ptr = r.into_pointer_value();
        emit_null_check_and_exit(ptr, "jade: stream(?p) failed — jade-tree daemon unreachable or returned an error\n", ctx)?;
        return Ok(ptr.into());
    }

    // Fallback: arg is a precomputed value — print it.
    let result = emit_expr(first, ctx)?;
    emit_printf_value(result, &first.ty, "", ctx)?;
    Ok(result)
}

// ── input() built-in ──────────────────────────────────────────────────────────

fn emit_input<'ctx>(
    args: &[TExpr],
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    ctx.uses_prompts = true; // ensures jade_rt is linked
    let prompt_ptr = if let Some(arg) = args.first() {
        as_pointer(emit_expr(arg, ctx)?, ctx)?
    } else {
        ctx.context.ptr_type(AddressSpace::default()).const_null()
    };
    let f = ctx.get_jrt_readline();
    ctx.call_rv(f, &[prompt_ptr.into()], "input_r")
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
        JadeType::Str | JadeType::Grammar => {
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
        | JadeType::Future(_) | JadeType::Prompt | JadeType::Grammar => ctx.builder
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
        | JadeType::Future(_) | JadeType::Prompt | JadeType::Grammar => {
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
            | JadeType::Future(_) | JadeType::Prompt | JadeType::Grammar => declared.clone(),
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
