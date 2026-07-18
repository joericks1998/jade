use inkwell::{
    types::BasicMetadataTypeEnum,
    values::{
        AnyValue, AnyValueEnum, BasicMetadataValueEnum, BasicValueEnum,
        CallSiteValue, FunctionValue, IntValue, PointerValue,
    },
    AddressSpace, FloatPredicate, IntPredicate,
};

use jade::frontend::ast::{BinOpKind, UnaryOpKind};
use jade::compiler::tir::{JadeType, TExpr, TExprKind, TFStrPart};

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
            // nil / None / null are three spellings of the same literal (VM treats
            // them identically). All map to the tagged NIL word (0b111) so a nil
            // flowing through a type-erased channel reads back as nil, not int 0.
            if name == "nil" || name == "None" || name == "null" {
                return Ok(ctx.context.i64_type().const_int(JRT_NIL_TAGGED, false).into());
            }
            // A native function used as a first-class value (`let f = m.fn`): build
            // a jade_fn_t whose indirect-call path dispatches via jrt_native_call.
            if let Some((pkgid, fname)) = parse_native_ref(name) {
                return emit_native_fn_value(pkgid, fname, ctx);
            }
            // Local variable in scope (covers closures, params, let-bindings).
            // The slot holds the value in its *binding* type's representation;
            // convert to this reference's static type (they differ when a
            // variable is reassigned to a value of another type — the binding
            // stays its original type while later reads may be typed Unknown).
            if let Some((ptr, ty)) = ctx.lookup(name) {
                let llvm_ty = types::jade_to_llvm(&ty, ctx.context);
                let loaded = ctx.builder
                    .build_load(llvm_ty, ptr, name)
                    .map_err(|e| e.to_string())?;
                return convert_repr(loaded, &ty, &expr.ty, ctx);
            }
            // Module-level global (defined at top level, stored as LLVM global).
            if let Some((global, ty)) = ctx.module_globals.get(name.as_str()).cloned() {
                let llvm_ty = types::jade_to_llvm(&ty, ctx.context);
                let loaded = ctx.builder
                    .build_load(llvm_ty, global.as_pointer_value(), name)
                    .map_err(|e| e.to_string())?;
                return convert_repr(loaded, &ty, &expr.ty, ctx);
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
        Await { expr: fut_expr } => {
            let fut_val = emit_expr(fut_expr, ctx)?;
            let fut_ptr = as_pointer(fut_val, ctx)?;
            ctx.uses_async = true;
            // The async body returns a tagged word; unbox by this await
            // expression's static (result) type.
            let raw = ctx.call_rv(ctx.jade_await_fn, &[fut_ptr.into()], "await_res")?
                .into_int_value();
            i64_to_value(raw, &expr.ty, ctx)
        }

        // ── prompt <expr> ─────────────────────────────────────────────────────
        // A Prompt value is represented identically to a Str pointer at the
        // LLVM level — the type distinction is only semantic.
        PromptLiteral { body } => emit_expr(body, ctx),

        // ── ?prompt  /  ?prompt |> Type  /  ?prompt |> grammar_expr ─────────
        PromptDeref { expr: pexpr, output_type, grammar_expr } => {
            ctx.uses_prompts = true;

            // Load the prompt string pointer. as_pointer (not into_pointer_value)
            // so an Unknown-typed prompt expr (a tagged i64) is untagged rather
            // than panicking.
            let prompt_ptr = as_pointer(emit_expr(pexpr, ctx)?, ctx)?;

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

/// Emit `object[index]`. Dispatches on the object's static type: dict (raises
/// on a missing key), Unknown-with-string-index (treated as a dict), string
/// (returns a 1-char string), or array (bounds-unchecked GEP load). An
/// Unknown-typed index is untagged to a raw offset first.
fn emit_index<'ctx>(
    object: &TExpr,
    index: &TExpr,
    result_ty: &JadeType,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    // Dict indexing — key is always a string (char*). `d[k]` raises on a missing
    // key (jade_dict_get_checked), matching the VM ("key 'X' not found in dict").
    if matches!(&object.ty, JadeType::Dict) {
        ctx.uses_dicts = true;
        let dict_ptr = as_pointer(emit_expr(object, ctx)?, ctx)?;
        let key_ptr  = as_pointer(emit_expr(index, ctx)?, ctx)?;
        let dget = jade_dict_get_checked_fn(ctx);
        let raw = ctx.call_rv(
            dget,
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
        let dget = jade_dict_get_checked_fn(ctx);
        let raw = ctx.call_rv(
            dget,
            &[dict_ptr.into(), key_ptr.into()],
            "dict_idx_unk",
        )?.into_int_value();
        return i64_to_value(raw, result_ty, ctx);
    }

    // String indexing — `s[i]` returns a 1-char string, matching the VM's
    // `chars().nth(i)` (byte-indexed here: exact for ASCII, the case the eval
    // suite covers). Preserves the source string's trust tag.
    if matches!(&object.ty, JadeType::Str) {
        let i8_ty  = ctx.context.i8_type();
        let i64_ty = ctx.context.i64_type();
        let src = as_pointer(emit_expr(object, ctx)?, ctx)?;
        let idx_raw = emit_expr(index, ctx)?.into_int_value();
        let idx = if matches!(&index.ty, JadeType::Unknown) { untag_int_iv(idx_raw, ctx)? } else { idx_raw };
        let byte_slot = ctx.gep(i8_ty, src, &[idx], "str_idx_slot")?;
        let byte = ctx.builder
            .build_load(i8_ty, byte_slot, "str_idx_byte")
            .map_err(|e| e.to_string())?
            .into_int_value();
        let trust = emit_jrt_trust_of(src, ctx, "str_idx_trust")?;
        let out = emit_jrt_str_new(i64_ty.const_int(1, false), trust, ctx, "str_idx_out")?;
        ctx.builder.build_store(out, byte).map_err(|e| e.to_string())?;
        return Ok(out.into());
    }

    let i64_ty = ctx.context.i64_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());

    let arr = as_pointer(emit_expr(object, ctx)?, ctx)?;
    let idx_raw = emit_expr(index, ctx)?.into_int_value();
    // An Unknown index arrives tagged — untag to a raw integer for the GEP.
    let idx = if matches!(&index.ty, JadeType::Unknown) { untag_int_iv(idx_raw, ctx)? } else { idx_raw };

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

/// Emit a struct literal: malloc `(nfields+1)*8` bytes, plant the type-name
/// pointer in slot 0 (used by typed-catch dispatch and Unknown field access),
/// zero-init unsupplied fields, then box each provided field into its slot.
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

/// Emit `object.field`. For a struct receiver, loads the field's slot (slot 0 is
/// the type-name pointer; fields are 1-indexed) and unboxes by the access's
/// static type. For a dict receiver it's a key lookup; for an Unknown receiver
/// it resolves the struct type via scope or falls back to a runtime type-tag
/// dispatch (`emit_unknown_field_access`).
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
                        // Receiver is Unknown but might actually be a struct
                        // pointer (untyped fn param, untyped field, etc.).
                        // Emit a runtime type-tag dispatch using slot 0 before
                        // falling through to jade_dict_get — otherwise reading
                        // the struct as a dict misinterprets slot offsets and
                        // segfaults on garbage `cap`/`slots` values.
                        return emit_unknown_field_access(object, field, result_ty, ctx);
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

    let i64_ty = ctx.context.i64_type();
    let struct_ptr = as_pointer(emit_expr(object, ctx)?, ctx)?;
    // +1 to skip the type_name slot at slot 0
    let slot = ctx.gep(i64_ty, struct_ptr, &[i64_ty.const_int((idx as u64) + 1, false)], "fa_slot")?;
    let raw = ctx.builder
        .build_load(i64_ty, slot, "fa_raw")
        .map_err(|e| e.to_string())?
        .into_int_value();

    // Unbox by the field-access expression's static type so the result's
    // representation matches what callers expect: a concrete type yields a
    // native value, Unknown stays a tagged word. (The slot itself always holds
    // a tagged value, written via value_to_i64 at construction time.)
    i64_to_value(raw, result_ty, ctx)
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
                    // as_pointer masks the string tag bits; a raw int_to_ptr would
                    // leave the +5 STRING tag and read the trust byte (data[-1])
                    // and chars from the wrong offset.
                    let p = as_pointer(val, ctx)?;
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

/// Get-or-declare `int jrt_snprintf_float(char*, size_t, double)` — the runtime
/// helper that formats a float the way the VM displays it (shortest round-trip,
/// trailing ".0" on integer-valued floats). Callers must set `uses_runtime`.
fn jrt_snprintf_float_fn<'ctx>(ctx: &mut CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("jrt_snprintf_float") {
        return f;
    }
    let i32t = ctx.context.i32_type();
    let i64t = ctx.context.i64_type();
    let f64t = ctx.context.f64_type();
    let ptr = ctx.context.ptr_type(AddressSpace::default());
    let ty = i32t.fn_type(&[ptr.into(), i64t.into(), f64t.into()], false);
    ctx.module.add_function("jrt_snprintf_float", ty, None)
}

/// Format a value into a buffer via snprintf, dispatching on its static type
/// (Unknown routes to the tag-dispatching `jrt_snprintf_any`). Used by f-string
/// interpolation; returns the number of chars written.
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

    // Check Unknown BEFORE effective_ty (which coerces Unknown → Int and would
    // mis-format string pointers as decimal addresses). For genuinely Unknown
    // values, dispatch at runtime via jrt_snprintf_any (heap-pointer heuristic:
    // >= 4GB → %s, else %lld).
    if matches!(ty, JadeType::Unknown) {
        // FloatValue carries its own type info — format VM-style via the
        // runtime helper rather than reinterpreting bits as an integer.
        if let BasicValueEnum::FloatValue(_) = val {
            ctx.uses_runtime = true;
            let f = jrt_snprintf_float_fn(ctx);
            let call = ctx.builder
                .build_call(f, &[write_ptr.into(), remaining.into(), val.into()], "snp_flt")
                .map_err(|e| e.to_string())?;
            return extract_i32_from_call(call, ctx);
        }
        let any_fn = match ctx.module.get_function("jrt_snprintf_any") {
            Some(f) => f,
            None => {
                let i64t = ctx.context.i64_type();
                let i32t = ctx.context.i32_type();
                let ptr  = ctx.context.ptr_type(AddressSpace::default());
                let ty   = i32t.fn_type(&[ptr.into(), i64t.into(), i64t.into()], false);
                ctx.module.add_function("jrt_snprintf_any", ty, None)
            }
        };
        let v_i64 = match val {
            BasicValueEnum::IntValue(iv) => iv,
            BasicValueEnum::PointerValue(pv) => ctx.builder
                .build_ptr_to_int(pv, ctx.context.i64_type(), "snp_p2i")
                .map_err(|e| e.to_string())?,
            _ => return Err("fstring: unsupported value kind for Unknown branch".into()),
        };
        let call = ctx.builder
            .build_call(any_fn, &[write_ptr.into(), remaining.into(), v_i64.into()], "snp_any")
            .map_err(|e| e.to_string())?;
        return extract_i32_from_call(call, ctx);
    }

    let call = match effective_ty(ty) {
        JadeType::Int => {
            let fmt = mk("%lld", ctx)?;
            ctx.builder.build_call(ctx.snprintf_fn, &[write_ptr.into(), remaining.into(), fmt.into(), val.into()], "snp_int")
                .map_err(|e| e.to_string())?
        }
        JadeType::Float => {
            ctx.uses_runtime = true;
            let f = jrt_snprintf_float_fn(ctx);
            ctx.builder.build_call(f, &[write_ptr.into(), remaining.into(), val.into()], "snp_flt")
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
            // NULL char* in a Str slot is nil routed through a typed slot; render
            // "nil" (a real Jade string is never NULL) instead of libc's "(null)".
            // A Str carried through an i64 slot (dict/array element, untyped param,
            // fn return) arrives as a *tagged* IntValue — as_pointer masks the tag
            // bits back off (a raw int_to_ptr would leave the +1 PTR tag, reading
            // the trust byte and first char from the wrong offset).
            let p = as_pointer(val, ctx)?;
            let nil_lit = mk("nil", ctx)?;
            let is_null = ctx.builder.build_is_null(p, "fstr_str_is_nil").map_err(|e| e.to_string())?;
            let sel = ctx.builder.build_select(is_null, nil_lit, p, "fstr_str_or_nil").map_err(|e| e.to_string())?;
            let fmt = mk("%s", ctx)?;
            ctx.builder.build_call(ctx.snprintf_fn, &[write_ptr.into(), remaining.into(), fmt.into(), sel.into()], "snp_str")
                .map_err(|e| e.to_string())?
        }
        JadeType::Nil => {
            // nil interpolates as the literal "nil" (matches the VM's
            // value_to_display), not the i64 0 it's represented as.
            let fmt = mk("nil", ctx)?;
            ctx.builder.build_call(ctx.snprintf_fn, &[write_ptr.into(), remaining.into(), fmt.into()], "snp_nil")
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

    // [7 pad][trust][bytes...][nul]. An 8-byte header keeps the returned data
    // pointer 8-aligned (low 3 bits free for the value tag), with the trust byte
    // at data[-1] (header index 7). The global is 8-aligned so global+8 is too.
    let bytes = s.as_bytes();
    let mut data: Vec<u8> = Vec::with_capacity(bytes.len() + 9);
    data.extend_from_slice(&[0u8; 7]);
    data.push(JRT_TRUSTED_LIT as u8);
    data.extend_from_slice(bytes);
    data.push(0);

    let arr_ty  = i8_ty.array_type(data.len() as u32);
    let const_arr = ctx.context.const_string(&data, false);
    let global = ctx.module.add_global(arr_ty, None, "str_lit_t");
    global.set_initializer(&const_arr);
    global.set_linkage(inkwell::module::Linkage::Internal);
    global.set_constant(true);
    global.set_alignment(8);

    let zero  = i32_ty.const_zero();
    let eight = i32_ty.const_int(8, false);
    let data_ptr = unsafe {
        ctx.builder
            .build_in_bounds_gep(arr_ty, global.as_pointer_value(), &[zero, eight], "lit_data")
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

// ── Type-conversion constructors: str / int / float / bool ────────────────────
// Mirror the VM's `vm_type_call` + `value_to_display` (jadelang vm.rs), the
// language's source of truth. The call's TIR result type is specialized to
// Int/Float/Bool/Str in type_infer, so `print` and binop codegen format the
// result correctly without inspecting these helpers.

/// `str(x)` → TRUSTED tagged `char*` equal to `value_to_display(x)`.
fn emit_convert_str<'ctx>(
    args: &[TExpr],
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let arg = args.get(0).ok_or("str(): missing argument")?;
    let val = emit_expr(arg, ctx)?;
    match val {
        // Already a string (or any heap value carried as a pointer): identity,
        // matching the VM's `str(str)` = the string itself.
        BasicValueEnum::PointerValue(_) => Ok(val),
        // Float → VM-style formatting into a TRUSTED tagged buffer.
        BasicValueEnum::FloatValue(fv) => {
            let i64_ty = ctx.context.i64_type();
            let i8_ty  = ctx.context.i8_type();
            let cap = i64_ty.const_int(64, false);
            let buf = emit_jrt_str_new(cap, i8_ty.const_int(JRT_TRUSTED_LIT, false), ctx, "str_fbuf")?;
            ctx.uses_runtime = true;
            let f = jrt_snprintf_float_fn(ctx);
            ctx.builder
                .build_call(f, &[buf.into(), cap.into(), fv.into()], "str_flt")
                .map_err(|e| e.to_string())?;
            Ok(buf.into())
        }
        BasicValueEnum::IntValue(iv) => {
            match actual_ty(&arg.ty, &val) {
                // str(nil) → "nil" TRUSTED literal (VM-faithful).
                JadeType::Nil => emit_tagged_literal("nil", ctx),
                // bool → "true"/"false" TRUSTED literal (no allocation needed).
                JadeType::Bool => {
                    let t = emit_tagged_literal("true", ctx)?;
                    let f = emit_tagged_literal("false", ctx)?;
                    let sel = ctx.builder
                        .build_select(iv, t, f, "str_bsel")
                        .map_err(|e| e.to_string())?;
                    Ok(sel)
                }
                // int → decimal TRUSTED string.
                _ => Ok(emit_int_to_str(iv, ctx)?.into()),
            }
        }
        _ => Err("str(): unsupported argument value".to_string()),
    }
}

/// `int(x)` → i64.
fn emit_convert_int<'ctx>(
    args: &[TExpr],
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let arg = args.get(0).ok_or("int(): missing argument")?;
    let val = emit_expr(arg, ctx)?;
    let i64_ty = ctx.context.i64_type();
    match val {
        // string → atoll (matches VM `s.trim().parse::<i64>()` for valid ints).
        BasicValueEnum::PointerValue(p) => ctx.call_rv(ctx.atoll_fn, &[p.into()], "int_of_str"),
        // float → truncate toward zero (VM `f as i64`).
        BasicValueEnum::FloatValue(fv) => {
            let i = ctx.builder
                .build_float_to_signed_int(fv, i64_ty, "flt2int")
                .map_err(|e| e.to_string())?;
            Ok(i.into())
        }
        // int already, or bool (i1) → zero-extend to i64 (true→1 / false→0).
        BasicValueEnum::IntValue(iv) => {
            if iv.get_type().get_bit_width() < 64 {
                let z = ctx.builder
                    .build_int_z_extend(iv, i64_ty, "to_i64")
                    .map_err(|e| e.to_string())?;
                Ok(z.into())
            } else {
                Ok(iv.into())
            }
        }
        _ => Err("int(): unsupported argument value".to_string()),
    }
}

/// `float(x)` → f64.
fn emit_convert_float<'ctx>(
    args: &[TExpr],
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let arg = args.get(0).ok_or("float(): missing argument")?;
    let val = emit_expr(arg, ctx)?;
    let f64_ty = ctx.context.f64_type();
    let i64_ty = ctx.context.i64_type();
    match val {
        BasicValueEnum::FloatValue(_) => Ok(val),
        // string → strtod(s, NULL).
        BasicValueEnum::PointerValue(p) => ctx.call_rv(
            ctx.strtod_fn,
            &[p.into(), ctx.context.ptr_type(AddressSpace::default()).const_null().into()],
            "float_of_str",
        ),
        // int/bool → signed int→float. Zero-extend a bool (i1) first, else a
        // signed i1→float would yield -1.0 for true.
        BasicValueEnum::IntValue(iv) => {
            let iv = if iv.get_type().get_bit_width() < 64 {
                ctx.builder.build_int_z_extend(iv, i64_ty, "to_i64").map_err(|e| e.to_string())?
            } else {
                iv
            };
            let f = ctx.builder
                .build_signed_int_to_float(iv, f64_ty, "int2flt")
                .map_err(|e| e.to_string())?;
            Ok(f.into())
        }
        _ => Err("float(): unsupported argument value".to_string()),
    }
}

