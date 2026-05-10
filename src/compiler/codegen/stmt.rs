use std::collections::HashMap;

use inkwell::{AddressSpace, IntPredicate};

use crate::compiler::tir::{JadeType, TExprKind, TStmt};

use super::{expr, types, CodegenCtx};

// ── Struct pre-pass ───────────────────────────────────────────────────────────

/// Walk all statements and record field names for every `StructDef`.
pub fn collect_struct_defs(
    stmts: &[TStmt],
    field_order: &mut HashMap<String, Vec<String>>,
) {
    for stmt in stmts {
        match stmt {
            TStmt::StructDef { name, fields, .. } => {
                let names = fields.iter().map(|f| f.name().to_string()).collect();
                field_order.insert(name.clone(), names);
            }
            TStmt::ExtendBlock { methods, .. } => {
                collect_struct_defs(methods, field_order);
            }
            TStmt::AsyncFnDef { body, .. } => {
                collect_struct_defs(body, field_order);
            }
            _ => {}
        }
    }
}

/// Walk all expressions to find `StructLiteral` nodes and record their field
/// types.  This allows `emit_field_access` to correctly reinterpret i64 slots
/// even though the type-inferencer sets field-access results to `Unknown`.
pub fn collect_struct_literal_types(
    stmts: &[TStmt],
    field_types: &mut HashMap<String, HashMap<String, crate::compiler::tir::JadeType>>,
) {
    use crate::compiler::tir::{TExpr, TExprKind, TStmt};

    fn walk_expr(e: &TExpr, ft: &mut HashMap<String, HashMap<String, crate::compiler::tir::JadeType>>) {
        match &e.kind {
            TExprKind::Await { expr } => walk_expr(expr, ft),
            TExprKind::StructLiteral { type_name, fields } => {
                // Record field types first, then recurse into field expressions.
                let entries: Vec<(String, crate::compiler::tir::JadeType)> = fields
                    .iter()
                    .map(|(n, e, _)| (n.clone(), e.ty.clone()))
                    .collect();
                let map = ft.entry(type_name.clone()).or_default();
                for (n, ty) in entries {
                    map.entry(n).or_insert(ty);
                }
                for (_, field_expr, _) in fields {
                    walk_expr(field_expr, ft);
                }
            }
            TExprKind::Call { callee, args } => {
                walk_expr(callee, ft);
                for a in args { walk_expr(a, ft); }
            }
            TExprKind::BinOp { left, right, .. } => { walk_expr(left, ft); walk_expr(right, ft); }
            TExprKind::UnaryOp { operand, .. } => walk_expr(operand, ft),
            TExprKind::FieldAccess { object, .. } => walk_expr(object, ft),
            TExprKind::Index { object, index } => { walk_expr(object, ft); walk_expr(index, ft); }
            TExprKind::Array { elements } => { for e in elements { walk_expr(e, ft); } }
            TExprKind::FStr { parts } => {
                for p in parts {
                    if let crate::compiler::tir::TFStrPart::Expr(e) = p { walk_expr(e, ft); }
                }
            }
            TExprKind::Dict { entries } => {
                for (k, v) in entries { walk_expr(k, ft); walk_expr(v, ft); }
            }
            TExprKind::Closure { body, .. } => walk_stmts(body, ft),
            TExprKind::PromptDeref { expr, .. } => walk_expr(expr, ft),
            _ => {}
        }
    }

    fn walk_stmts(stmts: &[TStmt], ft: &mut HashMap<String, HashMap<String, crate::compiler::tir::JadeType>>) {
        for s in stmts { walk_stmt(s, ft); }
    }

    fn walk_stmt(s: &TStmt, ft: &mut HashMap<String, HashMap<String, crate::compiler::tir::JadeType>>) {
        match s {
            TStmt::Let { value, .. } | TStmt::Assign { value, .. }
            | TStmt::Return { value: Some(value), .. }
            | TStmt::Expr(value)
            | TStmt::PromptDecl { body: value, .. }
            | TStmt::Raise { value, .. } => walk_expr(value, ft),
            TStmt::FnDef { body, .. } | TStmt::AsyncFnDef { body, .. } => walk_stmts(body, ft),
            TStmt::If { condition, then_body, else_body, .. } => {
                walk_expr(condition, ft);
                walk_stmts(then_body, ft);
                if let Some(eb) = else_body { walk_stmts(eb, ft); }
            }
            TStmt::While { condition, body, .. } => {
                walk_expr(condition, ft);
                walk_stmts(body, ft);
            }
            TStmt::For { iterable, body, .. } => {
                walk_expr(iterable, ft);
                walk_stmts(body, ft);
            }
            TStmt::FieldAssign { value, .. } | TStmt::IndexAssign { value, .. } => walk_expr(value, ft),
            TStmt::ExtendBlock { methods, .. } => walk_stmts(methods, ft),
            TStmt::TryCatch { body, arms, .. } => {
                walk_stmts(body, ft);
                for arm in arms { walk_stmts(&arm.body, ft); }
            }
            _ => {}
        }
    }

    walk_stmts(stmts, field_types);
}

// ── First pass: declare all top-level function signatures ─────────────────────

/// Forward-declare every top-level `FnDef` so recursive / out-of-order calls
/// resolve correctly in the second pass.
pub fn declare_fns<'ctx>(ctx: &mut CodegenCtx<'ctx>, stmts: &[TStmt]) -> Result<(), String> {
    for stmt in stmts {
        match stmt {
            TStmt::FnDef { name, params, ret_ty, .. } => {
                let param_meta: Vec<_> = params
                    .iter()
                    .map(|_| types::jade_to_meta(&JadeType::Unknown, ctx.context))
                    .collect();
                let fn_ty = types::jade_fn_type(ret_ty, &param_meta, ctx.context);
                let fn_val = ctx.module.add_function(name, fn_ty, None);
                let param_jt: Vec<JadeType> = params.iter().map(|_| JadeType::Unknown).collect();
                ctx.fn_info.insert(name.clone(), (fn_val, param_jt, ret_ty.clone()));
            }
            TStmt::AsyncFnDef { name, params, ret_ty, .. } => {
                let i64_ty = ctx.context.i64_type();
                let i32_ty = ctx.context.i32_type();
                let ptr_ty = ctx.context.ptr_type(AddressSpace::default());

                // Body helper: i64 (ptr %args, i32 %n)
                let body_name = format!("{name}__async_body");
                let body_fn_ty = i64_ty.fn_type(&[ptr_ty.into(), i32_ty.into()], false);
                ctx.module.add_function(&body_name, body_fn_ty, None);

                // Public wrapper: ptr (i64, i64, ...) — returns a jade_future_t
                let param_meta: Vec<_> = params
                    .iter()
                    .map(|_| types::jade_to_meta(&JadeType::Unknown, ctx.context))
                    .collect();
                let wrapper_ret_ty = JadeType::Future(Box::new(ret_ty.clone()));
                let fn_ty = types::jade_fn_type(&wrapper_ret_ty, &param_meta, ctx.context);
                let fn_val = ctx.module.add_function(name, fn_ty, None);
                let param_jt: Vec<JadeType> = params.iter().map(|_| JadeType::Unknown).collect();
                ctx.fn_info.insert(name.clone(), (fn_val, param_jt, wrapper_ret_ty));
            }
            _ => {}
        }
    }
    Ok(())
}

// ── Second pass: emit statements ──────────────────────────────────────────────

pub fn emit_stmts<'ctx>(ctx: &mut CodegenCtx<'ctx>, stmts: &[TStmt]) -> Result<(), String> {
    for stmt in stmts {
        if ctx.is_terminated() {
            break;
        }
        emit_stmt(ctx, stmt)?;
    }
    Ok(())
}