/// `bool(x)` → i1. Matches the VM: string → (lower=="false" || "")→false else
/// true; int→(x!=0); float→(x!=0, NaN→true); bool→identity.
fn emit_convert_bool<'ctx>(
    args: &[TExpr],
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let arg = args.get(0).ok_or("bool(): missing argument")?;
    let val = emit_expr(arg, ctx)?;
    // nil → false (constant).
    if matches!(&arg.ty, JadeType::Nil) {
        return Ok(ctx.context.bool_type().const_int(0, false).into());
    }
    // Unknown → tagged value of runtime-variable kind: dispatch via jrt_to_bool.
    if matches!(&arg.ty, JadeType::Unknown) {
        ctx.uses_runtime = true;
        let i64_ty = ctx.context.i64_type();
        let f = if let Some(f) = ctx.module.get_function("jrt_to_bool") { f } else {
            let ty = ctx.context.i32_type().fn_type(&[i64_ty.into()], false);
            ctx.module.add_function("jrt_to_bool", ty, None)
        };
        let r = ctx.call_rv(f, &[val.into_int_value().into()], "to_bool")?.into_int_value();
        let b = ctx.builder
            .build_int_compare(IntPredicate::NE, r, ctx.context.i32_type().const_zero(), "to_bool_i1")
            .map_err(|e| e.to_string())?;
        return Ok(b.into());
    }
    match val {
        // string → jrt_bool_of_str (case-insensitive, VM-exact); narrow to i1.
        BasicValueEnum::PointerValue(p) => {
            ctx.uses_runtime = true;
            let f = get_jrt_bool_of_str(ctx);
            let r = ctx.call_rv(f, &[p.into()], "bool_of_str")?.into_int_value();
            let b = ctx.builder
                .build_int_compare(IntPredicate::NE, r, ctx.context.i32_type().const_zero(), "bool_str_i1")
                .map_err(|e| e.to_string())?;
            Ok(b.into())
        }
        // float → unordered not-equal 0.0 (NaN→true, matching VM `f != 0.0`).
        BasicValueEnum::FloatValue(fv) => {
            let b = ctx.builder
                .build_float_compare(FloatPredicate::UNE, fv, ctx.context.f64_type().const_zero(), "bool_flt")
                .map_err(|e| e.to_string())?;
            Ok(b.into())
        }
        // bool already → identity; int → (x != 0).
        BasicValueEnum::IntValue(iv) => {
            if iv.get_type().get_bit_width() == 1 {
                Ok(iv.into())
            } else {
                let b = ctx.builder
                    .build_int_compare(IntPredicate::NE, iv, iv.get_type().const_zero(), "bool_int")
                    .map_err(|e| e.to_string())?;
                Ok(b.into())
            }
        }
        _ => Err("bool(): unsupported argument value".to_string()),
    }
}

/// Lazily declare `int32_t jrt_bool_of_str(const char*)` (defined in the C runtime).
fn get_jrt_bool_of_str<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("jrt_bool_of_str") {
        return f;
    }
    let ptr = ctx.context.ptr_type(AddressSpace::default());
    let ty = ctx.context.i32_type().fn_type(&[ptr.into()], false);
    ctx.module.add_function("jrt_bool_of_str", ty, None)
}

/// Lazily declare `int64_t jade_dict_get_checked(void*, const char*)` — like
/// jade_dict_get but raises a catchable error on a missing key (VM semantics).
fn jade_dict_get_checked_fn<'ctx>(ctx: &mut CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    ctx.uses_exceptions = true;
    if let Some(f) = ctx.module.get_function("jade_dict_get_checked") { return f; }
    let i64t = ctx.context.i64_type();
    let ptr = ctx.context.ptr_type(AddressSpace::default());
    let ty = i64t.fn_type(&[ptr.into(), ptr.into()], false);
    ctx.module.add_function("jade_dict_get_checked", ty, None)
}

/// Lazily declare `void* jade_dict_copy(void*)` (defined in the C runtime).
fn get_jade_dict_copy<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("jade_dict_copy") {
        return f;
    }
    let ptr = ctx.context.ptr_type(AddressSpace::default());
    let ty = ptr.fn_type(&[ptr.into()], false);
    ctx.module.add_function("jade_dict_copy", ty, None)
}

/// Copy a dict value (`jade_dict_copy`). Used to give `let d2 = d` / `d2 = d`
/// the VM's dict value semantics (binding an existing dict copies it).
pub(crate) fn emit_dict_copy<'ctx>(
    val: BasicValueEnum<'ctx>,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    ctx.uses_dicts = true;
    let f = get_jade_dict_copy(ctx);
    let src = as_pointer(val, ctx)?;
    ctx.call_rv(f, &[src.into()], "dict_copy")
}

// ── String concatenation ──────────────────────────────────────────────────────

fn emit_str_concat<'ctx>(
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let i8_ty  = ctx.context.i8_type();
    // Operands may arrive as i64 when carrying string pointers through
    // Unknown-typed channels (function params, struct fields, etc.).
    let lp = as_pointer(lhs, ctx)?;
    let rp = as_pointer(rhs, ctx)?;

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
            // The value's kind is only known at runtime. jrt_len_unknown reads it
            // as a raw 64-bit word and dispatches: a STRING-tagged value → strlen;
            // everything else (array/dict tagged, or a raw untagged heap pointer
            // from a stdlib Ret::Ptr like fs.list_dir / env.cwd) → array-header
            // .len at offset 8. This keeps the long-standing offset-8 behavior for
            // non-strings while giving a correct length for tagged strings such as
            // llm.tool_grammar(). Pass the word untagged: an IntValue is already a
            // word; a raw pointer is reinterpreted via ptrtoint (NOT value_to_i64,
            // which would wrongly STRING-tag a raw array pointer).
            ctx.uses_runtime = true;
            let i64_ty = ctx.context.i64_type();
            let word = match val {
                BasicValueEnum::IntValue(v) => v,
                BasicValueEnum::PointerValue(p) => ctx.builder
                    .build_ptr_to_int(p, i64_ty, "len_word")
                    .map_err(|e| e.to_string())?,
                other => return Err(format!("len(): unexpected Unknown value {other:?}")),
            };
            let f = ctx.module.get_function("jrt_len_unknown").unwrap_or_else(|| {
                ctx.module.add_function(
                    "jrt_len_unknown", i64_ty.fn_type(&[i64_ty.into()], false), None)
            });
            ctx.call_rv(f, &[word.into()], "len_unknown")
        }
        _ => Err(format!("len() not supported for type {:?}", arg.ty)),
    }
}

// ── join() built-in ───────────────────────────────────────────────────────────

fn emit_join<'ctx>(
    args: &[jade::compiler::tir::TExpr],
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
/// Runtime type-tag dispatch for `expr.field` where `expr` is statically
/// Unknown-typed but at runtime might be a struct pointer. Reads slot 0 (the
/// type-name string planted by `emit_struct_literal`), strcmp's against every
/// known struct type that declares this field, and on a hit loads the value
/// from the field's known slot index. Falls back to `jade_dict_get` if nothing
/// matches — preserves the existing behaviour for plain dicts.
fn emit_unknown_field_access<'ctx>(
    object: &TExpr,
    field: &str,
    result_ty: &JadeType,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let i64_ty = ctx.context.i64_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let fn_val = ctx.builder.get_insert_block().unwrap().get_parent().unwrap();

    // Emit the receiver ONCE as a tagged word, so we can tag-check it at runtime.
    let recv = value_to_i64(emit_expr(object, ctx)?, &object.ty, ctx)?;

    let result_slot = ctx.build_entry_alloca(i64_ty.into(), "fa_dispatch_r")?;
    ctx.builder.build_store(result_slot, i64_ty.const_int(JRT_NIL_TAGGED, false))
        .map_err(|e| e.to_string())?;
    let done_bb = ctx.context.append_basic_block(fn_val, "fa_dispatch_done");

    // ── Tag guard ─────────────────────────────────────────────────────────────
    // A STRING (or any non-heap immediate) has no struct/dict header — reading it
    // as one (slot0 type-name / jade_dict_get) interprets its bytes as a pointer
    // and SEGFAULTS. A caught runtime error is a string, so `.message` must be
    // safe: it yields the message itself (matching the VM's RuntimeError.message);
    // any other field on a non-struct/dict value is nil.
    let tag = ctx.builder.build_and(recv, i64_ty.const_int(7, false), "fa_tag")
        .map_err(|e| e.to_string())?;
    let is_str = ctx.builder
        .build_int_compare(IntPredicate::EQ, tag, i64_ty.const_int(5, false), "fa_isstr")
        .map_err(|e| e.to_string())?;
    let str_bb  = ctx.context.append_basic_block(fn_val, "fa_str");
    let heap_bb = ctx.context.append_basic_block(fn_val, "fa_heap");
    ctx.builder.build_conditional_branch(is_str, str_bb, heap_bb)
        .map_err(|e| e.to_string())?;

    // String receiver: `.message` → the string itself; any other field → nil.
    ctx.builder.position_at_end(str_bb);
    if field == "message" {
        ctx.builder.build_store(result_slot, recv).map_err(|e| e.to_string())?;
    }
    ctx.builder.build_unconditional_branch(done_bb).map_err(|e| e.to_string())?;

    // Heap receiver (struct or dict): untag and dispatch.
    ctx.builder.position_at_end(heap_bb);
    let recv_untagged = ctx.builder
        .build_and(recv, i64_ty.const_int(!7u64, false), "fa_untag")
        .map_err(|e| e.to_string())?;
    let recv_ptr = ctx.builder
        .build_int_to_ptr(recv_untagged, ptr_ty, "fa_recv_ptr")
        .map_err(|e| e.to_string())?;

    // Candidates: struct types that declare a field named `field`.
    let candidates: Vec<(String, usize, JadeType)> = ctx.struct_field_order.iter()
        .filter_map(|(ty_name, field_names)| {
            field_names.iter().position(|n| n == field).map(|idx| {
                let field_ty = ctx.struct_field_types
                    .get(ty_name)
                    .and_then(|m| m.get(field))
                    .cloned()
                    .unwrap_or(JadeType::Unknown);
                (ty_name.clone(), idx, field_ty)
            })
        })
        .collect();

    // No struct declares this field → treat the heap value as a dict.
    if candidates.is_empty() {
        ctx.uses_dicts = true;
        let key_lit = ctx.builder
            .build_global_string_ptr(field, "fa_unk_key")
            .map_err(|e| e.to_string())?
            .as_pointer_value();
        let raw = ctx.call_rv(
            ctx.jade_dict_get_fn,
            &[recv_ptr.into(), key_lit.into()],
            "fa_unk_dg",
        )?.into_int_value();
        ctx.builder.build_store(result_slot, raw).map_err(|e| e.to_string())?;
        ctx.builder.build_unconditional_branch(done_bb).map_err(|e| e.to_string())?;
        ctx.builder.position_at_end(done_bb);
        let raw_out = ctx.builder
            .build_load(i64_ty, result_slot, "fa_r_load")
            .map_err(|e| e.to_string())?
            .into_int_value();
        return i64_to_value(raw_out, result_ty, ctx);
    }

    // Load slot 0 → type-name pointer (struct convention).
    let slot0 = ctx.gep(i64_ty, recv_ptr, &[i64_ty.const_int(0, false)], "fa_slot0")?;
    let name_i64 = ctx.builder
        .build_load(i64_ty, slot0, "fa_name_i64")
        .map_err(|e| e.to_string())?
        .into_int_value();
    let name_ptr = ctx.builder
        .build_int_to_ptr(name_i64, ptr_ty, "fa_name_ptr")
        .map_err(|e| e.to_string())?;

    for (ty_name, idx, _field_ty) in &candidates {
        let ty_lit = ctx.builder
            .build_global_string_ptr(ty_name, "fa_ty_lit")
            .map_err(|e| e.to_string())?
            .as_pointer_value();
        let cmp = ctx.call_rv(ctx.strcmp_fn, &[name_ptr.into(), ty_lit.into()], "fa_scmp")?
            .into_int_value();
        let zero = ctx.context.i32_type().const_zero();
        let eq = ctx.builder
            .build_int_compare(IntPredicate::EQ, cmp, zero, "fa_eq")
            .map_err(|e| e.to_string())?;
        let hit_bb = ctx.context.append_basic_block(fn_val, "fa_hit");
        let next_bb = ctx.context.append_basic_block(fn_val, "fa_next");
        ctx.builder.build_conditional_branch(eq, hit_bb, next_bb).map_err(|e| e.to_string())?;

        ctx.builder.position_at_end(hit_bb);
        // Load slot (idx+1) — slot 0 is the type-name reserved slot.
        let slot = ctx.gep(i64_ty, recv_ptr, &[i64_ty.const_int((*idx as u64) + 1, false)], "fa_slot")?;
        let val = ctx.builder
            .build_load(i64_ty, slot, "fa_val")
            .map_err(|e| e.to_string())?
            .into_int_value();
        ctx.builder.build_store(result_slot, val).map_err(|e| e.to_string())?;
        ctx.builder.build_unconditional_branch(done_bb).map_err(|e| e.to_string())?;

        ctx.builder.position_at_end(next_bb);
    }
    // Tag matched no known struct → treat as plain dict.
    ctx.uses_dicts = true;
    let key_lit = ctx.builder
        .build_global_string_ptr(field, "fa_unk_key_fb")
        .map_err(|e| e.to_string())?
        .as_pointer_value();
    let raw = ctx.call_rv(
        ctx.jade_dict_get_fn,
        &[recv_ptr.into(), key_lit.into()],
        "fa_unk_dg_fb",
    )?.into_int_value();
    ctx.builder.build_store(result_slot, raw).map_err(|e| e.to_string())?;
    ctx.builder.build_unconditional_branch(done_bb).map_err(|e| e.to_string())?;

    ctx.builder.position_at_end(done_bb);
    let raw_out = ctx.builder
        .build_load(i64_ty, result_slot, "fa_r_load")
        .map_err(|e| e.to_string())?
        .into_int_value();
    i64_to_value(raw_out, result_ty, ctx)
}