pub fn emit_stmt<'ctx>(ctx: &mut CodegenCtx<'ctx>, stmt: &TStmt) -> Result<(), String> {
    match stmt {
        // ── let name = expr ───────────────────────────────────────────────────
        TStmt::Let { name, value, .. } => {
            let val = expr::emit_expr(value, ctx)?;
            let llvm_ty = types::jade_to_llvm(&value.ty, ctx.context);
            let ptr = ctx.build_entry_alloca(llvm_ty, name)?;
            ctx.builder
                .build_store(ptr, val)
                .map_err(|e| e.to_string())?;
            ctx.define(name.clone(), ptr, value.ty.clone());
        }

        // ── name = expr ───────────────────────────────────────────────────────
        TStmt::Assign { name, value, .. } => {
            let val = expr::emit_expr(value, ctx)?;
            let (ptr, _) = ctx
                .lookup(name)
                .ok_or_else(|| format!("assignment to undefined variable: {name}"))?;
            ctx.builder
                .build_store(ptr, val)
                .map_err(|e| e.to_string())?;
        }

        // ── fn definitions ────────────────────────────────────────────────────
        TStmt::FnDef { name, params, body, ret_ty, .. } => {
            emit_fn_body(ctx, name, params, body, ret_ty)?;
        }

        // ── async fn definitions ──────────────────────────────────────────────
        TStmt::AsyncFnDef { name, params, body, ret_ty, .. } => {
            emit_async_fn_def(ctx, name, params, body, ret_ty)?;
        }

        // ── return [expr] ─────────────────────────────────────────────────────
        TStmt::Return { value, .. } => {
            // Inside an async body function all values must be returned as i64
            // (jade_value_t).  async_body_ret_ty carries the original Jade type
            // needed to convert non-integer values correctly.
            let conv_ty = ctx.async_body_ret_ty.clone();
            match value {
                Some(e) => {
                    let val = expr::emit_expr(e, ctx)?;
                    let ret_val: inkwell::values::BasicValueEnum = if let Some(ref ty) = conv_ty {
                        expr::value_to_i64(val, ty, ctx)?.into()
                    } else {
                        val
                    };
                    ctx.builder
                        .build_return(Some(&ret_val))
                        .map_err(|e| e.to_string())?;
                }
                None => {
                    if conv_ty.is_some() {
                        let zero = ctx.context.i64_type().const_int(0, false);
                        ctx.builder.build_return(Some(&zero)).map_err(|e| e.to_string())?;
                    } else {
                        ctx.builder.build_return(None).map_err(|e| e.to_string())?;
                    }
                }
            }
        }

        // ── if condition { then } [else { else }] ─────────────────────────────
        TStmt::If { condition, then_body, else_body, .. } => {
            let fn_val = ctx
                .builder
                .get_insert_block()
                .and_then(|bb| bb.get_parent())
                .ok_or("if outside function")?;

            let cond = expr::emit_expr(condition, ctx)?.into_int_value();
            let then_bb  = ctx.context.append_basic_block(fn_val, "if_then");
            let else_bb  = ctx.context.append_basic_block(fn_val, "if_else");
            let merge_bb = ctx.context.append_basic_block(fn_val, "if_merge");

            ctx.builder.build_conditional_branch(cond, then_bb, else_bb).map_err(|e| e.to_string())?;

            ctx.builder.position_at_end(then_bb);
            ctx.push_scope();
            emit_stmts(ctx, then_body)?;
            ctx.pop_scope();
            if !ctx.is_terminated() {
                ctx.builder.build_unconditional_branch(merge_bb).map_err(|e| e.to_string())?;
            }

            ctx.builder.position_at_end(else_bb);
            if let Some(else_stmts) = else_body {
                ctx.push_scope();
                emit_stmts(ctx, else_stmts)?;
                ctx.pop_scope();
            }
            if !ctx.is_terminated() {
                ctx.builder.build_unconditional_branch(merge_bb).map_err(|e| e.to_string())?;
            }

            ctx.builder.position_at_end(merge_bb);
        }

        // ── while condition { body } ──────────────────────────────────────────
        TStmt::While { condition, body, .. } => {
            let fn_val = ctx
                .builder
                .get_insert_block()
                .and_then(|bb| bb.get_parent())
                .ok_or("while outside function")?;

            let cond_bb = ctx.context.append_basic_block(fn_val, "while_cond");
            let loop_bb = ctx.context.append_basic_block(fn_val, "while_body");
            let exit_bb = ctx.context.append_basic_block(fn_val, "while_exit");

            ctx.builder.build_unconditional_branch(cond_bb).map_err(|e| e.to_string())?;

            ctx.builder.position_at_end(cond_bb);
            let cond = expr::emit_expr(condition, ctx)?.into_int_value();
            ctx.builder.build_conditional_branch(cond, loop_bb, exit_bb).map_err(|e| e.to_string())?;

            ctx.builder.position_at_end(loop_bb);
            ctx.push_scope();
            emit_stmts(ctx, body)?;
            ctx.pop_scope();
            if !ctx.is_terminated() {
                ctx.builder.build_unconditional_branch(cond_bb).map_err(|e| e.to_string())?;
            }

            ctx.builder.position_at_end(exit_bb);
        }

        // ── for var in iterable { body } ──────────────────────────────────────
        TStmt::For { var, iterable, body, .. } => {
            let fn_val = ctx
                .builder
                .get_insert_block()
                .and_then(|bb| bb.get_parent())
                .ok_or("for outside function")?;

            let elem_ty = match &iterable.ty {
                JadeType::Array(inner) => *inner.clone(),
                _ => return Err(format!(
                    "for-loop iterable must be an array, got {:?}", iterable.ty
                )),
            };

            let i64_ty = ctx.context.i64_type();
            let ptr_ty = ctx.context.ptr_type(AddressSpace::default());

            // Emit the iterable
            let arr_ptr = expr::emit_expr(iterable, ctx)?.into_pointer_value();

            // Load len from field 1
            let f1 = ctx.builder
                .build_struct_gep(ctx.array_ty, arr_ptr, 1, "for_f1")
                .map_err(|e| e.to_string())?;
            let arr_len = ctx.builder
                .build_load(i64_ty, f1, "for_len")
                .map_err(|e| e.to_string())?
                .into_int_value();

            // Load data ptr from field 0
            let f0 = ctx.builder
                .build_struct_gep(ctx.array_ty, arr_ptr, 0, "for_f0")
                .map_err(|e| e.to_string())?;
            let data_ptr = ctx.builder
                .build_load(ptr_ty, f0, "for_data")
                .map_err(|e| e.to_string())?
                .into_pointer_value();

            // Loop counter + variable slot in entry block
            let loop_i   = ctx.build_entry_alloca(i64_ty.into(), "__for_i")?;
            let llvm_ety = types::jade_to_llvm(&elem_ty, ctx.context);
            let var_slot = ctx.build_entry_alloca(llvm_ety, var)?;

            ctx.builder
                .build_store(loop_i, i64_ty.const_int(0, false))
                .map_err(|e| e.to_string())?;

            let cond_bb = ctx.context.append_basic_block(fn_val, "for_cond");
            let body_bb = ctx.context.append_basic_block(fn_val, "for_body");
            let exit_bb = ctx.context.append_basic_block(fn_val, "for_exit");

            ctx.builder.build_unconditional_branch(cond_bb).map_err(|e| e.to_string())?;

            // Condition
            ctx.builder.position_at_end(cond_bb);
            let i = ctx.builder
                .build_load(i64_ty, loop_i, "for_i")
                .map_err(|e| e.to_string())?
                .into_int_value();
            let cond = ctx.builder
                .build_int_compare(IntPredicate::SLT, i, arr_len, "for_lt")
                .map_err(|e| e.to_string())?;
            ctx.builder.build_conditional_branch(cond, body_bb, exit_bb).map_err(|e| e.to_string())?;

            // Body
            ctx.builder.position_at_end(body_bb);
            let i = ctx.builder
                .build_load(i64_ty, loop_i, "for_i2")
                .map_err(|e| e.to_string())?
                .into_int_value();
            let slot = ctx.gep(i64_ty, data_ptr, &[i], "for_slot")?;
            let raw = ctx.builder
                .build_load(i64_ty, slot, "for_raw")
                .map_err(|e| e.to_string())?
                .into_int_value();
            let elem_val = expr::i64_to_value(raw, &elem_ty, ctx)?;

            ctx.builder.build_store(var_slot, elem_val).map_err(|e| e.to_string())?;

            ctx.push_scope();
            ctx.define(var.clone(), var_slot, elem_ty.clone());
            emit_stmts(ctx, body)?;
            ctx.pop_scope();

            if !ctx.is_terminated() {
                let i = ctx.builder
                    .build_load(i64_ty, loop_i, "for_i3")
                    .map_err(|e| e.to_string())?
                    .into_int_value();
                let next = ctx.builder
                    .build_int_add(i, i64_ty.const_int(1, false), "for_next")
                    .map_err(|e| e.to_string())?;
                ctx.builder.build_store(loop_i, next).map_err(|e| e.to_string())?;
                ctx.builder.build_unconditional_branch(cond_bb).map_err(|e| e.to_string())?;
            }

            ctx.builder.position_at_end(exit_bb);
        }

        // ── arr[idx] = value  /  dict[key] = value ───────────────────────────
        TStmt::IndexAssign { name, index, value, .. } => {
            let (arr_slot, arr_ty) = ctx
                .lookup(name)
                .ok_or_else(|| format!("undefined variable: {name}"))?;

            // Dict: d[key] = value  →  jade_dict_set(dict_ptr, key_ptr, value_i64)
            if matches!(arr_ty, JadeType::Dict) {
                let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
                let dict_ptr = ctx.builder
                    .build_load(ptr_ty, arr_slot, "ia_dict")
                    .map_err(|e| e.to_string())?
                    .into_pointer_value();
                let key_val = expr::emit_expr(index, ctx)?;
                let key_ptr = expr::as_pointer(key_val, ctx)?;
                let val = expr::emit_expr(value, ctx)?;
                let as_i64 = expr::value_to_i64(val, &value.ty, ctx)?;
                ctx.builder
                    .build_call(
                        ctx.jade_dict_set_fn,
                        &[dict_ptr.into(), key_ptr.into(), as_i64.into()],
                        "",
                    )
                    .map_err(|e| e.to_string())?;
                ctx.uses_dicts = true;
                return Ok(());
            }

            // Parameters with no type annotation are stored as Unknown.  Treat
            // them as an array of Unknown elements so index assignment still
            // works at runtime (the pointer bits are valid; only the TIR type
            // is missing).
            let elem_ty = match &arr_ty {
                JadeType::Array(inner) => *inner.clone(),
                JadeType::Unknown => JadeType::Unknown,
                _ => return Err(format!(
                    "index assignment on non-array type {:?}", arr_ty
                )),
            };

            let i64_ty = ctx.context.i64_type();
            let ptr_ty = ctx.context.ptr_type(AddressSpace::default());

            // Load the jade.array header pointer from the variable slot.
            //
            // When `arr` is a function parameter typed as Unknown, its alloca
            // was created for an i64 value (the LLVM function signature uses i64
            // for all Unknown params).  We must load as i64 and then cast to
            // pointer.  When the type is Array (a local `let arr = [...]`), the
            // alloca holds a pointer directly.
            let load_ty = match &arr_ty {
                JadeType::Unknown => types::jade_to_llvm(&JadeType::Int, ctx.context),
                _ => types::jade_to_llvm(&JadeType::Array(Box::new(JadeType::Unknown)), ctx.context),
            };
            let raw_load = ctx.builder
                .build_load(load_ty, arr_slot, "ia_arr_raw")
                .map_err(|e| e.to_string())?;
            let arr_ptr = expr::as_pointer(raw_load, ctx)?;

            let idx = expr::emit_expr(index, ctx)?.into_int_value();

            // Load data ptr
            let f0 = ctx.builder
                .build_struct_gep(ctx.array_ty, arr_ptr, 0, "ia_f0")
                .map_err(|e| e.to_string())?;
            let data_ptr = ctx.builder
                .build_load(ptr_ty, f0, "ia_data")
                .map_err(|e| e.to_string())?
                .into_pointer_value();

            let slot = ctx.gep(i64_ty, data_ptr, &[idx], "ia_slot")?;

            let val = expr::emit_expr(value, ctx)?;
            let as_i64 = expr::value_to_i64(val, &elem_ty, ctx)?;
            ctx.builder.build_store(slot, as_i64).map_err(|e| e.to_string())?;
        }

        // ── obj.field = value ─────────────────────────────────────────────────
        TStmt::FieldAssign { object, field, value, .. } => {
            let (obj_slot, obj_ty) = ctx
                .lookup(object)
                .ok_or_else(|| format!("undefined variable: {object}"))?;

            let type_name = match &obj_ty {
                JadeType::Struct(n) => n.clone(),
                _ => return Err(format!(
                    "field assignment on non-struct type {:?}", obj_ty
                )),
            };

            let field_names = ctx.struct_field_order
                .get(&type_name)
                .cloned()
                .ok_or_else(|| format!("unknown struct type: {type_name}"))?;

            let field_idx = field_names
                .iter()
                .position(|n| n == field)
                .ok_or_else(|| format!("unknown field '{field}' on struct '{type_name}'"))?;

            let i64_ty = ctx.context.i64_type();
            let ptr_ty = ctx.context.ptr_type(AddressSpace::default());

            // Load the struct pointer from the variable slot
            let struct_ptr = ctx.builder
                .build_load(ptr_ty, obj_slot, "fa_struct")
                .map_err(|e| e.to_string())?
                .into_pointer_value();

            // +1 to skip the type_name slot at slot 0
            let slot = ctx.gep(i64_ty, struct_ptr, &[i64_ty.const_int((field_idx as u64) + 1, false)], "fa_slot")?;

            let val = expr::emit_expr(value, ctx)?;
            let as_i64 = expr::value_to_i64(val, &value.ty, ctx)?;
            ctx.builder.build_store(slot, as_i64).map_err(|e| e.to_string())?;
        }

        // ── bare expression (side-effect call, etc.) ──────────────────────────
        TStmt::Expr(e) => {
            expr::emit_expr(e, ctx)?;
        }

        // ── Silently skip non-code definitions ────────────────────────────────
        TStmt::StructDef { .. }
        | TStmt::InterfaceDef { .. }
        | TStmt::ExtendBlock { .. } => {}

        // ── prompt p = expr ───────────────────────────────────────────────────
        // A prompt declaration is a string value with Prompt type.  Represented
        // identically to a Str pointer — the type distinction is only semantic.
        TStmt::PromptDecl { name, body, .. } => {
            let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
            let val = expr::emit_expr(body, ctx)?;
            let slot = ctx.build_entry_alloca(ptr_ty.into(), name)?;
            ctx.builder.build_store(slot, val).map_err(|e| e.to_string())?;
            ctx.define(name.clone(), slot, JadeType::Prompt);
        }

        // ── use "path" — expanded by resolve_imports before codegen ──────────
        TStmt::Use { .. } => {}

        // ── try { body } catch binding { arm } ────────────────────────────────
        TStmt::TryCatch { body, arms, .. } => {
            ctx.uses_exceptions = true;

            let fn_val = ctx.builder.get_insert_block()
                .and_then(|b| b.get_parent())
                .ok_or("try/catch outside function")?;

            // Stack-allocate a 256-byte jmpbuf (conservative for all x86_64 targets).
            let i8_ty  = ctx.context.i8_type();
            let buf_ty = i8_ty.array_type(256);
            let buf    = ctx.build_entry_alloca(buf_ty.into(), "exc_buf")?;

            // Register the frame, then setjmp.
            ctx.call_void(ctx.jade_exc_push_frame_fn, &[buf.into()])?;
            let r = ctx.call_rv(ctx.setjmp_fn, &[buf.into()], "exc_r")?
                .into_int_value();

            let try_bb   = ctx.context.append_basic_block(fn_val, "try_body");
            let catch_bb = ctx.context.append_basic_block(fn_val, "catch_body");
            let merge_bb = ctx.context.append_basic_block(fn_val, "exc_merge");

            let is_throw = ctx.builder
                .build_int_compare(IntPredicate::NE, r, ctx.context.i32_type().const_zero(), "is_throw")
                .map_err(|e| e.to_string())?;
            ctx.builder.build_conditional_branch(is_throw, catch_bb, try_bb)
                .map_err(|e| e.to_string())?;

            // Try body — pop the frame on clean exit.
            ctx.builder.position_at_end(try_bb);
            ctx.push_scope();
            emit_stmts(ctx, body)?;
            ctx.pop_scope();
            if !ctx.is_terminated() {
                ctx.call_void(ctx.jade_exc_pop_fn, &[])?;
                ctx.builder.build_unconditional_branch(merge_bb).map_err(|e| e.to_string())?;
            }

            // Catch body — retrieve thrown value, dispatch to typed arms.
            ctx.builder.position_at_end(catch_bb);
            let exc_i64 = ctx.call_rv(ctx.jade_exc_value_fn, &[], "exc_val")?.into_int_value();

            let i64_ty  = ctx.context.i64_type();
            let i32_ty  = ctx.context.i32_type();
            let ptr_ty  = ctx.context.ptr_type(AddressSpace::default());

            // Block to jump to when no arm matches (swallow the exception).
            let no_match_bb = ctx.context.append_basic_block(fn_val, "catch_nomatch");

            if arms.is_empty() {
                // Nothing to bind — swallow.
                ctx.builder.build_unconditional_branch(merge_bb).map_err(|e| e.to_string())?;
            }

            for (arm_idx, arm) in arms.iter().enumerate() {
                if ctx.is_terminated() { break; }
                let arm_body_bb = ctx.context.append_basic_block(fn_val, &format!("catch_arm{arm_idx}"));

                if let Some(ref expected_type) = arm.catch_type {
                    // Load the type_name stored at slot 0 of the raised struct.
                    let struct_ptr = ctx.builder
                        .build_int_to_ptr(exc_i64, ptr_ty, "exc_sptr")
                        .map_err(|e| e.to_string())?;
                    let ty_slot = ctx.gep(i64_ty, struct_ptr, &[i64_ty.const_int(0, false)], "exc_ty_slot")?;
                    let ty_i64  = ctx.builder
                        .build_load(i64_ty, ty_slot, "exc_ty_i64")
                        .map_err(|e| e.to_string())?
                        .into_int_value();
                    let ty_ptr  = ctx.builder
                        .build_int_to_ptr(ty_i64, ptr_ty, "exc_ty_ptr")
                        .map_err(|e| e.to_string())?;
                    let exp_ptr = ctx.builder
                        .build_global_string_ptr(expected_type, "exp_ty")
                        .map_err(|e| e.to_string())?
                        .as_pointer_value();
                    let cmp = ctx.call_rv(ctx.strcmp_fn, &[ty_ptr.into(), exp_ptr.into()], "ty_cmp")?
                        .into_int_value();
                    let is_match = ctx.builder
                        .build_int_compare(IntPredicate::EQ, cmp, i32_ty.const_zero(), "ty_match")
                        .map_err(|e| e.to_string())?;

                    let is_last = arm_idx + 1 >= arms.len();
                    let else_bb = if is_last {
                        no_match_bb
                    } else {
                        ctx.context.append_basic_block(fn_val, &format!("catch_check{}", arm_idx + 1))
                    };

                    ctx.builder.build_conditional_branch(is_match, arm_body_bb, else_bb)
                        .map_err(|e| e.to_string())?;

                    // Arm body: bind as struct pointer.
                    ctx.builder.position_at_end(arm_body_bb);
                    let slot = ctx.build_entry_alloca(ptr_ty.into(), &arm.binding)?;
                    let exc_ptr = ctx.builder
                        .build_int_to_ptr(exc_i64, ptr_ty, "exc_bind_ptr")
                        .map_err(|e| e.to_string())?;
                    ctx.builder.build_store(slot, exc_ptr).map_err(|e| e.to_string())?;
                    ctx.push_scope();
                    ctx.define(arm.binding.clone(), slot, JadeType::Struct(expected_type.clone()));
                    emit_stmts(ctx, &arm.body)?;
                    ctx.pop_scope();
                    if !ctx.is_terminated() {
                        ctx.builder.build_unconditional_branch(merge_bb).map_err(|e| e.to_string())?;
                    }

                    if !is_last {
                        ctx.builder.position_at_end(else_bb);
                    }
                } else {
                    // Catch-all arm: bind as raw i64, run body.
                    ctx.builder.build_unconditional_branch(arm_body_bb).map_err(|e| e.to_string())?;
                    ctx.builder.position_at_end(arm_body_bb);
                    let slot = ctx.build_entry_alloca(i64_ty.into(), &arm.binding)?;
                    ctx.builder.build_store(slot, exc_i64).map_err(|e| e.to_string())?;
                    ctx.push_scope();
                    ctx.define(arm.binding.clone(), slot, JadeType::Int);
                    emit_stmts(ctx, &arm.body)?;
                    ctx.pop_scope();
                    if !ctx.is_terminated() {
                        ctx.builder.build_unconditional_branch(merge_bb).map_err(|e| e.to_string())?;
                    }
                    break; // catch-all terminates the chain
                }
            }

            // no_match_bb: no typed arm matched — swallow and continue.
            ctx.builder.position_at_end(no_match_bb);
            if !ctx.is_terminated() {
                ctx.builder.build_unconditional_branch(merge_bb).map_err(|e| e.to_string())?;
            }

            ctx.builder.position_at_end(merge_bb);
        }

        // ── raise expr ────────────────────────────────────────────────────────
        TStmt::Raise { value, .. } => {
            ctx.uses_exceptions = true;
            let val = expr::emit_expr(value, ctx)?;
            // Coerce any value to i64 for the exception slot.
            let i64_val = match val {
                inkwell::values::BasicValueEnum::IntValue(v)   => v,
                inkwell::values::BasicValueEnum::FloatValue(v) =>
                    ctx.builder.build_bit_cast(v, ctx.context.i64_type(), "raise_bc")
                        .map_err(|e: inkwell::builder::BuilderError| e.to_string())?.into_int_value(),
                inkwell::values::BasicValueEnum::PointerValue(v) =>
                    ctx.builder.build_ptr_to_int(v, ctx.context.i64_type(), "raise_pi")
                        .map_err(|e| e.to_string())?,
                _ => ctx.context.i64_type().const_int(0, false),
            };
            ctx.call_void(ctx.jade_exc_throw_fn, &[i64_val.into()])?;
            ctx.builder.build_unreachable().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

// ── Async function emission ───────────────────────────────────────────────────

/// Emit both halves of an `async fn`:
///
/// 1. `<name>__async_body(ptr %args, i32 %n) -> i64`
///    The actual computation.  Parameters are loaded from the args array.
///    All return values are bit-cast to i64 (jade_value_t).
///
/// 2. `<name>(i64 arg0, ...) -> ptr`
///    The public wrapper.  Allocates an args array on the stack, fills it,
///    calls `jade_spawn(&<name>__async_body, args, n)`, and returns the
///    resulting `jade_future_t` pointer.
fn emit_async_fn_def<'ctx>(
    ctx: &mut CodegenCtx<'ctx>,
    name: &str,
    params: &[String],
    body: &[TStmt],
    ret_ty: &JadeType,
) -> Result<(), String> {
    use inkwell::values::BasicValueEnum;

    let i64_ty = ctx.context.i64_type();
    let i32_ty = ctx.context.i32_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());

    let restore_bb = ctx.builder.get_insert_block();

    // ── 1. Body function ─────────────────────────────────────────────────────
    let body_name = format!("{name}__async_body");
    let body_fn = ctx.module.get_function(&body_name)
        .ok_or_else(|| format!("async body '{body_name}' not declared in pass 1"))?;

    let body_entry = ctx.context.append_basic_block(body_fn, "entry");
    ctx.builder.position_at_end(body_entry);

    ctx.push_scope();
    let saved_ret   = ctx.current_ret_ty.take();
    let saved_async = ctx.async_body_ret_ty.take();
    ctx.current_ret_ty   = Some(ret_ty.clone());
    ctx.async_body_ret_ty = Some(ret_ty.clone());

    // Load each parameter from args[i] (uniform i64 slots).
    let args_ptr = body_fn.get_nth_param(0)
        .ok_or("async body: missing args param")?
        .into_pointer_value();

    for (i, param_name) in params.iter().enumerate() {
        let slot = ctx.gep(
            i64_ty,
            args_ptr,
            &[i64_ty.const_int(i as u64, false)],
            &format!("abp{i}_slot"),
        )?;
        let raw = ctx.builder
            .build_load(i64_ty, slot, &format!("abp{i}_raw"))
            .map_err(|e| e.to_string())?;
        let alloca = ctx.builder
            .build_alloca(i64_ty, param_name)
            .map_err(|e| e.to_string())?;
        ctx.builder.build_store(alloca, raw).map_err(|e| e.to_string())?;
        ctx.define(param_name.clone(), alloca, JadeType::Unknown);
    }

    emit_stmts(ctx, body)?;

    // Fall-through return → always i64 0 (nil) for async bodies.
    if !ctx.is_terminated() {
        ctx.builder
            .build_return(Some(&i64_ty.const_int(0, false)))
            .map_err(|e| e.to_string())?;
    }

    ctx.pop_scope();
    ctx.current_ret_ty   = saved_ret;
    ctx.async_body_ret_ty = saved_async;

    // ── 2. Wrapper function ──────────────────────────────────────────────────
    let wrapper_fn = ctx.module.get_function(name)
        .ok_or_else(|| format!("async wrapper '{name}' not declared in pass 1"))?;

    let wrapper_entry = ctx.context.append_basic_block(wrapper_fn, "entry");
    ctx.builder.position_at_end(wrapper_entry);

    let n = params.len();
    let alloc_size = i64_ty.const_int(n.max(1) as u64, false);

    // Stack-allocate `jade_value_t args[n]`.
    let args_arr = ctx.builder
        .build_array_alloca(i64_ty, alloc_size, "spawn_args")
        .map_err(|e| e.to_string())?;

    // Store each argument (always i64 in the wrapper signature) into args[i].
    for i in 0..n {
        let arg = wrapper_fn
            .get_nth_param(i as u32)
            .ok_or_else(|| format!("param {i} out of range for async wrapper '{name}'"))?;
        // Wrapper params are Unknown → i64; coerce wider types to i64.
        let as_i64: inkwell::values::IntValue<'ctx> = match arg {
            BasicValueEnum::IntValue(v) => {
                if v.get_type().get_bit_width() < 64 {
                    ctx.builder
                        .build_int_z_extend(v, i64_ty, &format!("sw{i}_ext"))
                        .map_err(|e| e.to_string())?
                } else {
                    v
                }
            }
            BasicValueEnum::FloatValue(f) => ctx.float_to_i64_bits(f)?,
            BasicValueEnum::PointerValue(p) => ctx.builder
                .build_ptr_to_int(p, i64_ty, &format!("sw{i}_p2i"))
                .map_err(|e| e.to_string())?,
            other => {
                return Err(format!("unsupported arg type in async wrapper: {other:?}"));
            }
        };
        let slot = ctx.gep(
            i64_ty,
            args_arr,
            &[i64_ty.const_int(i as u64, false)],
            &format!("sw{i}_slot"),
        )?;
        ctx.builder.build_store(slot, as_i64).map_err(|e| e.to_string())?;
    }

    // jade_spawn(&body_fn, args_arr, n)
    let body_fn_ptr = body_fn.as_global_value().as_pointer_value();
    let n_val = i32_ty.const_int(n as u64, false);
    let future_ptr = ctx
        .call_rv(
            ctx.jade_spawn_fn,
            &[body_fn_ptr.into(), args_arr.into(), n_val.into()],
            "future",
        )?
        .into_pointer_value();

    ctx.builder
        .build_return(Some(&future_ptr))
        .map_err(|e| e.to_string())?;

    ctx.uses_async = true;

    if let Some(bb) = restore_bb {
        ctx.builder.position_at_end(bb);
    }

    Ok(())
}