pub(crate) fn emit_fn_as_value<'ctx>(
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
    body: &[jade::compiler::tir::TStmt],
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

    let lhs_v = emit_expr(left, ctx)?;
    let lhs = emit_cond_i1(lhs_v, ctx)?;
    let rhs_bb   = ctx.context.append_basic_block(fn_val, "and_rhs");
    let merge_bb = ctx.context.append_basic_block(fn_val, "and_merge");

    ctx.builder
        .build_conditional_branch(lhs, rhs_bb, merge_bb)
        .map_err(|e| e.to_string())?;
    let lhs_end = ctx.builder.get_insert_block()
        .ok_or_else(|| "&&: builder lost insert block after lhs branch".to_string())?;

    ctx.builder.position_at_end(rhs_bb);
    let rhs_v = emit_expr(right, ctx)?;
    let rhs = emit_cond_i1(rhs_v, ctx)?;
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

    let lhs_v = emit_expr(left, ctx)?;
    let lhs = emit_cond_i1(lhs_v, ctx)?;
    let rhs_bb   = ctx.context.append_basic_block(fn_val, "or_rhs");
    let merge_bb = ctx.context.append_basic_block(fn_val, "or_merge");

    ctx.builder
        .build_conditional_branch(lhs, merge_bb, rhs_bb)
        .map_err(|e| e.to_string())?;
    let lhs_end = ctx.builder.get_insert_block()
        .ok_or_else(|| "||: builder lost insert block after lhs branch".to_string())?;

    ctx.builder.position_at_end(rhs_bb);
    let rhs_v = emit_expr(right, ctx)?;
    let rhs = emit_cond_i1(rhs_v, ctx)?;
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

/// Emit a binary operation. If either operand is statically Unknown (and the op
/// isn't In/NotIn/And/Or) it routes to `emit_binop_any` (runtime tag dispatch).
/// Otherwise it matches on the concrete operand types: native int/float
/// arithmetic & comparison, string concat/compare, bitwise, mixed int/float
/// promotion, bool/nil comparisons, and membership.
fn emit_binop<'ctx>(
    op: &BinOpKind,
    lty: &JadeType,
    rty: &JadeType,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    use BinOpKind::*;

    // Tagged dynamic dispatch: when either operand is statically Unknown, the
    // value carries its kind at runtime — route to the jrt_*_any helpers rather
    // than collapsing Unknown→Int and doing raw arithmetic on a tagged word.
    // (In/NotIn fall through to emit_contains; And/Or are short-circuited above.)
    if matches!(lty, JadeType::Unknown) || matches!(rty, JadeType::Unknown) {
        if !matches!(op, In | NotIn | And | Or) {
            return emit_binop_any(op, lty, lhs, rty, rhs, ctx);
        }
    }

    // When one operand has a known non-int type (Str/Array/Dict/Struct) and the
    // other is Unknown, prefer the known side rather than collapsing Unknown to
    // Int.  Otherwise `untyped_param != "" ` would hit the (Int,Ne,Str) arm and
    // return a compile-time `true`, and `untyped_param + "..."` would stringify
    // the pointer bits via int_to_str — both produce the symptoms seen when
    // function params receive strings from the caller but the param itself is
    // typed Unknown.
    fn prefer_known(a: &JadeType, b: &JadeType, op: &BinOpKind) -> JadeType {
        match (a, b) {
            (JadeType::Unknown, JadeType::Str)
            | (JadeType::Unknown, JadeType::Array(_))
            | (JadeType::Unknown, JadeType::Dict)
            | (JadeType::Unknown, JadeType::Struct(_)) => b.clone(),
            // For Unknown==Unknown / Unknown!=Unknown keep Unknown so the
            // runtime-helper arm catches both cases (handles strings + ints
            // without statically guessing). Other ops fall back to Int.
            (JadeType::Unknown, JadeType::Unknown) if matches!(op, Eq | Ne) => JadeType::Unknown,
            _ => effective_ty(a),
        }
    }
    let elty = prefer_known(lty, rty, op);
    let erty = prefer_known(rty, lty, op);

    // Integer divide/modulo by zero: LLVM sdiv/srem by 0 is UB and traps
    // (SIGILL/SIGSEGV) on arm64, whereas the VM raises a catchable runtime
    // error. Emit a zero-divisor check that raises via jade_exc_throw_typed so
    // AOT matches the VM and try/catch can recover. Done before the immutable
    // `b` borrow below, while `ctx` is still mutably available.
    if matches!(op, Div | Mod)
        && matches!(elty, JadeType::Int)
        && matches!(erty, JadeType::Int)
    {
        let fn_val = ctx.builder.get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or("division outside function")?;
        let i64_ty = ctx.context.i64_type();
        let i8_ty  = ctx.context.i8_type();
        let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
        let is_zero = ctx.builder
            .build_int_compare(IntPredicate::EQ, rhs.into_int_value(), i64_ty.const_zero(), "divzero")
            .map_err(|e| e.to_string())?;
        let throw_bb = ctx.context.append_basic_block(fn_val, "divzero_throw");
        let ok_bb    = ctx.context.append_basic_block(fn_val, "divzero_ok");
        ctx.builder.build_conditional_branch(is_zero, throw_bb, ok_bb)
            .map_err(|e| e.to_string())?;
        ctx.builder.position_at_end(throw_bb);
        // Throw a TRUSTED, *tagged* "division by zero" string so a catch binding
        // (Unknown) reads it back as a string. emit_tagged_literal gives an
        // 8-aligned data pointer; tag it as a pointer value for the exc slot.
        let msg_ptr = as_pointer(emit_tagged_literal("division by zero", ctx)?, ctx)?;
        let str_i64 = tag_ptr_iv(msg_ptr, ctx)?;
        let _ = i8_ty;
        ctx.call_void(ctx.jade_exc_throw_typed_fn,
            &[str_i64.into(), ptr_ty.const_null().into()])?;
        ctx.builder.build_unreachable().map_err(|e| e.to_string())?;
        ctx.builder.position_at_end(ok_bb);
    }

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
        // frem matches the VM's `%` on floats (fmod: result takes the dividend's
        // sign), e.g. 5.5 % 2.0 = 1.5.
        (JadeType::Float, Mod, JadeType::Float) =>
            Ok(b.build_float_rem(lhs.into_float_value(), rhs.into_float_value(), "frem").map_err(|e| e.to_string())?.into()),

        // ── Mixed int/float: promote int → float ──────────────────────────────
        (JadeType::Int, Add | Sub | Mul | Div | Mod, JadeType::Float) => {
            let lf = b.build_signed_int_to_float(lhs.into_int_value(), ctx.context.f64_type(), "itof").map_err(|e| e.to_string())?;
            emit_binop(op, &JadeType::Float, &JadeType::Float, lf.into(), rhs, ctx)
        }
        (JadeType::Float, Add | Sub | Mul | Div | Mod, JadeType::Int) => {
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

        // (Unknown × Unknown — and any Unknown operand — is intercepted at the
        // top of emit_binop and routed to emit_binop_any, so no Unknown arm is
        // needed here.)

        // ── Nil comparisons ──────────────────────────────────────────────────
        // The non-nil operand is nil iff it is a null heap pointer (a real Jade
        // string/struct is never null — a function returning nil yields NULL,
        // e.g. path.ext) or the tagged-nil word. Compare accordingly.
        (_, Eq, JadeType::Nil) | (JadeType::Nil, Eq, _) | (_, Ne, JadeType::Nil) | (JadeType::Nil, Ne, _) => {
            let eq = matches!(op, Eq);
            let other = if matches!(lty, JadeType::Nil) { rhs } else { lhs };
            let other_ty = if matches!(lty, JadeType::Nil) { &rty } else { &lty };
            let pred = if eq { IntPredicate::EQ } else { IntPredicate::NE };
            match other {
                BasicValueEnum::PointerValue(p) => {
                    if eq {
                        Ok(ctx.builder.build_is_null(p, "is_nil").map_err(|e| e.to_string())?.into())
                    } else {
                        Ok(ctx.builder.build_is_not_null(p, "is_not_nil").map_err(|e| e.to_string())?.into())
                    }
                }
                _ => {
                    let ov = value_to_i64(other, other_ty, ctx)?;
                    let nil = ctx.context.i64_type().const_int(JRT_NIL_TAGGED, false);
                    Ok(ctx.builder.build_int_compare(pred, ov, nil, "nil_cmp").map_err(|e| e.to_string())?.into())
                }
            }
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
        // Unknown operand is a tagged word — negate at runtime by kind.
        UnaryOpKind::Neg if matches!(ty, JadeType::Unknown) => {
            ctx.uses_runtime = true;
            let f = jrt_neg_any_fn(ctx);
            let r = ctx.call_rv(f, &[val.into_int_value().into()], "neg_any")?.into_int_value();
            Ok(r.into())
        }
        UnaryOpKind::Neg => match effective_ty(ty) {
            JadeType::Int =>
                Ok(ctx.builder.build_int_neg(val.into_int_value(), "ineg").map_err(|e| e.to_string())?.into()),
            JadeType::Float =>
                Ok(ctx.builder.build_float_neg(val.into_float_value(), "fneg").map_err(|e| e.to_string())?.into()),
            _ => Err(format!("cannot negate {:?} in LLVM backend", ty)),
        },
        // Logical not → i1, regardless of the operand's width. A bool carried
        // through an i64 slot (untyped param, dict/method result) arrives as an
        // i64; `build_not` would give a (wrong, wider) bitwise complement that
        // can't merge with an i1 in an `||`/`&&` phi. `val == 0` is the truth
        // inversion and is always i1.
        UnaryOpKind::Not => {
            // For an Unknown operand the bool is tagged; untag it to an i1 first.
            // For a native i1 (or i64 bool-in-slot) `== 0` is the inversion.
            let iv = if matches!(ty, JadeType::Unknown) {
                untag_bool_iv(val.into_int_value(), ctx)?
            } else {
                val.into_int_value()
            };
            let zero = iv.get_type().const_zero();
            Ok(ctx.builder.build_int_compare(IntPredicate::EQ, iv, zero, "lnot")
                .map_err(|e| e.to_string())?.into())
        }
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

/// Get-or-declare `int64_t jrt_ipow(int64_t, int64_t)`. Callers set `uses_runtime`.
fn jrt_ipow_fn<'ctx>(ctx: &mut CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("jrt_ipow") {
        return f;
    }
    let i64t = ctx.context.i64_type();
    let ty = i64t.fn_type(&[i64t.into(), i64t.into()], false);
    ctx.module.add_function("jrt_ipow", ty, None)
}

/// True only when the argument is *statically* an Int. Unknown (e.g. an
/// untyped fn param) stays on the float path — the VM dispatches on the runtime
/// value, which codegen can't see, so promoting is the safe default there.
fn is_static_int(e: &TExpr) -> bool {
    matches!(e.ty, JadeType::Int)
}

/// Fold a compile-time integer constant: a literal, or a negated literal
/// (`-1` parses as `Neg(Integer(1))`). Used to resolve the sign of a `pow`
/// exponent statically so the Int-vs-Float result type can match the VM.
fn const_int(e: &TExpr) -> Option<i64> {
    match &e.kind {
        TExprKind::Integer(n) => Some(*n),
        TExprKind::UnaryOp { op: UnaryOpKind::Neg, operand } => const_int(operand).map(|v| -v),
        _ => None,
    }
}

fn math_promote<'ctx>(arg: &TExpr, ctx: &mut CodegenCtx<'ctx>) -> Result<BasicValueEnum<'ctx>, String> {
    let f64_ty = ctx.context.f64_type();
    let val = emit_expr(arg, ctx)?;
    // An Unknown arg is a tagged value of runtime-variable kind — convert it to
    // a double at runtime (raises a catchable TypeError for non-numeric values).
    if matches!(&arg.ty, JadeType::Unknown) {
        ctx.uses_runtime = true;
        let to_d = if let Some(f) = ctx.module.get_function("jrt_any_to_double") { f } else {
            let ty = f64_ty.fn_type(&[ctx.context.i64_type().into()], false);
            ctx.module.add_function("jrt_any_to_double", ty, None)
        };
        return Ok(ctx.call_rv(to_d, &[val.into_int_value().into()], "any2d")?.into_float_value().into());
    }
    match val {
        BasicValueEnum::FloatValue(_) => Ok(val),
        BasicValueEnum::IntValue(i) => Ok(ctx.builder
            .build_signed_int_to_float(i, f64_ty, "math_itof")
            .map_err(|e| e.to_string())?.into()),
        BasicValueEnum::PointerValue(p) => {
            // A heap value (string/dict/array) is not numeric — route it through
            // jrt_any_to_double, which raises a catchable TypeError like the VM
            // (the old code bit-reinterpreted the *address* as an IEEE-754 double,
            // producing garbage; floats are heap-boxed now, never raw bits).
            ctx.uses_runtime = true;
            let to_d = if let Some(f) = ctx.module.get_function("jrt_any_to_double") { f } else {
                let ty = f64_ty.fn_type(&[ctx.context.i64_type().into()], false);
                ctx.module.add_function("jrt_any_to_double", ty, None)
            };
            let tagged = tag_ptr_iv(p, ctx)?;
            Ok(ctx.call_rv(to_d, &[tagged.into()], "any2d")?.into_float_value().into())
        }
        _ => Err("math: unexpected argument type".to_string()),
    }
}

/// Box a native math result (int/float/ptr) into a tagged value. Used when the
/// math call's static type is Unknown (the usual case for `math.*`), so the
/// result reads back by its runtime kind.
fn box_native<'ctx>(v: BasicValueEnum<'ctx>, ctx: &mut CodegenCtx<'ctx>) -> Result<BasicValueEnum<'ctx>, String> {
    match v {
        BasicValueEnum::IntValue(iv) => Ok(tag_int_iv(iv, ctx)?.into()),
        BasicValueEnum::FloatValue(fv) => {
            let bf = jrt_box_float_fn(ctx);
            Ok(ctx.call_rv(bf, &[fv.into()], "boxf")?.into_int_value().into())
        }
        BasicValueEnum::PointerValue(pv) => Ok(tag_ptr_iv(pv, ctx)?.into()),
        other => Ok(other),
    }
}

/// `math.pow` with a *runtime*-valued integer exponent: the result is Int when
/// exp >= 0 and Float when exp < 0 (matching the VM), so dispatch at runtime and
/// return a tagged value. Returns None when this case doesn't apply (the static
/// native path handles constant exponents and float operands).
fn emit_pow_maybe_runtime<'ctx>(
    args: &[TExpr],
    ret_ty: &JadeType,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let a_e = args.first().ok_or("math.pow: missing arg 0")?;
    let b_e = args.get(1).ok_or("math.pow: missing arg 1")?;
    if !(is_static_int(a_e) && is_static_int(b_e) && const_int(b_e).is_none()
         && matches!(ret_ty, JadeType::Unknown)) {
        return Ok(None);
    }
    ctx.uses_runtime = true;
    let i64_ty = ctx.context.i64_type();
    let f64_ty = ctx.context.f64_type();
    let base = emit_expr(a_e, ctx)?.into_int_value();
    let exp  = emit_expr(b_e, ctx)?.into_int_value();
    let fn_val = ctx.builder.get_insert_block().and_then(|bb| bb.get_parent())
        .ok_or("math.pow outside function")?;
    let nonneg = ctx.builder
        .build_int_compare(IntPredicate::SGE, exp, i64_ty.const_zero(), "pow_nonneg")
        .map_err(|e| e.to_string())?;
    let int_bb = ctx.context.append_basic_block(fn_val, "pow_int");
    let flt_bb = ctx.context.append_basic_block(fn_val, "pow_flt");
    let merge_bb = ctx.context.append_basic_block(fn_val, "pow_merge");
    ctx.builder.build_conditional_branch(nonneg, int_bb, flt_bb).map_err(|e| e.to_string())?;
    // exp >= 0 → integer power, tagged as Int.
    ctx.builder.position_at_end(int_bb);
    let ipow = jrt_ipow_fn(ctx);
    let ir = ctx.call_rv(ipow, &[base.into(), exp.into()], "ipow")?.into_int_value();
    let it = tag_int_iv(ir, ctx)?;
    ctx.builder.build_unconditional_branch(merge_bb).map_err(|e| e.to_string())?;
    let int_end = ctx.builder.get_insert_block().unwrap();
    // exp < 0 → floating power, boxed as Float.
    ctx.builder.position_at_end(flt_bb);
    let bf2 = ctx.builder.build_signed_int_to_float(base, f64_ty, "pow_bf").map_err(|e| e.to_string())?;
    let ef2 = ctx.builder.build_signed_int_to_float(exp, f64_ty, "pow_ef").map_err(|e| e.to_string())?;
    let powf = math_libc_fn("pow", 2, ctx);
    let fr = ctx.call_rv(powf, &[bf2.into(), ef2.into()], "powf")?.into_float_value();
    let boxf = jrt_box_float_fn(ctx);
    let ft = ctx.call_rv(boxf, &[fr.into()], "boxf")?.into_int_value();
    ctx.builder.build_unconditional_branch(merge_bb).map_err(|e| e.to_string())?;
    let flt_end = ctx.builder.get_insert_block().unwrap();
    // Merge.
    ctx.builder.position_at_end(merge_bb);
    let phi = ctx.builder.build_phi(i64_ty, "pow_r").map_err(|e| e.to_string())?;
    phi.add_incoming(&[(&it, int_end), (&ft, flt_end)]);
    Ok(Some(phi.as_basic_value()))
}

fn emit_math_call<'ctx>(
    method: &str,
    args: &[TExpr],
    ret_ty: &JadeType,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    // Runtime-valued pow exponent: tagged int-or-float result.
    if method == "pow" {
        if let Some(r) = emit_pow_maybe_runtime(args, ret_ty, ctx)? {
            return Ok(r);
        }
    }
    let native = emit_math_call_native(method, args, ctx)?;
    // math.* results are statically Unknown — box the native value so it reads
    // back by its runtime kind. (If a concrete type was inferred, keep native.)
    if matches!(ret_ty, JadeType::Unknown) { box_native(native, ctx) } else { Ok(native) }
}