// ── Function body emission ────────────────────────────────────────────────────

fn emit_fn_body<'ctx>(
    ctx: &mut CodegenCtx<'ctx>,
    name: &str,
    params: &[String],
    body: &[TStmt],
    ret_ty: &JadeType,
) -> Result<(), String> {
    let (fn_val, param_tys, _) = ctx
        .fn_info
        .get(name)
        .ok_or_else(|| format!("function '{name}' not declared in pass 1"))?
        .clone();

    let restore_bb = ctx.builder.get_insert_block();

    let entry_bb = ctx.context.append_basic_block(fn_val, "entry");
    ctx.builder.position_at_end(entry_bb);

    ctx.push_scope();
    let saved_ret = ctx.current_ret_ty.take();
    ctx.current_ret_ty = Some(ret_ty.clone());

    // Allocate one slot per parameter in the entry block and store the arg.
    for (i, param_name) in params.iter().enumerate() {
        let jt = param_tys.get(i).cloned().unwrap_or(JadeType::Unknown);
        let llvm_ty = types::jade_to_llvm(&jt, ctx.context);
        let ptr = ctx
            .builder
            .build_alloca(llvm_ty, param_name)
            .map_err(|e| e.to_string())?;
        let arg_val = fn_val
            .get_nth_param(i as u32)
            .ok_or_else(|| format!("param {i} out of range for '{name}'"))?;
        ctx.builder.build_store(ptr, arg_val).map_err(|e| e.to_string())?;
        ctx.define(param_name.clone(), ptr, jt);
    }

    emit_stmts(ctx, body)?;

    // If the body didn't terminate, add a default return.
    if !ctx.is_terminated() {
        match ret_ty {
            JadeType::Nil => {
                ctx.builder.build_return(None).map_err(|e| e.to_string())?;
            }
            _ => {
                let zero = ctx.context.i64_type().const_int(0, false);
                ctx.builder.build_return(Some(&zero)).map_err(|e| e.to_string())?;
            }
        }
    }

    ctx.pop_scope();
    ctx.current_ret_ty = saved_ret;

    if let Some(bb) = restore_bb {
        ctx.builder.position_at_end(bb);
    }

    Ok(())
}