fn emit_math_call_native<'ctx>(
    method: &str,
    args: &[TExpr],
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    match method {
        "floor" | "ceil" => {
            // The VM returns an Int from floor/ceil (floor(3.7) == 3, not 3.0),
            // so truncate the libc double result back to i64.
            let libc = if method == "floor" { "floor" } else { "ceil" };
            let f = math_libc_fn(libc, 1, ctx);
            let a = math_promote(args.first().ok_or_else(|| format!("math.{method}: missing arg"))?, ctx)?;
            let r = ctx.call_rv(f, &[a.into()], "math_r")?;
            let iv = ctx.builder
                .build_float_to_signed_int(r.into_float_value(), ctx.context.i64_type(), "math_f2i")
                .map_err(|e| e.to_string())?;
            Ok(iv.into())
        }
        "sqrt" => {
            let f = math_libc_fn("sqrt", 1, ctx);
            let a = math_promote(args.first().ok_or_else(|| "math.sqrt: missing arg".to_string())?, ctx)?;
            ctx.call_rv(f, &[a.into()], "math_r")
        }
        "abs" => {
            let arg = args.first().ok_or_else(|| "math.abs: missing arg".to_string())?;
            if is_static_int(arg) {
                // VM: abs(int) -> Int. select(x < 0, -x, x).
                let v = emit_expr(arg, ctx)?.into_int_value();
                let zero = ctx.context.i64_type().const_zero();
                let neg = ctx.builder.build_int_neg(v, "iabs_neg").map_err(|e| e.to_string())?;
                let lt = ctx.builder.build_int_compare(IntPredicate::SLT, v, zero, "iabs_lt").map_err(|e| e.to_string())?;
                Ok(ctx.builder.build_select(lt, neg, v, "iabs").map_err(|e| e.to_string())?.into())
            } else {
                let f = math_libc_fn("fabs", 1, ctx);
                let a = math_promote(arg, ctx)?;
                ctx.call_rv(f, &[a.into()], "math_r")
            }
        }
        "min" | "max" => {
            let a_e = args.first().ok_or_else(|| format!("math.{method}: missing arg 0"))?;
            let b_e = args.get(1).ok_or_else(|| format!("math.{method}: missing arg 1"))?;
            if is_static_int(a_e) && is_static_int(b_e) {
                // VM: min/max(int, int) -> Int. select on a signed compare.
                let a = emit_expr(a_e, ctx)?.into_int_value();
                let b = emit_expr(b_e, ctx)?.into_int_value();
                let pred = if method == "min" { IntPredicate::SLT } else { IntPredicate::SGT };
                let cmp = ctx.builder.build_int_compare(pred, a, b, "imm_cmp").map_err(|e| e.to_string())?;
                Ok(ctx.builder.build_select(cmp, a, b, "imm").map_err(|e| e.to_string())?.into())
            } else {
                let libc = if method == "min" { "fmin" } else { "fmax" };
                let f = math_libc_fn(libc, 2, ctx);
                let a = math_promote(a_e, ctx)?;
                let b = math_promote(b_e, ctx)?;
                ctx.call_rv(f, &[a.into(), b.into()], "math_r")
            }
        }
        "pow" => {
            let a_e = args.first().ok_or_else(|| "math.pow: missing arg 0".to_string())?;
            let b_e = args.get(1).ok_or_else(|| "math.pow: missing arg 1".to_string())?;
            // VM: pow(int, int) is Int for exp >= 0 and Float for exp < 0. When
            // the exponent is a compile-time constant we pick the matching type
            // exactly; a *runtime*-valued negative exponent still can't be a
            // static type and yields 0 via jrt_ipow (the remaining edge — to be
            // closed by the runtime-tagged-number refactor).
            let neg_const_exp = const_int(b_e).is_some_and(|n| n < 0);
            if is_static_int(a_e) && is_static_int(b_e) && !neg_const_exp {
                ctx.uses_runtime = true;
                let a = emit_expr(a_e, ctx)?.into_int_value();
                let b = emit_expr(b_e, ctx)?.into_int_value();
                let f = jrt_ipow_fn(ctx);
                ctx.call_rv(f, &[a.into(), b.into()], "ipow")
            } else {
                let f = math_libc_fn("pow", 2, ctx);
                let a = math_promote(a_e, ctx)?;
                let b = math_promote(b_e, ctx)?;
                ctx.call_rv(f, &[a.into(), b.into()], "math_r")
            }
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

    // Direct named call we know about: resolve positional + keyword args into
    // full parameter slots, filling any omitted slot with that param's default
    // (a literal). This covers both `f(kw=…)` and `f()` with default params.
    // Taken whenever there are kwargs OR fewer positional args than params; the
    // common all-positional call keeps the fast path below.
    if let TExprKind::Identifier(fn_name) = &callee.kind {
        if let Some(param_names) = ctx.fn_param_names.get(fn_name.as_str()).cloned() {
            let n = param_names.len();
            if !kwargs.is_empty() || args.len() < n {
                let defaults = ctx.fn_param_defaults.get(fn_name.as_str()).cloned().unwrap_or_default();
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
                // Build a flat ordered arg list: provided value, else the param's
                // default expr, else a zero literal (defensive — a missing
                // required arg is an arity error the frontend should have caught).
                let ordered: Vec<TExpr> = slots.into_iter().enumerate().map(|(i, s)| {
                    s.or_else(|| defaults.get(i).cloned().flatten())
                     .unwrap_or_else(|| jade::compiler::tir::TExpr {
                        kind: TExprKind::Integer(0),
                        ty: JadeType::Int,
                        span: callee.span.clone(),
                     })
                }).collect();
                return emit_call(callee, &ordered, ret_ty, ctx);
            }
        }
    }

    if kwargs.is_empty() {
        return emit_call(callee, args, ret_ty, ctx);
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
            // Raw native i64: untag an Unknown word to a native int; a concrete
            // Int SSA value is already native.
            stdlib::Arg::I64Raw => match &arg_expr.ty {
                JadeType::Unknown => i64_to_value(v.into_int_value(), &JadeType::Int, ctx)?.into(),
                _ => v.into_int_value().into(),
            },
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

/// Emit a call expression. Tries each dispatch form in order (the ordering is
/// load-bearing): `Grammar.new`, `math.*`, the stdlib Sig table, builtins
/// (print/json/etc.), a known top-level function, a struct method, a primitive
/// (str/array/dict) method, an Unknown-receiver runtime type-tag dispatch, a
/// module-alias call, and finally an indirect (first-class fn value) call.
/// Each call result is converted to `ret_ty`'s representation via convert_repr.
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
                return emit_math_call(field, args, ret_ty, ctx);
            }
        }
    }

    // ── Stdlib module dispatch (table-driven) ────────────────────────────────────
    if let TExprKind::FieldAccess { object, field } = &callee.kind {
        if let TExprKind::Identifier(obj_name) = &object.kind {
            // Special case: json.stringify has complex inline codegen.
            if obj_name == "json" && (field == "stringify" || field == "stringify_pretty") {
                ctx.uses_dicts = true;
                let arg = args.get(0).ok_or("json.stringify: missing arg")?;
                return emit_json_stringify(arg, field == "stringify_pretty", ctx);
            }
            // Special case: llm.* functions that return dicts/arrays/strings or
            // mutate sticky session state. These must yield an Unknown-typed
            // value (tagged word or native string ptr), so they're lowered here
            // rather than through the fixed-shape Sig table. count_tokens and
            // total_tokens (plain i64 returns) fall through to the table below.
            if obj_name == "llm" {
                match field.as_str() {
                    // No-op stub (max-tokens is fixed at codegen time in AOT).
                    "set_max_tokens" => {
                        return Ok(ctx.context.i64_type().const_int(0, false).into());
                    }
                    // Sticky session flag → jrt_llm_keep_anchors(bool). Returns nil.
                    "keep_anchors" => {
                        let arg = emit_expr(
                            args.get(0).ok_or("llm.keep_anchors: missing bool arg")?, ctx)?;
                        let b_i1 = match arg {
                            BasicValueEnum::IntValue(v) if v.get_type().get_bit_width() == 1 => v,
                            // An Unknown-typed bool arrives as a tagged word; untag it.
                            BasicValueEnum::IntValue(v) => untag_bool_iv(v, ctx)?,
                            other => return Err(format!(
                                "llm.keep_anchors: expected bool, got {other:?}")),
                        };
                        let i32v = ctx.builder
                            .build_int_z_extend(b_i1, ctx.context.i32_type(), "ka_i32")
                            .map_err(|e| e.to_string())?;
                        let f = ctx.get_jrt_llm_keep_anchors();
                        ctx.call_void(f, &[i32v.into()])?;
                        return Ok(ctx.context.i64_type().const_int(JRT_NIL_TAGGED, false).into());
                    }
                    // Active model name (reads $JADE_MODEL) — same as __model__.
                    // The call yields a char*; tag it as a string so the Unknown
                    // result is a proper tagged word — len()/print dispatch on the
                    // tag (a raw ptr would be mis-read as an array by len()).
                    "model" => {
                        let p = ctx.call_rv(ctx.jrt_get_model_fn, &[], "llm_model")?
                            .into_pointer_value();
                        return Ok(tag_str_iv(p, ctx)?.into());
                    }
                    // Canonical tool-call GBNF body (a TRUSTED string).
                    "tool_grammar" => {
                        let f = ctx.get_jrt_tool_grammar();
                        let p = ctx.call_rv(f, &[], "llm_tool_grammar")?.into_pointer_value();
                        return Ok(tag_str_iv(p, ctx)?.into());
                    }
                    // {name, args} dict (or nil) — tagged i64 from the runtime.
                    "find_tool_call" => {
                        ctx.uses_dicts = true;
                        let arg = as_pointer(emit_expr(
                            args.get(0).ok_or("llm.find_tool_call: missing str arg")?, ctx)?, ctx)?;
                        let f = ctx.get_jrt_llm_find_tool_call();
                        return ctx.call_rv(f, &[arg.into()], "llm_find_tc");
                    }
                    // Array of {name, args} dicts — tagged i64 from the runtime.
                    "find_tool_calls" => {
                        ctx.uses_dicts = true;
                        let arg = as_pointer(emit_expr(
                            args.get(0).ok_or("llm.find_tool_calls: missing str arg")?, ctx)?, ctx)?;
                        let f = ctx.get_jrt_llm_find_tool_calls();
                        return ctx.call_rv(f, &[arg.into()], "llm_find_tcs");
                    }
                    // Active model's profile dict (or nil).
                    "profile" => {
                        ctx.uses_dicts = true;
                        let f = ctx.get_jrt_llm_profile();
                        return ctx.call_rv(f, &[], "llm_profile");
                    }
                    // Daemon health snapshot dict (talks to the inference socket).
                    "health" => {
                        ctx.uses_dicts = true;
                        ctx.uses_prompts = true;
                        let f = ctx.get_jrt_llm_health();
                        return ctx.call_rv(f, &[], "llm_health");
                    }
                    // count_tokens / total_tokens → fall through to the Sig table.
                    _ => {}
                }
            }
            // Special case: path.join is variadic (VM accepts >= 2 segments).
            // The Sig table only models a fixed 2-arg call, so fold the args
            // left-to-right through the 2-arg jrt_path_join, matching the VM's
            // sequential PathBuf::push.
            if obj_name == "path" && field == "join" {
                ctx.uses_runtime = true;
                if args.is_empty() {
                    return emit_tagged_literal("", ctx);
                }
                let mut acc = as_pointer(emit_expr(&args[0], ctx)?, ctx)?;
                if args.len() == 1 {
                    return Ok(acc.into());
                }
                let sig = stdlib::module_sig("path", "join")
                    .ok_or("path.join: missing sig")?;
                let fn_val = ctx.extern_fn(&sig);
                for a in &args[1..] {
                    let p = as_pointer(emit_expr(a, ctx)?, ctx)?;
                    acc = ctx.call_rv(fn_val, &[acc.into(), p.into()], "path_join")?
                        .into_pointer_value();
                }
                return Ok(acc.into());
            }
            // Special case: time.sleep takes a float arg (Sig models only
            // Ptr/I64), so coerce to f64 and call jrt_time_sleep(double).
            if obj_name == "time" && field == "sleep" {
                ctx.uses_runtime = true;
                let secs = math_promote(
                    args.get(0).ok_or("time.sleep: missing arg")?, ctx)?
                    .into_float_value();
                let f = if let Some(f) = ctx.module.get_function("jrt_time_sleep") { f } else {
                    let void_ty = ctx.context.void_type();
                    let f64_ty = ctx.context.f64_type();
                    ctx.module.add_function(
                        "jrt_time_sleep", void_ty.fn_type(&[f64_ty.into()], false), None)
                };
                ctx.call_void(f, &[secs.into()])?;
                return Ok(ctx.context.i64_type().const_int(JRT_NIL_TAGGED, false).into());
            }
            // Special case: env.get returns str-or-nil and is Unknown-typed, so
            // the char*/NULL result is tagged as a string or folded to a tagged
            // nil — the Sig table can't express that conditional tagging.
            if obj_name == "env" && field == "get" {
                ctx.uses_runtime = true;
                let arg = as_pointer(emit_expr(
                    args.get(0).ok_or("env.get: missing arg")?, ctx)?, ctx)?;
                let f = if let Some(f) = ctx.module.get_function("jrt_env_get") { f } else {
                    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
                    ctx.module.add_function(
                        "jrt_env_get", ptr_ty.fn_type(&[ptr_ty.into()], false), None)
                };
                let p = ctx.call_rv(f, &[arg.into()], "env_get")?.into_pointer_value();
                let tagged = tag_str_iv(p, ctx)?;
                let isnull = ctx.builder.build_is_null(p, "env_get_null")
                    .map_err(|e| e.to_string())?;
                let nil = ctx.context.i64_type().const_int(JRT_NIL_TAGGED, false);
                let sel = ctx.builder.build_select(isnull, nil, tagged, "env_get_sel")
                    .map_err(|e| e.to_string())?;
                return Ok(sel.into());
            }
            // Special case: fs.read takes an optional `trust` bool — trust=true
            // skips the tainted-path refusal and returns TRUSTED content (so an
            // LLM/program can read a file and use it at sinks). The Sig table is
            // fixed-arity, so it's lowered here. (Temporary operator override;
            // kernel file-trust is the durable design.)
            if obj_name == "fs" && field == "read" {
                ctx.uses_runtime = true;
                let path = as_pointer(
                    emit_expr(args.get(0).ok_or("fs.read: missing path")?, ctx)?, ctx)?;
                let i32_ty = ctx.context.i32_type();
                let trust = if let Some(a) = args.get(1) {
                    let v = emit_expr(a, ctx)?;
                    let b = match v {
                        BasicValueEnum::IntValue(iv) if iv.get_type().get_bit_width() == 1 => iv,
                        // An Unknown-typed bool arrives as a tagged word; untag it.
                        BasicValueEnum::IntValue(iv) => untag_bool_iv(iv, ctx)?,
                        other => return Err(format!("fs.read: trust must be a bool, got {other:?}")),
                    };
                    ctx.builder.build_int_z_extend(b, i32_ty, "fs_trust")
                        .map_err(|e| e.to_string())?
                } else {
                    i32_ty.const_zero()
                };
                let f = if let Some(f) = ctx.module.get_function("jrt_fs_read") { f } else {
                    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
                    ctx.module.add_function(
                        "jrt_fs_read", ptr_ty.fn_type(&[ptr_ty.into(), i32_ty.into()], false), None)
                };
                let r = ctx.call_rv(f, &[path.into(), trust.into()], "fs_read")?;
                return convert_repr(r, &JadeType::Str, ret_ty, ctx);
            }
            // Special case: random.float returns a double (Sig has no float ret).
            if obj_name == "random" && field == "float" {
                ctx.uses_runtime = true;
                let f = if let Some(f) = ctx.module.get_function("jrt_random_float") { f } else {
                    let f64_ty = ctx.context.f64_type();
                    ctx.module.add_function("jrt_random_float", f64_ty.fn_type(&[], false), None)
                };
                let r = ctx.call_rv(f, &[], "rand_float")?;
                return convert_repr(r, &JadeType::Float, ret_ty, ctx);
            }
            // Special case: http.* has optional headers (and body for post/put),
            // which the fixed-arity Sig table can't model — pass a null dict ptr
            // when omitted. Every verb returns a { status, body } dict.
            if obj_name == "http"
                && matches!(field.as_str(), "get" | "post" | "put" | "delete" | "head")
            {
                ctx.uses_dicts = true;
                ctx.uses_runtime = true;
                let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
                let nullp = ptr_ty.const_null();
                let url = as_pointer(
                    emit_expr(args.get(0).ok_or("http: missing url")?, ctx)?, ctx)?;
                let cname = match field.as_str() {
                    "get" => "jrt_http_get", "post" => "jrt_http_post",
                    "put" => "jrt_http_put", "delete" => "jrt_http_delete",
                    _ => "jrt_http_head",
                };
                let has_body = matches!(field.as_str(), "post" | "put");
                let nparams = if has_body { 3 } else { 2 };
                let f = if let Some(f) = ctx.module.get_function(cname) { f } else {
                    let params = vec![ptr_ty.into(); nparams];
                    ctx.module.add_function(cname, ptr_ty.fn_type(&params, false), None)
                };
                let r = if has_body {
                    let body = as_pointer(
                        emit_expr(args.get(1).ok_or("http: missing body")?, ctx)?, ctx)?;
                    let headers = if args.len() > 2 {
                        as_pointer(emit_expr(&args[2], ctx)?, ctx)?
                    } else { nullp };
                    ctx.call_rv(f, &[url.into(), body.into(), headers.into()], "http")?
                } else {
                    let headers = if args.len() > 1 {
                        as_pointer(emit_expr(&args[1], ctx)?, ctx)?
                    } else { nullp };
                    ctx.call_rv(f, &[url.into(), headers.into()], "http")?
                };
                return convert_repr(r, &JadeType::Dict, ret_ty, ctx);
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
            "str"    => return emit_convert_str(args, ctx),
            "int"    => return emit_convert_int(args, ctx),
            "float"  => return emit_convert_float(args, ctx),
            "bool"   => return emit_convert_bool(args, ctx),
            _ => {}
        }
    }

    // ── Native (C-ABI) package call: __native$<id>$<fn>(args…) ────────────────
    // The renamer (imports.rs) collapses `m.fn` / `from lib use fn` references to
    // this canonical identifier. Dispatch directly through jrt_native_call.
    if let TExprKind::Identifier(name) = &callee.kind {
        if let Some((pkgid, fname)) = parse_native_ref(name) {
            return emit_native_call(pkgid, fname, args, ret_ty, ctx);
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
            // The result is in the callee's declared return-type representation;
            // convert to this call expression's static type (they differ when the
            // call site is Unknown-typed but the callee returns a concrete type).
            let result: BasicValueEnum<'ctx> = match call_site.as_any_value_enum() {
                AnyValueEnum::IntValue(v)     => v.into(),
                AnyValueEnum::FloatValue(v)   => v.into(),
                AnyValueEnum::PointerValue(v) => v.into(),
                AnyValueEnum::StructValue(v)  => v.into(),
                AnyValueEnum::ArrayValue(v)   => v.into(),
                AnyValueEnum::VectorValue(v)  => v.into(),
                // void return (Nil-typed callee): synthesize a tagged nil.
                _ => ctx.context.i64_type().const_int(JRT_NIL_TAGGED, false).into(),
            };
            // A void/Nil callee already yields the tagged-nil sentinel above.
            if matches!(fn_ret_ty, JadeType::Nil) {
                return i64_to_value(ctx.context.i64_type().const_int(JRT_NIL_TAGGED, false), ret_ty, ctx);
            }
            return convert_repr(result, &fn_ret_ty, ret_ty, ctx);
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
                // Emit the receiver (self) as the first argument. The method's
                // self param is a raw struct pointer, but emit_expr may yield a
                // tagged i64 when the receiver's static type is Unknown (e.g. an
                // imported struct whose binding is Struct but whose reference is
                // typed Unknown) — as_pointer untags it back to a raw pointer.
                let self_val = emit_expr(object, ctx)?;
                let self_ptr = as_pointer(self_val, ctx)?;
                let mut llvm_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(param_tys.len());
                llvm_args.push(self_ptr.into());
                // Fill each non-self param (index 1..) with the provided arg, else
                // that param's default literal — `c.bump()` must supply `by`'s
                // default rather than calling with too few args.
                let defaults = ctx.fn_param_defaults.get(mangled.as_str()).cloned().unwrap_or_default();
                let unknown = JadeType::Unknown;
                for pi in 1..param_tys.len() {
                    let param_ty = param_tys.get(pi).unwrap_or(&unknown);
                    let coerced = if let Some(arg_expr) = args.get(pi - 1) {
                        let v = emit_expr(arg_expr, ctx)?;
                        coerce(v, &arg_expr.ty, param_ty, ctx)?
                    } else if let Some(Some(def_expr)) = defaults.get(pi) {
                        let v = emit_expr(def_expr, ctx)?;
                        coerce(v, &def_expr.ty, param_ty, ctx)?
                    } else {
                        // Missing required arg (frontend arity check should have
                        // caught this); defensive zero keeps the call well-formed.
                        ctx.context.i64_type().const_int(0, false).into()
                    };
                    llvm_args.push(coerced.into());
                }
                let call_site = ctx.builder
                    .build_call(fn_val, &llvm_args, "mcall")
                    .map_err(|e| e.to_string())?;
                if matches!(fn_ret_ty, JadeType::Nil) {
                    return i64_to_value(ctx.context.i64_type().const_int(JRT_NIL_TAGGED, false), ret_ty, ctx);
                }
                let result: BasicValueEnum<'ctx> = match call_site.as_any_value_enum() {
                    AnyValueEnum::IntValue(v)     => v.into(),
                    AnyValueEnum::FloatValue(v)   => v.into(),
                    AnyValueEnum::PointerValue(v) => v.into(),
                    _ => ctx.context.i64_type().const_int(JRT_NIL_TAGGED, false).into(),
                };
                return convert_repr(result, &fn_ret_ty, ret_ty, ctx);
            }
        }
    }

    // ── Primitive method dispatch (table-driven) ─────────────────────────────────
    // Route a method call to the primitive emitter when the field is a known
    // builtin method OR the receiver is statically a primitive type (str/array/
    // dict) — primitives have no fields, so `s.foo(...)` is always a method call.
    // The latter ensures an unimplemented method yields a clear "unknown method"
    // error from emit_primitive_method rather than falling through to the struct
    // field-access path ("field access on non-struct: Str").
    if let TExprKind::FieldAccess { object, field } = &callee.kind {
        if stdlib::is_builtin_method(field)
            || matches!(effective_ty(&object.ty), JadeType::Str | JadeType::Array(_) | JadeType::Dict)
        {
            return emit_primitive_method(object, field, args, ret_ty, ctx);
        }
    }

    // ── Unknown-receiver runtime type-tag dispatch ───────────────────────────
    // When `obj.method(args)` has an Unknown receiver type but at least one
    // `<Type>__method` function exists in fn_info, the receiver is most likely
    // a struct value flowing through an untyped parameter or field. Read slot 0
    // (the type-name string pointer planted by `emit_struct_literal`) and
    // strcmp against every candidate type's name, dispatching to the matching
    // `<Type>__method`. Falls through to indirect-call if the tag matches
    // nothing — caller errors stay observable instead of silently segfaulting.
    if let TExprKind::FieldAccess { object, field } = &callee.kind {
        let receiver_is_unknown = matches!(object.ty, JadeType::Unknown);
        if receiver_is_unknown {
            let candidates: Vec<(String, inkwell::values::FunctionValue<'ctx>, Vec<JadeType>, JadeType)> =
                ctx.fn_info.iter()
                    .filter_map(|(k, v)| {
                        // Mangling is `<Type>__<method>`. Use the *first* `__`
                        // as the splitter so methods with leading underscores
                        // (e.g. `_dispatch_task` → `ToolGroup___dispatch_task`)
                        // resolve to `ty="ToolGroup", method="_dispatch_task"`
                        // rather than the rsplit answer of `ty="ToolGroup_"`.
                        k.split_once("__").and_then(|(ty, method)| {
                            // Match the method name AND the arity. Two structs can
                            // define a same-named method with different parameter
                            // counts (e.g. `ToolGroup.add(self, fn, ctx)` vs
                            // `SlashRegistry.add(self, name, help, handler)`); a
                            // candidate whose arity doesn't fit this call site
                            // would be emitted with the wrong number of arguments
                            // and LLVM rejects the module. v.1 is the full param
                            // list including `self`, so it must be args.len() + 1.
                            if method == field.as_str() && v.1.len() == args.len() + 1 {
                                Some((ty.to_string(), v.0, v.1.clone(), v.2.clone()))
                            } else { None }
                        })
                    })
                    .collect();
            if !candidates.is_empty() {
                let i64_ty = ctx.context.i64_type();
                let i8_ty = ctx.context.i8_type();
                let recv_ptr = as_pointer(emit_expr(object, ctx)?, ctx)?;
                // Load slot 0 → type-name pointer (stored as i64).
                let slot0 = ctx.gep(i64_ty, recv_ptr, &[i64_ty.const_int(0, false)], "rt_slot0")?;
                let name_i64 = ctx.builder
                    .build_load(i64_ty, slot0, "rt_name_i64")
                    .map_err(|e| e.to_string())?
                    .into_int_value();
                let name_ptr = ctx.builder
                    .build_int_to_ptr(name_i64, ctx.context.ptr_type(AddressSpace::default()), "rt_name_ptr")
                    .map_err(|e| e.to_string())?;

                // Emit arg expressions once (outside the if-chain). Codegen for
                // the same TExpr inside multiple branches would duplicate side
                // effects and rebind names.
                let arg_vals: Vec<BasicValueEnum<'ctx>> = args.iter()
                    .map(|a| emit_expr(a, ctx))
                    .collect::<Result<_, _>>()?;

                let fn_val = ctx.builder.get_insert_block().unwrap().get_parent().unwrap();
                let result_slot = ctx.build_entry_alloca(i64_ty.into(), "rt_dispatch_r")?;
                ctx.builder.build_store(result_slot, i64_ty.const_zero()).map_err(|e| e.to_string())?;
                let done_bb = ctx.context.append_basic_block(fn_val, "rt_dispatch_done");

                for (ty_name, fv, param_tys, fn_ret_ty) in &candidates {
                    let ty_lit = ctx.builder
                        .build_global_string_ptr(ty_name, "rt_ty_lit")
                        .map_err(|e| e.to_string())?
                        .as_pointer_value();
                    let cmp = ctx.call_rv(ctx.strcmp_fn, &[name_ptr.into(), ty_lit.into()], "rt_scmp")?
                        .into_int_value();
                    let zero = ctx.context.i32_type().const_zero();
                    let eq = ctx.builder
                        .build_int_compare(IntPredicate::EQ, cmp, zero, "rt_eq")
                        .map_err(|e| e.to_string())?;
                    let hit_bb = ctx.context.append_basic_block(fn_val, "rt_hit");
                    let next_bb = ctx.context.append_basic_block(fn_val, "rt_next");
                    ctx.builder.build_conditional_branch(eq, hit_bb, next_bb).map_err(|e| e.to_string())?;

                    ctx.builder.position_at_end(hit_bb);
                    let mut llvm_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(args.len() + 1);
                    llvm_args.push(recv_ptr.into());
                    for (i, av) in arg_vals.iter().enumerate() {
                        let param_ty = param_tys.get(i + 1).unwrap_or(&JadeType::Unknown);
                        let coerced = coerce(*av, &args[i].ty, param_ty, ctx)?;
                        llvm_args.push(coerced.into());
                    }
                    let call_site = ctx.builder
                        .build_call(*fv, &llvm_args, "rt_mcall")
                        .map_err(|e| e.to_string())?;
                    // Box the result by the *callee's* declared return type into the
                    // shared i64 slot (so a float is jrt_box_float'd, a string is
                    // string-tagged, etc.). The final i64_to_value(_, ret_ty) below
                    // then decodes to this call's use-site representation. (The old
                    // code stored a raw ptr2int / float-bitcast → an untagged value
                    // that downstream tag-dispatch misread.)
                    let ret_i64 = if matches!(fn_ret_ty, JadeType::Nil) {
                        i64_ty.const_int(JRT_NIL_TAGGED, false)
                    } else {
                        let bv: BasicValueEnum<'ctx> = match call_site.as_any_value_enum() {
                            AnyValueEnum::IntValue(v)     => v.into(),
                            AnyValueEnum::FloatValue(v)   => v.into(),
                            AnyValueEnum::PointerValue(v) => v.into(),
                            _ => i64_ty.const_int(JRT_NIL_TAGGED, false).into(),
                        };
                        value_to_i64(bv, fn_ret_ty, ctx)?
                    };
                    ctx.builder.build_store(result_slot, ret_i64).map_err(|e| e.to_string())?;
                    ctx.builder.build_unconditional_branch(done_bb).map_err(|e| e.to_string())?;

                    ctx.builder.position_at_end(next_bb);
                }
                // No type matched — leave result as 0 and fall through to done.
                ctx.builder.build_unconditional_branch(done_bb).map_err(|e| e.to_string())?;
                ctx.builder.position_at_end(done_bb);

                let raw = ctx.builder
                    .build_load(i64_ty, result_slot, "rt_r_load")
                    .map_err(|e| e.to_string())?
                    .into_int_value();
                let _ = i8_ty;
                return i64_to_value(raw, ret_ty, ctx);
            }
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

/// Parse a native function reference identifier (`__native$<pkgid>$<fn>`) into
/// its `(pkgid, fn_name)`. The renamer (imports.rs) produces this form for every
/// native package reference. Returns `None` for ordinary identifiers.
fn parse_native_ref(name: &str) -> Option<(u32, &str)> {
    let rest = name.strip_prefix("__native$")?;
    let (id, fname) = rest.split_once('$')?;
    Some((id.parse().ok()?, fname))
}

/// Build a stack `[N x i64]` of tagged arguments and return a pointer to its
/// first element (a null pointer when there are no args), for `jrt_native_call`.
fn build_tagged_argv<'ctx>(
    args: &[TExpr],
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<PointerValue<'ctx>, String> {
    let i64_ty = ctx.context.i64_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    if args.is_empty() {
        return Ok(ptr_ty.const_null());
    }
    let arr_ty = i64_ty.array_type(args.len() as u32);
    let argv = ctx.build_entry_alloca(arr_ty.into(), "native_argv")?;
    for (i, a) in args.iter().enumerate() {
        let v = emit_expr(a, ctx)?;
        let tagged = value_to_i64(v, &a.ty, ctx)?;
        let slot = ctx.gep(i64_ty, argv, &[i64_ty.const_int(i as u64, false)], &format!("native_argv{i}"))?;
        ctx.builder.build_store(slot, tagged).map_err(|e| e.to_string())?;
    }
    Ok(argv)
}

/// Direct native (C-ABI) call: `__native$<pkgid>$<fn>(args…)`. Loads the package
/// handle, marshals tagged args, dispatches through `jrt_native_call`, and
/// reinterprets the tagged result as `ret_ty`. Handles arbitrary arity.
fn emit_native_call<'ctx>(
    pkgid: u32,
    fname: &str,
    args: &[TExpr],
    ret_ty: &JadeType,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    ctx.uses_runtime = true;
    ctx.uses_exceptions = true; // jrt_native_call can raise
    let i64_ty = ctx.context.i64_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());

    let g = *ctx.native_pkgs.get(&pkgid)
        .ok_or_else(|| format!("native package {pkgid} not registered"))?;
    let handle = ctx.builder
        .build_load(ptr_ty, g.as_pointer_value(), "native_handle")
        .map_err(|e| e.to_string())?
        .into_pointer_value();

    let argv = build_tagged_argv(args, ctx)?;
    let name_ptr = ctx.builder
        .build_global_string_ptr(fname, "native_fn_name")
        .map_err(|e| e.to_string())?
        .as_pointer_value();
    let call_fn = ctx.get_jrt_native_call();
    let raw = ctx.call_rv(
        call_fn,
        &[handle.into(), name_ptr.into(), argv.into(), i64_ty.const_int(args.len() as u64, false).into()],
        "native_call",
    )?.into_int_value();
    i64_to_value(raw, ret_ty, ctx)
}

/// Build a first-class native function value: a `jade_fn_t` whose `fn_ptr` field
/// is the address of `jrt_native_call` used purely as a sentinel marker (a real
/// closure / named-fn pointer is never this symbol), and whose `env_ptr` carries
/// `{ handle, fn_name }`. `emit_indirect_call` recognizes the sentinel and routes
/// the call through `jrt_native_call`.
fn emit_native_fn_value<'ctx>(
    pkgid: u32,
    fname: &str,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let i64_ty = ctx.context.i64_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());

    // env = { ptr handle, ptr name }
    let g = *ctx.native_pkgs.get(&pkgid)
        .ok_or_else(|| format!("native package {pkgid} not registered"))?;
    let handle = ctx.builder
        .build_load(ptr_ty, g.as_pointer_value(), "native_handle")
        .map_err(|e| e.to_string())?;
    let name_ptr = ctx.builder
        .build_global_string_ptr(fname, "native_fn_name")
        .map_err(|e| e.to_string())?
        .as_pointer_value();
    let env = ctx.malloc_ptr(i64_ty.const_int(16, false), "native_env")?;
    let h_slot = ctx.gep(ptr_ty, env, &[i64_ty.const_int(0, false)], "env_h")?;
    ctx.builder.build_store(h_slot, handle).map_err(|e| e.to_string())?;
    let n_slot = ctx.gep(ptr_ty, env, &[i64_ty.const_int(1, false)], "env_n")?;
    ctx.builder.build_store(n_slot, name_ptr).map_err(|e| e.to_string())?;

    // jade_fn_t { fn_ptr = &jrt_native_call (sentinel), env, name }
    let marker = ctx.get_jrt_native_call().as_global_value().as_pointer_value();
    let jade_fn = ctx.malloc_ptr(i64_ty.const_int(24, false), "native_fn_val")?;
    let f0 = ctx.builder.build_struct_gep(ctx.jade_fn_ty, jade_fn, 0, "nfv_f0").map_err(|e| e.to_string())?;
    ctx.builder.build_store(f0, marker).map_err(|e| e.to_string())?;
    let f1 = ctx.builder.build_struct_gep(ctx.jade_fn_ty, jade_fn, 1, "nfv_f1").map_err(|e| e.to_string())?;
    ctx.builder.build_store(f1, env).map_err(|e| e.to_string())?;
    let f2 = ctx.builder.build_struct_gep(ctx.jade_fn_ty, jade_fn, 2, "nfv_f2").map_err(|e| e.to_string())?;
    ctx.builder.build_store(f2, name_ptr).map_err(|e| e.to_string())?;
    Ok(jade_fn.into())
}

/// Call a first-class function value (closure, named-fn-as-value, or native-fn
/// value) stored as a `jade_fn_t*`. All arguments are packed as `i64`. A native
/// value (recognized by the `jrt_native_call` sentinel in `fn_ptr`) dispatches
/// through `jrt_native_call`; everything else uses the uniform indirect ABI
/// `i64(i64..., ptr env)`. The result is returned as `i64` then reinterpreted.
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

    // Coerce all arguments to tagged i64 once (shared by both branches).
    let tagged: Vec<IntValue<'ctx>> = args.iter()
        .map(|a| { let v = emit_expr(a, ctx)?; value_to_i64(v, &a.ty, ctx) })
        .collect::<Result<_, _>>()?;

    // Branch on whether this is a native-fn value (fn_ptr == &jrt_native_call).
    let marker = ctx.get_jrt_native_call().as_global_value().as_pointer_value();
    let is_native = ctx.builder
        .build_int_compare(IntPredicate::EQ,
            ctx.builder.build_ptr_to_int(fn_ptr, i64_ty, "fp_i").map_err(|e| e.to_string())?,
            ctx.builder.build_ptr_to_int(marker, i64_ty, "mk_i").map_err(|e| e.to_string())?,
            "is_native")
        .map_err(|e| e.to_string())?;

    let cur_fn = ctx.builder.get_insert_block().unwrap().get_parent().unwrap();
    let native_bb = ctx.context.append_basic_block(cur_fn, "icall_native");
    let normal_bb = ctx.context.append_basic_block(cur_fn, "icall_normal");
    let cont_bb = ctx.context.append_basic_block(cur_fn, "icall_cont");
    ctx.builder.build_conditional_branch(is_native, native_bb, normal_bb).map_err(|e| e.to_string())?;

    // ── native branch: jrt_native_call(handle, name, argv, argc) ──────────────
    ctx.builder.position_at_end(native_bb);
    let raw_native = {
        ctx.uses_exceptions = true;
        // env = { ptr handle, ptr name }
        let h_slot = ctx.gep(ptr_ty, env_ptr, &[i64_ty.const_int(0, false)], "nenv_h")?;
        let handle = ctx.builder.build_load(ptr_ty, h_slot, "nhandle").map_err(|e| e.to_string())?.into_pointer_value();
        let n_slot = ctx.gep(ptr_ty, env_ptr, &[i64_ty.const_int(1, false)], "nenv_n")?;
        let name = ctx.builder.build_load(ptr_ty, n_slot, "nname").map_err(|e| e.to_string())?.into_pointer_value();
        let argv = if tagged.is_empty() {
            ptr_ty.const_null()
        } else {
            let arr_ty = i64_ty.array_type(tagged.len() as u32);
            let argv = ctx.build_entry_alloca(arr_ty.into(), "nargv")?;
            for (i, t) in tagged.iter().enumerate() {
                let slot = ctx.gep(i64_ty, argv, &[i64_ty.const_int(i as u64, false)], &format!("nargv{i}"))?;
                ctx.builder.build_store(slot, *t).map_err(|e| e.to_string())?;
            }
            argv
        };
        let call_fn = ctx.get_jrt_native_call();
        ctx.call_rv(call_fn,
            &[handle.into(), name.into(), argv.into(), i64_ty.const_int(tagged.len() as u64, false).into()],
            "native_icall")?.into_int_value()
    };
    let native_end = ctx.builder.get_insert_block().unwrap();
    ctx.builder.build_unconditional_branch(cont_bb).map_err(|e| e.to_string())?;

    // ── normal branch: uniform indirect ABI i64(i64..., ptr env) ──────────────
    ctx.builder.position_at_end(normal_bb);
    let raw_normal = {
        let mut llvm_args: Vec<BasicMetadataValueEnum<'ctx>> = tagged.iter().map(|t| (*t).into()).collect();
        llvm_args.push(env_ptr.into());
        let mut param_tys: Vec<BasicMetadataTypeEnum<'ctx>> =
            args.iter().map(|_| BasicMetadataTypeEnum::IntType(i64_ty)).collect();
        param_tys.push(BasicMetadataTypeEnum::PointerType(ptr_ty));
        let indirect_fn_ty = i64_ty.fn_type(&param_tys, false);
        let call_site = ctx.builder
            .build_indirect_call(indirect_fn_ty, fn_ptr, &llvm_args, "icall")
            .map_err(|e| e.to_string())?;
        match call_site.as_any_value_enum() {
            AnyValueEnum::IntValue(v) => v,
            _ => i64_ty.const_int(0, false),
        }
    };
    let normal_end = ctx.builder.get_insert_block().unwrap();
    ctx.builder.build_unconditional_branch(cont_bb).map_err(|e| e.to_string())?;

    // ── join ──────────────────────────────────────────────────────────────────
    ctx.builder.position_at_end(cont_bb);
    let phi = ctx.builder.build_phi(i64_ty, "icall_r").map_err(|e| e.to_string())?;
    phi.add_incoming(&[(&raw_native, native_end), (&raw_normal, normal_end)]);
    i64_to_value(phi.as_basic_value().into_int_value(), ret_ty, ctx)
}

// ── Primitive method dispatch ─────────────────────────────────────────────────

/// Emit a primitive (built-in) method call on a str/array/dict receiver — e.g.
/// `s.upper()`, `a.push(x)`, `d.get(k)`, `len`. Most route to a runtime `jrt_*`
/// via the Sig table; a few (len, get, sort) are special-cased here.
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
                    convert_repr(b.into(), &JadeType::Bool, ret_ty, ctx)
                }
                _ => {
                    let sig = stdlib::str_method_sig("contains").unwrap();
                    let fn_val = ctx.extern_fn(&sig);
                    let res = ctx.call_rv(fn_val, &[obj_ptr.into(), needle.into()], "pm_contains")?.into_int_value();
                    let b = ctx.builder.build_int_compare(IntPredicate::NE, res, ctx.context.i32_type().const_zero(), "cont_b").map_err(|e| e.to_string())?;
                    convert_repr(b.into(), &JadeType::Bool, ret_ty, ctx)
                }
            }
        }

        // ── String predicates (starts_with / ends_with) → i1 ──────────────────
        "starts_with" | "ends_with" => {
            let obj_ptr = as_pointer(obj_val, ctx)?;
            let arg = as_pointer(emit_expr(args.get(0).ok_or("starts_with/ends_with: missing arg")?, ctx)?, ctx)?;
            let sig = stdlib::str_method_sig(method).unwrap();
            let fn_val = ctx.extern_fn(&sig);
            let res = ctx.call_rv(fn_val, &[obj_ptr.into(), arg.into()], "pm_sw")?.into_int_value();
            let b = ctx.builder.build_int_compare(IntPredicate::NE, res, ctx.context.i32_type().const_zero(), "sw_b")
                .map_err(|e| e.to_string())?;
            // Match the call's static type: native i1 for a Bool result, tagged
            // for an Unknown one (a bool method on an Unknown receiver).
            convert_repr(b.into(), &JadeType::Bool, ret_ty, ctx)
        }

        // ── String primitive methods (table-driven) ───────────────────────────
        "split" | "trim" | "replace" | "upper" | "lower" => {
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
                    stdlib::Arg::I64Raw => match &arg_expr.ty {
                        JadeType::Unknown => i64_to_value(v.into_int_value(), &JadeType::Int, ctx)?.into(),
                        _ => v.into_int_value().into(),
                    },
                };
                llvm_args.push(coerced);
            }
            let fn_val = ctx.extern_fn(&sig);
            let r = ctx.call_rv(fn_val, &llvm_args, "pm_str")?;
            // split returns an array; the rest return strings. Convert to the
            // call's static type (native for a concrete result, tagged for an
            // Unknown one — i.e. the method called on an Unknown receiver).
            let from = if method == "split" { JadeType::Array(Box::new(JadeType::Unknown)) } else { JadeType::Str };
            convert_repr(r, &from, ret_ty, ctx)
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
            convert_repr(b.into(), &JadeType::Bool, ret_ty, ctx)
        }
        "get" => {
            ctx.uses_dicts = true;
            let obj_ptr = as_pointer(obj_val, ctx)?;
            let key = as_pointer(emit_expr(args.get(0).ok_or("get: missing arg")?, ctx)?, ctx)?;
            let raw = ctx.call_rv(ctx.jade_dict_get_fn, &[obj_ptr.into(), key.into()], "pm_get")?.into_int_value();
            i64_to_value(raw, ret_ty, ctx)
        }

        // ── array sort / reverse: mutate in place, return Nil ─────────────────
        "sort" | "reverse" => {
            ctx.uses_runtime = true;
            let obj_ptr = as_pointer(obj_val, ctx)?;
            let cname = if method == "sort" { "jrt_array_sort" } else { "jrt_array_reverse" };
            let f = if let Some(f) = ctx.module.get_function(cname) { f } else {
                let void_ty = ctx.context.void_type();
                let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
                ctx.module.add_function(cname, void_ty.fn_type(&[ptr_ty.into()], false), None)
            };
            ctx.call_void(f, &[obj_ptr.into()])?;
            // The VM's primitive sort/reverse return Nil.
            i64_to_value(i64_ty.const_int(JRT_NIL_TAGGED, false), ret_ty, ctx)
        }

        // ── dict keys / values: return an array (keys & values sorted by key) ──
        "keys" | "values" => {
            ctx.uses_dicts = true;
            let obj_ptr = as_pointer(obj_val, ctx)?;
            let cname = if method == "keys" { "jrt_dict_keys" } else { "jrt_dict_values" };
            let f = if let Some(f) = ctx.module.get_function(cname) { f } else {
                let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
                ctx.module.add_function(cname, ptr_ty.fn_type(&[ptr_ty.into()], false), None)
            };
            let r = ctx.call_rv(f, &[obj_ptr.into()], "pm_dict_kv")?;
            // Result is a jade_array ptr; surface it as the call's static type
            // (native array ptr for a concrete Array, tagged for an Unknown call).
            convert_repr(r, &JadeType::Array(Box::new(JadeType::Unknown)), ret_ty, ctx)
        }

        // ── len: type-dispatch special case ───────────────────────────────────
        "len" => {
            let lenval: BasicValueEnum<'ctx> = match obj_ty {
                // Unknown receiver: the value's kind (string vs array) is only
                // known at runtime — dispatch on its tag via jrt_len_any rather
                // than blindly reading an array header (which read a string's
                // bytes as a header → garbage length).
                JadeType::Unknown => {
                    ctx.uses_runtime = true;
                    let tagged = value_to_i64(obj_val, &JadeType::Unknown, ctx)?;
                    let f = if let Some(f) = ctx.module.get_function("jrt_len_any") { f } else {
                        let i64t = ctx.context.i64_type();
                        ctx.module.add_function("jrt_len_any", i64t.fn_type(&[i64t.into()], false), None)
                    };
                    ctx.call_rv(f, &[tagged.into()], "pm_len_any")?
                }
                JadeType::Str => {
                    let obj_ptr = as_pointer(obj_val, ctx)?;
                    ctx.call_rv(ctx.strlen_fn, &[obj_ptr.into()], "pm_slen")?
                }
                JadeType::Dict => {
                    ctx.uses_dicts = true;
                    let obj_ptr = as_pointer(obj_val, ctx)?;
                    ctx.call_rv(ctx.jade_dict_len_fn, &[obj_ptr.into()], "pm_dlen")?
                }
                _ => {
                    // Array — read .len field from jade.array header.
                    let arr_ptr = as_pointer(obj_val, ctx)?;
                    let f1 = ctx.builder.build_struct_gep(ctx.array_ty, arr_ptr, 1, "pm_alen_f1").map_err(|e| e.to_string())?;
                    ctx.builder.build_load(i64_ty, f1, "pm_alen").map_err(|e| e.to_string())?
                }
            };
            // len is an Int; box it when the call's static type is Unknown.
            convert_repr(lenval, &JadeType::Int, ret_ty, ctx)
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
    pretty: bool,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    if let TExprKind::Dict { entries } = &arg.kind {
        // An empty object is "{}" in both modes (serde's to_string_pretty too).
        if entries.is_empty() {
            return emit_tagged_literal("{}", ctx);
        }
        // Seeds must be tagged literals — emit_str_concat reads the trust
        // header of both operands; a bare `build_global_string_ptr` would
        // dereference offset -1 of .rodata (undefined behaviour, often a fault).
        // `pretty` mirrors serde_json::to_string_pretty (2-space indent) for a
        // flat object; nested values delegate to the compact serializer, so
        // deeply-nested pretty output may differ — the same flat-first limit the
        // compact stringify already has.
        let mut acc: BasicValueEnum<'ctx> =
            emit_tagged_literal(if pretty { "{\n" } else { "{" }, ctx)?;

        for (i, (key_expr, val_expr)) in entries.iter().enumerate() {
            let key_str = match &key_expr.kind {
                TExprKind::Str(s) => s.clone(),
                _ => return Err("json.stringify: non-string dict key".to_string()),
            };
            let prefix = match (pretty, i == 0) {
                (true, true)   => format!("  \"{key_str}\": "),
                (true, false)  => format!(",\n  \"{key_str}\": "),
                (false, true)  => format!("\"{key_str}\": "),
                (false, false) => format!(", \"{key_str}\": "),
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

        let close = emit_tagged_literal(if pretty { "\n}" } else { "}" }, ctx)?;
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
        let prompt_ptr = as_pointer(emit_expr(pexpr, ctx)?, ctx)?;
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

/// Print a value to stdout, dispatching on its (actual) static type: scalars
/// and strings format directly; arrays recurse via `emit_print_array`; an
/// Unknown value routes to the tag-dispatching `jrt_snprintf_any`. `suffix` is
/// appended (e.g. a trailing newline for `print`).
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
            // Format VM-style into a scratch buffer, then print it as a string.
            ctx.uses_runtime = true;
            let cap = ctx.context.i64_type().const_int(64, false);
            let buf = ctx.malloc_ptr(cap, "fltbuf")?;
            let f = jrt_snprintf_float_fn(ctx);
            ctx.builder.build_call(f, &[buf.into(), cap.into(), val.into()], "").map_err(|e| e.to_string())?;
            let fmt = sp(&format!("%s{suffix}"), ctx)?;
            ctx.builder.build_call(ctx.printf_fn, &[fmt.into(), buf.into()], "").map_err(|e| e.to_string())?;
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
            // A NULL char* in a Str slot is nil that flowed through a typed
            // (Str-inferred) return/slot — e.g. `fn f(){ if c {return "x"}; return nil }`.
            // A real Jade string is never NULL (even "" allocates a tagged buffer),
            // so print it as "nil" to match the VM rather than libc's "(null)".
            let p = val.into_pointer_value();
            let nil_lit = sp("nil", ctx)?;
            let is_null = ctx.builder.build_is_null(p, "str_is_nil").map_err(|e| e.to_string())?;
            let sel = ctx.builder.build_select(is_null, nil_lit, p, "str_or_nil").map_err(|e| e.to_string())?;
            let fmt = sp(&format!("%s{suffix}"), ctx)?;
            ctx.builder.build_call(ctx.printf_fn, &[fmt.into(), sel.into()], "").map_err(|e| e.to_string())?;
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
        JadeType::Unknown => {
            // Static type is Unknown: the i64 is a tagged value. jrt_print_any
            // formats it by its runtime tag (int/float/bool/nil) and writes strings
            // unbounded — so a long Unknown string (e.g. llm.tool_grammar()) prints
            // in full rather than truncating at a fixed scratch buffer.
            ctx.uses_runtime = true;
            let i64_ty = ctx.context.i64_type();
            let v = val.into_int_value();
            let suffix_ptr = sp(suffix, ctx)?;
            let print_fn = ctx.module.get_function("jrt_print_any").unwrap_or_else(|| {
                let ptr = ctx.context.ptr_type(AddressSpace::default());
                let ty  = ctx.context.void_type().fn_type(&[i64_ty.into(), ptr.into()], false);
                ctx.module.add_function("jrt_print_any", ty, None)
            });
            ctx.builder.build_call(print_fn, &[v.into(), suffix_ptr.into()], "")
                .map_err(|e| e.to_string())?;
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

// ── Tagged value conversion (low-bit tags) ────────────────────────────────────
// See runtime.h "Tagged value ABI". `value_to_i64` (box) and `i64_to_value`
// (unbox) are the ONLY places that apply/strip tags. Per option A, statically-
// typed SSA values stay native (raw i64 / f64 / i1 / ptr); a value only becomes
// tagged when it enters a type-erased slot (dict/array/struct/Unknown/return)
// and is unboxed back to native on the way out. An `Unknown` SSA value IS a
// tagged word, so box/unbox are the identity for it. Tags (see runtime.h):
//   INT bit0==0 (v<<1)  PTR 0b001 (non-string heap)  FLOAT 0b011 (boxed)
//   STR 0b101 (heap string)  IMM 0b111: nil=0b0111, bool=0b1111 (value in bit4)

const JRT_NIL_TAGGED: u64 = 7;       // 0b00111
const JRT_FALSE_TAGGED: u64 = 15;    // 0b01111  (bool false; true = 0b11111 = 31)

/// Lower a condition value to an i1. A native i1 is used as-is; a tagged value
/// (Unknown) is truthy unless it is Bool(false) (tagged 15) — matching the VM,
/// where only Bool(false) is falsy (`JumpIfFalse` jumps only on Bool(false)).
pub(crate) fn emit_cond_i1<'ctx>(
    val: BasicValueEnum<'ctx>,
    ctx: &CodegenCtx<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    let iv = val.into_int_value();
    if iv.get_type().get_bit_width() == 1 {
        return Ok(iv);
    }
    let false_tag = ctx.context.i64_type().const_int(JRT_FALSE_TAGGED, false);
    ctx.builder
        .build_int_compare(IntPredicate::NE, iv, false_tag, "cond_i1")
        .map_err(|e| e.to_string())
}

fn tag_int_iv<'ctx>(v: IntValue<'ctx>, ctx: &CodegenCtx<'ctx>) -> Result<IntValue<'ctx>, String> {
    let one = ctx.context.i64_type().const_int(1, false);
    ctx.builder.build_left_shift(v, one, "tag_int").map_err(|e| e.to_string())
}
pub(crate) fn untag_int_iv<'ctx>(v: IntValue<'ctx>, ctx: &CodegenCtx<'ctx>) -> Result<IntValue<'ctx>, String> {
    let one = ctx.context.i64_type().const_int(1, false);
    ctx.builder.build_right_shift(v, one, true, "untag_int").map_err(|e| e.to_string())
}
fn tag_ptr_iv<'ctx>(p: inkwell::values::PointerValue<'ctx>, ctx: &CodegenCtx<'ctx>) -> Result<IntValue<'ctx>, String> {
    let i = ctx.builder.build_ptr_to_int(p, ctx.context.i64_type(), "p2i").map_err(|e| e.to_string())?;
    let one = ctx.context.i64_type().const_int(1, false);
    ctx.builder.build_or(i, one, "tag_ptr").map_err(|e| e.to_string())
}
fn untag_ptr_iv<'ctx>(v: IntValue<'ctx>, ctx: &CodegenCtx<'ctx>) -> Result<inkwell::values::PointerValue<'ctx>, String> {
    let mask = ctx.context.i64_type().const_int(!7u64, false);
    let m = ctx.builder.build_and(v, mask, "untag_mask").map_err(|e| e.to_string())?;
    ctx.builder.build_int_to_ptr(m, ctx.context.ptr_type(AddressSpace::default()), "untag_ptr").map_err(|e| e.to_string())
}
/// Tag a heap STRING pointer (low3 = 0b101), distinct from a non-string heap
/// pointer (tag_ptr_iv, low3 = 0b001) so the runtime can tell strings apart.
fn tag_str_iv<'ctx>(p: inkwell::values::PointerValue<'ctx>, ctx: &CodegenCtx<'ctx>) -> Result<IntValue<'ctx>, String> {
    let i = ctx.builder.build_ptr_to_int(p, ctx.context.i64_type(), "p2i_str").map_err(|e| e.to_string())?;
    let tag = ctx.context.i64_type().const_int(5, false);
    ctx.builder.build_or(i, tag, "tag_str").map_err(|e| e.to_string())
}
/// Box a bool i1 → immediate (false=0b01111=15, true=0b11111=31): value in bit4.
fn tag_bool_iv<'ctx>(b: IntValue<'ctx>, ctx: &CodegenCtx<'ctx>) -> Result<IntValue<'ctx>, String> {
    let i64t = ctx.context.i64_type();
    let z = ctx.builder.build_int_z_extend(b, i64t, "b2i").map_err(|e| e.to_string())?;
    let four = i64t.const_int(4, false);
    let sh = ctx.builder.build_left_shift(z, four, "b_shl").map_err(|e| e.to_string())?;
    let tag = i64t.const_int(0xf, false);  /* low4 = 0b1111 marks bool */
    ctx.builder.build_or(sh, tag, "tag_bool").map_err(|e| e.to_string())
}
/// Decode a tagged bool back to i1 (value lives in bit4). Already-i1 values pass
/// through unchanged — a bool may arrive native (e.g. a bool-returning method on
/// an Unknown receiver yields a raw i1) rather than as a tagged i64.
fn untag_bool_iv<'ctx>(v: IntValue<'ctx>, ctx: &CodegenCtx<'ctx>) -> Result<IntValue<'ctx>, String> {
    if v.get_type().get_bit_width() == 1 {
        return Ok(v);
    }
    let four = ctx.context.i64_type().const_int(4, false);
    let sh = ctx.builder.build_right_shift(v, four, false, "b_lshr").map_err(|e| e.to_string())?;
    ctx.builder.build_int_truncate(sh, ctx.context.bool_type(), "untag_bool").map_err(|e| e.to_string())
}
/// Lazily declare `jade_value_t jrt_box_float(double)`.
fn jrt_box_float_fn<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("jrt_box_float") { return f; }
    let ty = ctx.context.i64_type().fn_type(&[ctx.context.f64_type().into()], false);
    ctx.module.add_function("jrt_box_float", ty, None)
}
/// Lazily declare `double jrt_unbox_float(jade_value_t)`.
fn jrt_unbox_float_fn<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("jrt_unbox_float") { return f; }
    let ty = ctx.context.f64_type().fn_type(&[ctx.context.i64_type().into()], false);
    ctx.module.add_function("jrt_unbox_float", ty, None)
}
/// Lazily declare a tagged binary helper `jade_value_t f(jade_value_t, jade_value_t)`.
fn jrt_any2_fn<'ctx>(name: &str, ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function(name) { return f; }
    let i64t = ctx.context.i64_type();
    let ty = i64t.fn_type(&[i64t.into(), i64t.into()], false);
    ctx.module.add_function(name, ty, None)
}
/// Lazily declare `jade_value_t jrt_neg_any(jade_value_t)`.
fn jrt_neg_any_fn<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("jrt_neg_any") { return f; }
    let i64t = ctx.context.i64_type();
    ctx.module.add_function("jrt_neg_any", i64t.fn_type(&[i64t.into()], false), None)
}
/// Lazily declare `int jrt_cmp_any(jade_value_t, jade_value_t)`.
fn jrt_cmp_any_fn<'ctx>(ctx: &CodegenCtx<'ctx>) -> FunctionValue<'ctx> {
    if let Some(f) = ctx.module.get_function("jrt_cmp_any") { return f; }
    let i64t = ctx.context.i64_type();
    let ty = ctx.context.i32_type().fn_type(&[i64t.into(), i64t.into()], false);
    ctx.module.add_function("jrt_cmp_any", ty, None)
}

/// Tagged dynamic binop for when either operand's static type is Unknown. Boxes
/// both operands and dispatches to the jrt_*_any runtime helpers, mirroring the
/// VM's eval_binop_dynamic. Arithmetic returns a tagged i64 (the Unknown SSA
/// representation); comparisons return a native i1 (Bool).
fn emit_binop_any<'ctx>(
    op: &BinOpKind,
    lty: &JadeType,
    lhs: BasicValueEnum<'ctx>,
    rty: &JadeType,
    rhs: BasicValueEnum<'ctx>,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    use BinOpKind::*;
    ctx.uses_runtime = true;
    let la = value_to_i64(lhs, lty, ctx)?;
    let ra = value_to_i64(rhs, rty, ctx)?;
    let i32z = ctx.context.i32_type().const_zero();
    match op {
        Eq | Ne => {
            let f = ctx.module.get_function("jrt_eq_any").unwrap_or_else(|| {
                let i64t = ctx.context.i64_type();
                let ty = ctx.context.i32_type().fn_type(&[i64t.into(), i64t.into()], false);
                ctx.module.add_function("jrt_eq_any", ty, None)
            });
            let r = ctx.call_rv(f, &[la.into(), ra.into()], "eq_any")?.into_int_value();
            let pred = if matches!(op, Eq) { IntPredicate::NE } else { IntPredicate::EQ };
            // eq_any returns 1 on equal: Eq → (r != 0), Ne → (r == 0).
            Ok(ctx.builder.build_int_compare(pred, r, i32z, "eq_any_b").map_err(|e| e.to_string())?.into())
        }
        Lt | Gt | Le | Ge => {
            let f = jrt_cmp_any_fn(ctx);
            let c = ctx.call_rv(f, &[la.into(), ra.into()], "cmp_any")?.into_int_value();
            let pred = match op { Lt => IntPredicate::SLT, Gt => IntPredicate::SGT,
                                  Le => IntPredicate::SLE, _ => IntPredicate::SGE };
            Ok(ctx.builder.build_int_compare(pred, c, i32z, "cmp_any_b").map_err(|e| e.to_string())?.into())
        }
        BitAnd | BitOr | BitXor | Shl | Shr => {
            // Bitwise/shift are int-only; unbox, apply natively, re-tag.
            let li = untag_int_iv(la, ctx)?;
            let ri = untag_int_iv(ra, ctx)?;
            let r = match op {
                BitAnd => ctx.builder.build_and(li, ri, "band").map_err(|e| e.to_string())?,
                BitOr  => ctx.builder.build_or(li, ri, "bor").map_err(|e| e.to_string())?,
                BitXor => ctx.builder.build_xor(li, ri, "bxor").map_err(|e| e.to_string())?,
                Shl    => ctx.builder.build_left_shift(li, ri, "shl").map_err(|e| e.to_string())?,
                _      => ctx.builder.build_right_shift(li, ri, true, "shr").map_err(|e| e.to_string())?,
            };
            Ok(tag_int_iv(r, ctx)?.into())
        }
        Add | Sub | Mul | Div | Mod => {
            let name = match op { Add => "jrt_add_any", Sub => "jrt_sub_any", Mul => "jrt_mul_any",
                                  Div => "jrt_div_any", _ => "jrt_mod_any" };
            let f = jrt_any2_fn(name, ctx);
            let r = ctx.call_rv(f, &[la.into(), ra.into()], "arith_any")?.into_int_value();
            Ok(r.into()) // tagged i64 = Unknown SSA
        }
        In | NotIn | And | Or =>
            Err(format!("emit_binop_any: {:?} should be handled before dispatch", op)),
    }
}

/// Convert a value from one Jade type's native representation to another's,
/// round-tripping through the tagged form. Identity when the types match. Used
/// where a value's static type at a use site differs from how it was stored
/// (reassigned variables, slot reads vs. expression types).
pub(crate) fn convert_repr<'ctx>(
    val: BasicValueEnum<'ctx>,
    from: &JadeType,
    to: &JadeType,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    // Fast path only for matching *concrete* types. When either side is Unknown
    // we must still route through the tag boundary: some emit paths produce a
    // native value (e.g. `x.trim()` returns a ptr) for an Unknown-typed
    // expression, so the value isn't necessarily already tagged. `value_to_i64`
    // with the actual `from` type normalizes it (a native ptr/float → tagged; an
    // already-tagged i64 → identity, emitting no instructions), and
    // `i64_to_value` unboxes to `to`.
    if from == to && !matches!(to, JadeType::Unknown) {
        return Ok(val);
    }
    let tagged = value_to_i64(val, from, ctx)?;
    i64_to_value(tagged, to, ctx)
}

/// Box any Jade value into a tagged i64 for storage in a type-erased slot.
pub fn value_to_i64<'ctx>(
    val: BasicValueEnum<'ctx>,
    ty: &JadeType,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    let i64_ty = ctx.context.i64_type();
    match ty {
        // An Unknown value is already a tagged word — pass it through (widen a
        // narrow int defensively; tag a stray pointer/float that slipped in).
        JadeType::Unknown => match val {
            BasicValueEnum::IntValue(v) => {
                if v.get_type().get_bit_width() < 64 {
                    ctx.builder.build_int_z_extend(v, i64_ty, "zext_i64").map_err(|e| e.to_string())
                } else { Ok(v) }
            }
            // A native pointer for an Unknown-typed expr comes from a string-
            // producing path (e.g. x.trim()); concrete dict/array/struct values
            // are tagged at their own typed boundaries below. Tag it as a string
            // so print/eq treat it correctly.
            BasicValueEnum::PointerValue(p) => tag_str_iv(p, ctx),
            BasicValueEnum::FloatValue(f) => {
                let bf = jrt_box_float_fn(ctx);
                Ok(ctx.call_rv(bf, &[f.into()], "boxf")?.into_int_value())
            }
            _ => Err(format!("value_to_i64: unhandled Unknown value {:?}", val)),
        },
        JadeType::Nil => Ok(i64_ty.const_int(JRT_NIL_TAGGED, false)),
        JadeType::Bool => tag_bool_iv(val.into_int_value(), ctx),
        JadeType::Float => {
            let bf = jrt_box_float_fn(ctx);
            Ok(ctx.call_rv(bf, &[val.into_float_value().into()], "boxf")?.into_int_value())
        }
        // Strings (and prompts, which are string pointers) get the STRING tag;
        // every other heap kind gets the generic non-string pointer tag.
        JadeType::Str | JadeType::Prompt =>
            tag_str_iv(val.into_pointer_value(), ctx),
        JadeType::Array(_) | JadeType::Struct(_)
        | JadeType::Dict | JadeType::Fn { .. } | JadeType::AsyncFn { .. }
        | JadeType::Future(_) | JadeType::Grammar =>
            tag_ptr_iv(val.into_pointer_value(), ctx),
        // Int (and any other scalar): tag as a small integer.
        _ => {
            let v = val.into_int_value();
            let w = if v.get_type().get_bit_width() < 64 {
                ctx.builder.build_int_z_extend(v, i64_ty, "zext_i64").map_err(|e| e.to_string())?
            } else { v };
            tag_int_iv(w, ctx)
        }
    }
}

/// Unbox a tagged i64 slot value back to a native value of the given type.
pub fn i64_to_value<'ctx>(
    raw: IntValue<'ctx>,
    ty: &JadeType,
    ctx: &mut CodegenCtx<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    match ty {
        // Unknown stays tagged (it's the type-erased representation).
        JadeType::Unknown | JadeType::Nil => Ok(raw.into()),
        JadeType::Float => {
            let uf = jrt_unbox_float_fn(ctx);
            Ok(ctx.call_rv(uf, &[raw.into()], "unboxf")?.into_float_value().into())
        }
        JadeType::Bool => Ok(untag_bool_iv(raw, ctx)?.into()),
        JadeType::Str | JadeType::Array(_) | JadeType::Struct(_)
        | JadeType::Dict | JadeType::Fn { .. } | JadeType::AsyncFn { .. }
        | JadeType::Future(_) | JadeType::Prompt | JadeType::Grammar =>
            Ok(untag_ptr_iv(raw, ctx)?.into()),
        _ => Ok(untag_int_iv(raw, ctx)?.into()),
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
        // A heap value carried in an i64 is a tagged pointer; mask off the low
        // tag bits (no-op for an already-aligned raw pointer, e.g. struct slot
        // 0) before reinterpreting it as a pointer.
        BasicValueEnum::IntValue(i) => untag_ptr_iv(i, ctx),
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
            // Preserve Unknown so emit_binop can refine it against the other
            // operand (e.g. an untyped function param carrying a string ptr in
            // an i64 still needs to be treated as Str when compared with "").
            JadeType::Unknown => JadeType::Unknown,
            JadeType::Int | JadeType::Nil => effective_ty(declared),
            _ => JadeType::Int,
        },
        _ => declared.clone(),
    }
}

/// Coerce `val` from `actual_ty` to `target_ty` when passing arguments.
pub(crate) fn coerce<'ctx>(
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
    // An Unknown target (e.g. every user-function param) receives a tagged
    // word — box the value by its actual type so the callee, which treats the
    // param as Unknown, reads a correctly-tagged value.
    // An Unknown target (e.g. every user-function param) receives a tagged word.
    // value_to_i64 by the actual type boxes correctly — and for an Unknown actual
    // it normalizes a native ptr/float (which some emit paths produce for an
    // Unknown-typed expr, e.g. `x.trim()`) while leaving an already-tagged i64
    // untouched.
    if matches!(target_ty, JadeType::Unknown) {
        return Ok(value_to_i64(val, actual_ty, ctx)?.into());
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
