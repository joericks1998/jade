//! The per-instruction lowering match.
//!
//! See this directory's README.

use super::*;

/// Core builtin names that have a callable *value*, not just a call lowering.
///
/// Keep in step with `jrt_builtin_value` in `runtime_aot/common.c`, which holds
/// the box for each. A name here with no box there reads as nil; a box there
/// with no name here is never reached.
pub(super) const BUILTIN_VALUES: &[&str] =
    &["len", "str", "int", "float", "bool", "char", "print", "write"];

/// Core names this backend has no value for, so reading one declines the build
/// rather than quietly loading an empty global cell.
///
/// `func` looks a function up by name and `input` reads a line; this backend
/// lowers neither as a *call* either, so declining the value form too is the
/// consistent answer.
///
/// `Grammar` is deliberately absent even though reading it also yields nil:
/// `Grammar.new(…)` reaches the namespace through this very instruction, so
/// refusing it here would refuse a feature that works. `route` likewise.
const BUILTIN_NO_VALUE: &[&str] = &["func", "input"];

/// Lower one instruction. Returns `Ok(true)` if it emitted a block terminator
/// (`Return`/`Jump`/conditional jump), `Ok(false)` otherwise.
/// Everything about the body being lowered that an instruction may need.
///
/// All of it is computed once, before the first instruction, and read by many —
/// so it travelled as six separate parameters through a function that already
/// had four of its own. One value says what it is: the shape of this body.
pub(super) struct BodyCtx<'a, 'ctx> {
    pub llblocks: &'a [LlvmBlock<'ctx>],
    pub graph: &'a cfg::Cfg,
    pub handler_bufs: &'a HashMap<usize, PointerValue<'ctx>>,
    /// Per instruction: is a handler *of this function* active here? A `raise`
    /// inside one stays in this frame; a `raise` outside every one is leaving.
    /// See the `Raise` arm.
    pub in_handler: &'a [bool],
    pub call_builtins: &'a HashMap<usize, BuiltinCall>,
    pub user_calls: &'a HashMap<usize, CallKind>,
    pub fn_defs: &'a [Arc<CompiledFn>],
    pub fnctx: &'a FnCtx<'ctx>,
}

pub(super) fn lower_instr<'ctx>(
    low: &Lowerer<'_, 'ctx>,
    instr: &Instr,
    idx: usize,
    body: &BodyCtx<'_, 'ctx>,
) -> Result<bool, String> {
    let BodyCtx {
        llblocks,
        graph,
        handler_bufs,
        in_handler,
        call_builtins,
        user_calls,
        fn_defs,
        fnctx,
    } = *body;
    use Instr::*;
    let b = low.builder;
    let i64_ty = low.i64t();
    // PC-relative jump target (see cfg / bytecode.rs::patch_jump).
    let target = |off: i32| -> usize { (idx as i64 + 1 + off as i64) as usize };
    let block_of = |instr_idx: usize| -> LlvmBlock<'ctx> { llblocks[graph.block_at[&instr_idx]] };

    match instr {
        // ── Constant loads ────────────────────────────────────────────────
        LoadInt(d, v) => {
            // Tagged int = v << 1. One bit goes to the tag, so a word holds 63
            // bits, not 64 — and this used to just truncate, so
            // `print(9223372036854775807)` compiled to `-1`: a wrong answer, not
            // an error. The magnitude is known here, so refuse it at build time
            // rather than emitting something that silently computes garbage.
            const INT_MAX: i64 = (1 << 62) - 1;
            const INT_MIN: i64 = -(1 << 62);
            if *v > INT_MAX || *v < INT_MIN {
                return Err(format!(
                    "integer overflow: {v} does not fit a Jade integer \
                     (the compiled representation holds {INT_MIN}..={INT_MAX})"
                ));
            }
            let tagged = (*v).wrapping_shl(1) as u64;
            low.store(*d, i64_ty.const_int(tagged, false));
            Ok(false)
        }
        LoadBool(d, v) => {
            low.store(*d, i64_ty.const_int(if *v { TRUE } else { FALSE }, false));
            Ok(false)
        }
        LoadNil(d) => {
            low.store(*d, i64_ty.const_int(NIL, false));
            Ok(false)
        }

        // ── Move ──────────────────────────────────────────────────────────
        Move(d, s) => {
            let v = low.load(*s);
            low.retain(v); // borrowed alias → the dest slot becomes a new owner
            low.store(*d, v);
            Ok(false)
        }

        // ── Variable access ───────────────────────────────────────────────
        // Locals share the register slot array (see VM GetLocal/SetLocal), so
        // these are moves between slot indices.
        GetLocal(d, slot) => {
            let v = low.load_idx(*slot as usize);
            low.retain(v); // borrowed alias
            low.store(*d, v);
            Ok(false)
        }
        SetLocal(slot, s) => {
            let v = low.load(*s);
            low.retain(v); // borrowed alias into the target local
            low.store_idx(*slot as usize, v);
            Ok(false)
        }
        // Module-scoped globals: load/store the named LLVM global cell.
        GetGlobal(d, name) => {
            // A native package reference used as a value materializes a first-class
            // native function value (a jade_fn sentinel); an ordinary global loads
            // its cell.
            //
            // This used to claim the materialized value is dead-code-eliminated
            // when the reference is immediately called. It is not: the `Call`
            // does devirtualize, but the store just below puts the tagged word
            // in the register-file alloca, and a word that is dead to Jade is
            // still live to LLVM. Whatever this emits stays in the loop body
            // beside the call, which is why `emit_native_fn_value` must not
            // allocate — when it did, every FFI call leaked its box.
            let v = if let Some((pkgid, fname)) = parse_native_ref(name) {
                emit_native_fn_value(low, pkgid, fname)?
            } else if BUILTIN_VALUES.contains(&name.as_str())
                && !fnctx.user_globals.contains(name.as_str())
            {
                // A core builtin read as a value rather than called: `let f = len`,
                // `xs.map(str)`. There is no global cell for one, so this used to
                // load nil. `jrt_builtin_value` hands back the static box for it.
                // Skipped when the program assigns the name itself, since then it
                // really is an ordinary global.
                let f = low
                    .runtime_fn("jrt_builtin_value", i64_ty.fn_type(&[low.ptrt().into()], false));
                b.build_call(f, &[low.cstr(name).into()], "bival")
                    .map_err(|e| e.to_string())?
                    .as_any_value_enum()
                    .into_int_value()
            } else if BUILTIN_NO_VALUE.contains(&name.as_str())
                && !fnctx.user_globals.contains(name.as_str())
            {
                // A core name this backend has no value for. Calling one already
                // declines the build ("unsupported builtin call"), so reading one
                // has to as well — otherwise it loads the empty global cell and
                // the program runs with `nil` where the interpreter had a
                // function, which is the quietest way to be wrong.
                return Err(format!("codegen: unsupported builtin value `{name}`"));
            } else {
                let g = low.global_slot(name);
                b.build_load(i64_ty, g, "gld").map_err(|e| e.to_string())?.into_int_value()
            };
            // Borrowed from the global cell → the dest slot becomes a new owner.
            // On the native path this is a no-op whatever the mode, because
            // `jrt_incref` is gated on the kind and a fn box is not a collection
            // — the same gate that makes a shared, immutable, statically
            // allocated fn value safe to hand out.
            low.retain(v);
            low.store(*d, v);
            Ok(false)
        }
        SetGlobal(name, s) => {
            let v = low.load(*s);
            let g = low.global_slot(name);
            // The global cell becomes a new owner: retain the value, and release
            // whatever the cell held before (globals are never scope-exit-released,
            // so their final value intentionally lives until process end).
            low.retain(v);
            let old = b.build_load(i64_ty, g, "gold").map_err(|e| e.to_string())?.into_int_value();
            let f = low.runtime_fn(
                "jrt_rc_replace",
                low.ctx.void_type().fn_type(&[i64_ty.into(), i64_ty.into()], false),
            );
            b.build_call(f, &[old.into(), v.into()], "").map_err(|e| e.to_string())?;
            b.build_store(g, v).map_err(|e| e.to_string())?;
            Ok(false)
        }

        // ── Integer arithmetic (native op on untagged, then re-tag) ───────
        // Overflow-checked, matching the VM's checked_add/sub/mul. See
        // `checked_int_result` for why the arithmetic widens to i128 first.
        AddInt(d, l, r) => {
            let (a, c) = low.int_operands(*l, *r);
            let s =
                b.build_int_add(low.widen(a)?, low.widen(c)?, "addi").map_err(|e| e.to_string())?;
            let res = low.checked_int_result(s, "addi")?;
            low.store(*d, res);
            Ok(false)
        }
        SubInt(d, l, r) => {
            let (a, c) = low.int_operands(*l, *r);
            let s =
                b.build_int_sub(low.widen(a)?, low.widen(c)?, "subi").map_err(|e| e.to_string())?;
            let res = low.checked_int_result(s, "subi")?;
            low.store(*d, res);
            Ok(false)
        }
        MulInt(d, l, r) => {
            let (a, c) = low.int_operands(*l, *r);
            let s =
                b.build_int_mul(low.widen(a)?, low.widen(c)?, "muli").map_err(|e| e.to_string())?;
            let res = low.checked_int_result(s, "muli")?;
            low.store(*d, res);
            Ok(false)
        }
        NegInt(d, s) => {
            let a = low.untag_int(low.load(*s));
            let n = b.build_int_neg(low.widen(a)?, "negi").map_err(|e| e.to_string())?;
            let res = low.checked_int_result(n, "negi")?;
            low.store(*d, res);
            Ok(false)
        }
        DivInt(d, l, r) => {
            let res = low.int_div_mod(*l, *r, false)?;
            low.store(*d, res);
            Ok(false)
        }
        ModInt(d, l, r) => {
            let res = low.int_div_mod(*l, *r, true)?;
            low.store(*d, res);
            Ok(false)
        }

        // ── Float loads / arithmetic (unbox, native op, re-box) ───────────
        LoadFloat(d, v) => {
            let c = i64_ty.const_int(v.to_bits(), false);
            let f = b
                .build_bit_cast(c, low.f64t(), "fbits")
                .map_err(|e| e.to_string())?
                .into_float_value();
            low.store(*d, low.box_float(f));
            Ok(false)
        }
        AddFloat(d, l, r) => {
            let (a, c) = low.float_operands(*l, *r);
            let s = b.build_float_add(a, c, "addf").map_err(|e| e.to_string())?;
            low.store(*d, low.box_float(s));
            Ok(false)
        }
        SubFloat(d, l, r) => {
            let (a, c) = low.float_operands(*l, *r);
            let s = b.build_float_sub(a, c, "subf").map_err(|e| e.to_string())?;
            low.store(*d, low.box_float(s));
            Ok(false)
        }
        MulFloat(d, l, r) => {
            let (a, c) = low.float_operands(*l, *r);
            let s = b.build_float_mul(a, c, "mulf").map_err(|e| e.to_string())?;
            low.store(*d, low.box_float(s));
            Ok(false)
        }
        DivFloat(d, l, r) => {
            let res = low.float_div(*l, *r)?;
            low.store(*d, res);
            Ok(false)
        }
        NegFloat(d, s) => {
            let a = low.unbox_float(low.load(*s));
            let n = b.build_float_neg(a, "negf").map_err(|e| e.to_string())?;
            low.store(*d, low.box_float(n));
            Ok(false)
        }
        IntToFloat(d, s) => {
            let i = low.untag_int(low.load(*s));
            let f = b.build_signed_int_to_float(i, low.f64t(), "i2f").map_err(|e| e.to_string())?;
            low.store(*d, low.box_float(f));
            Ok(false)
        }

        // ── Typed comparisons → bool word (native icmp/fcmp) ──────────────
        CmpEqInt(d, l, r) => {
            low.store(*d, low.int_cmp(*l, *r, IntPredicate::EQ));
            Ok(false)
        }
        CmpNeInt(d, l, r) => {
            low.store(*d, low.int_cmp(*l, *r, IntPredicate::NE));
            Ok(false)
        }
        CmpLtInt(d, l, r) => {
            low.store(*d, low.int_cmp(*l, *r, IntPredicate::SLT));
            Ok(false)
        }
        CmpGtInt(d, l, r) => {
            low.store(*d, low.int_cmp(*l, *r, IntPredicate::SGT));
            Ok(false)
        }
        CmpLeInt(d, l, r) => {
            low.store(*d, low.int_cmp(*l, *r, IntPredicate::SLE));
            Ok(false)
        }
        CmpGeInt(d, l, r) => {
            low.store(*d, low.int_cmp(*l, *r, IntPredicate::SGE));
            Ok(false)
        }

        CmpEqFloat(d, l, r) => {
            low.store(*d, low.float_cmp(*l, *r, FloatPredicate::OEQ));
            Ok(false)
        }
        CmpNeFloat(d, l, r) => {
            low.store(*d, low.float_cmp(*l, *r, FloatPredicate::UNE));
            Ok(false)
        }
        CmpLtFloat(d, l, r) => {
            low.store(*d, low.float_cmp(*l, *r, FloatPredicate::OLT));
            Ok(false)
        }
        CmpGtFloat(d, l, r) => {
            low.store(*d, low.float_cmp(*l, *r, FloatPredicate::OGT));
            Ok(false)
        }
        CmpLeFloat(d, l, r) => {
            low.store(*d, low.float_cmp(*l, *r, FloatPredicate::OLE));
            Ok(false)
        }
        CmpGeFloat(d, l, r) => {
            low.store(*d, low.float_cmp(*l, *r, FloatPredicate::OGE));
            Ok(false)
        }

        CmpEqBool(d, l, r) => {
            low.store(*d, low.bool_cmp(*l, *r, IntPredicate::EQ));
            Ok(false)
        }
        CmpNeBool(d, l, r) => {
            low.store(*d, low.bool_cmp(*l, *r, IntPredicate::NE));
            Ok(false)
        }
        CmpLtBool(d, l, r) => {
            low.store(*d, low.bool_cmp(*l, *r, IntPredicate::ULT));
            Ok(false)
        }
        CmpGtBool(d, l, r) => {
            low.store(*d, low.bool_cmp(*l, *r, IntPredicate::UGT));
            Ok(false)
        }
        CmpLeBool(d, l, r) => {
            low.store(*d, low.bool_cmp(*l, *r, IntPredicate::ULE));
            Ok(false)
        }
        CmpGeBool(d, l, r) => {
            low.store(*d, low.bool_cmp(*l, *r, IntPredicate::UGE));
            Ok(false)
        }

        CmpLtIntFloat(d, l, r) => {
            low.store(*d, low.mixed_cmp(*l, true, *r, false, FloatPredicate::OLT));
            Ok(false)
        }
        CmpGtIntFloat(d, l, r) => {
            low.store(*d, low.mixed_cmp(*l, true, *r, false, FloatPredicate::OGT));
            Ok(false)
        }
        CmpLeIntFloat(d, l, r) => {
            low.store(*d, low.mixed_cmp(*l, true, *r, false, FloatPredicate::OLE));
            Ok(false)
        }
        CmpGeIntFloat(d, l, r) => {
            low.store(*d, low.mixed_cmp(*l, true, *r, false, FloatPredicate::OGE));
            Ok(false)
        }
        CmpLtFloatInt(d, l, r) => {
            low.store(*d, low.mixed_cmp(*l, false, *r, true, FloatPredicate::OLT));
            Ok(false)
        }
        CmpGtFloatInt(d, l, r) => {
            low.store(*d, low.mixed_cmp(*l, false, *r, true, FloatPredicate::OGT));
            Ok(false)
        }
        CmpLeFloatInt(d, l, r) => {
            low.store(*d, low.mixed_cmp(*l, false, *r, true, FloatPredicate::OLE));
            Ok(false)
        }
        CmpGeFloatInt(d, l, r) => {
            low.store(*d, low.mixed_cmp(*l, false, *r, true, FloatPredicate::OGE));
            Ok(false)
        }

        // ── Logical / bitwise (integers) ──────────────────────────────────
        Not(d, s) => {
            let b1 = low.untag_bool(low.load(*s));
            let n = b.build_not(b1, "lnot").map_err(|e| e.to_string())?;
            low.store(*d, low.bool_word(n));
            Ok(false)
        }
        BitAnd(d, l, r) => {
            low.store(
                *d,
                low.int_bitop(*l, *r, |a, c| b.build_and(a, c, "band").map_err(|e| e.to_string()))?,
            );
            Ok(false)
        }
        BitOr(d, l, r) => {
            low.store(
                *d,
                low.int_bitop(*l, *r, |a, c| b.build_or(a, c, "bor").map_err(|e| e.to_string()))?,
            );
            Ok(false)
        }
        BitXor(d, l, r) => {
            low.store(
                *d,
                low.int_bitop(*l, *r, |a, c| b.build_xor(a, c, "bxor").map_err(|e| e.to_string()))?,
            );
            Ok(false)
        }
        Shl(d, l, r) => {
            low.store(
                *d,
                low.int_bitop(*l, *r, |a, c| {
                    b.build_left_shift(a, c, "shl").map_err(|e| e.to_string())
                })?,
            );
            Ok(false)
        }
        Shr(d, l, r) => {
            low.store(
                *d,
                low.int_bitop(*l, *r, |a, c| {
                    b.build_right_shift(a, c, true, "shr").map_err(|e| e.to_string())
                })?,
            );
            Ok(false)
        }
        BitNot(d, s) => {
            let a = low.untag_int(low.load(*s));
            let n = b.build_not(a, "bnot").map_err(|e| e.to_string())?;
            low.store(*d, low.tag_int(n));
            Ok(false)
        }

        // ── Dynamic ops (Unknown-typed operands) → jrt_*_any (A7) ─────────
        // Emitted when the type-inferrer can't specialize (notably every
        // function-parameter use, since Jade params are untyped). Routes to the
        // same tag-dispatching decision core the VM runs.
        BinOp(d, op, l, r) => {
            use BinOpKind::*;
            match op {
                Add => {
                    let v = low.any2_int_fast("jrt_add_any", *l, *r)?;
                    low.store(*d, v);
                }
                Sub => {
                    let v = low.any2_int_fast("jrt_sub_any", *l, *r)?;
                    low.store(*d, v);
                }
                Mul => {
                    let v = low.any2_int_fast("jrt_mul_any", *l, *r)?;
                    low.store(*d, v);
                }
                Div => low.store(*d, low.any2("jrt_div_any", *l, *r)),
                Mod => low.store(*d, low.any2("jrt_mod_any", *l, *r)),
                // Bitwise/shift are int-only: untag, native op, re-tag.
                BitAnd => low.store(
                    *d,
                    low.int_bitop(*l, *r, |a, c| {
                        b.build_and(a, c, "band").map_err(|e| e.to_string())
                    })?,
                ),
                BitOr => low.store(
                    *d,
                    low.int_bitop(*l, *r, |a, c| {
                        b.build_or(a, c, "bor").map_err(|e| e.to_string())
                    })?,
                ),
                BitXor => low.store(
                    *d,
                    low.int_bitop(*l, *r, |a, c| {
                        b.build_xor(a, c, "bxor").map_err(|e| e.to_string())
                    })?,
                ),
                Shl => low.store(
                    *d,
                    low.int_bitop(*l, *r, |a, c| {
                        b.build_left_shift(a, c, "shl").map_err(|e| e.to_string())
                    })?,
                ),
                Shr => low.store(
                    *d,
                    low.int_bitop(*l, *r, |a, c| {
                        b.build_right_shift(a, c, true, "shr").map_err(|e| e.to_string())
                    })?,
                ),
                // `x in y` / `x not in y` — runtime containment (substring / array
                // element / dict key), producing a bool word.
                In | NotIn => {
                    let f = low.runtime_fn(
                        "jrt_in_any",
                        low.ctx.i32_type().fn_type(&[i64_ty.into(), i64_ty.into()], false),
                    );
                    let present = b
                        .build_call(f, &[low.load(*l).into(), low.load(*r).into()], "inany")
                        .map_err(|e| e.to_string())?
                        .as_any_value_enum()
                        .into_int_value();
                    let pred = if matches!(op, In) { IntPredicate::NE } else { IntPredicate::EQ };
                    low.store(*d, low.i32cmp_word(present, pred));
                }
                // And/Or are emitted as short-circuit jumps, never a BinOp opcode.
                _ => return Err(format!("codegen: unsupported dynamic BinOp {op:?}")),
            }
            Ok(false)
        }
        UnaryOp(d, op, s) => {
            match op {
                UnaryOpKind::Neg => low.store(*d, low.neg_any(*s)),
                UnaryOpKind::Not => {
                    // Untag the (tagged) bool operand, invert, re-wrap as a word.
                    let b1 = low.untag_bool(low.load(*s));
                    let n = b.build_not(b1, "lnotd").map_err(|e| e.to_string())?;
                    low.store(*d, low.bool_word(n));
                }
                UnaryOpKind::BitNot => {
                    let a = low.untag_int(low.load(*s));
                    let n = b.build_not(a, "bnotd").map_err(|e| e.to_string())?;
                    low.store(*d, low.tag_int(n));
                }
            }
            Ok(false)
        }
        // Dynamic equality/ordering → bool word (mirror expr.rs emit_binop_any).
        CmpEq(d, l, r) => {
            let e = low.eq_any(*l, *r);
            low.store(*d, low.i32cmp_word(e, IntPredicate::NE));
            Ok(false)
        }
        CmpNe(d, l, r) => {
            let e = low.eq_any(*l, *r);
            low.store(*d, low.i32cmp_word(e, IntPredicate::EQ));
            Ok(false)
        }
        CmpLt(d, l, r) => {
            let c = low.cmp_any(*l, *r, "'<'");
            low.store(*d, low.i32cmp_word(c, IntPredicate::SLT));
            Ok(false)
        }
        CmpGt(d, l, r) => {
            let c = low.cmp_any(*l, *r, "'>'");
            low.store(*d, low.i32cmp_word(c, IntPredicate::SGT));
            Ok(false)
        }
        CmpLe(d, l, r) => {
            let c = low.cmp_any(*l, *r, "'<='");
            low.store(*d, low.i32cmp_word(c, IntPredicate::SLE));
            Ok(false)
        }
        CmpGe(d, l, r) => {
            let c = low.cmp_any(*l, *r, "'>='");
            low.store(*d, low.i32cmp_word(c, IntPredicate::SGE));
            Ok(false)
        }

        // ── Strings (pre-tagged literal globals; runtime concat/compare) ──
        LoadStr(d, s) => {
            let ptr = low.str_literal_ptr(s)?;
            low.store(*d, low.tag_str(ptr));
            Ok(false)
        }
        ConcatStr(d, l, r) => {
            low.store(*d, low.str_concat(*l, *r));
            Ok(false)
        }
        // A typed `catch` arm matches the named type or anything that inherits
        // it, so this is not a string compare. The ancestry lives in the runtime
        // registry, filled by `jrt_struct_ancestor` calls emitted at program
        // start, because a compiled binary has no compiler left to ask.
        CatchMatches(d, actual, expected) => {
            let f = low.runtime_fn(
                "jrt_catch_matches",
                low.ctx.i32_type().fn_type(&[low.ptrt().into(), low.ptrt().into()], false),
            );
            let got = low.untag_ptr(low.load(*actual));
            let want = low.cstr(expected);
            let r = low
                .builder
                .build_call(f, &[got.into(), want.into()], "catchmatch")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_int_value();
            low.store(*d, low.i32cmp_word(r, IntPredicate::NE));
            Ok(false)
        }
        CmpEqStr(d, l, r) => {
            low.store(*d, low.str_cmp(*l, *r, IntPredicate::EQ));
            Ok(false)
        }
        CmpNeStr(d, l, r) => {
            low.store(*d, low.str_cmp(*l, *r, IntPredicate::NE));
            Ok(false)
        }
        CmpLtStr(d, l, r) => {
            low.store(*d, low.str_cmp(*l, *r, IntPredicate::SLT));
            Ok(false)
        }
        CmpGtStr(d, l, r) => {
            low.store(*d, low.str_cmp(*l, *r, IntPredicate::SGT));
            Ok(false)
        }
        CmpLeStr(d, l, r) => {
            low.store(*d, low.str_cmp(*l, *r, IntPredicate::SLE));
            Ok(false)
        }
        CmpGeStr(d, l, r) => {
            low.store(*d, low.str_cmp(*l, *r, IntPredicate::SGE));
            Ok(false)
        }

        // ── f-strings ─────────────────────────────────────────────────────
        // Fold the parts left-to-right with `jrt_str_concat` (trust = max):
        // each part is a tagged-string data pointer — a compile-time literal
        // global, or `jrt_str_of_any(value)` for an interpolated register. An
        // empty template yields the empty string.
        BuildFStr(d, parts) => {
            // Ownership is per part and uniform now. A literal is a `constant`
            // global — borrowed, immortal, never freed. An interpolated register
            // goes through `jrt_str_of_any`, which allocates: it used to hand
            // back the caller's own pointer when the value was already a string,
            // so whether the result was owned depended on the value's *type*,
            // and a fold over parts of both kinds had to get one of them wrong.
            //
            // It got both. `f"{x}"` on a string stored that pointer as a second
            // owner — a double free once strings became reference-counted — and
            // `f"{a}-{i}"` leaked the fresh string rendered for the int. Neither
            // was visible before 1.3.16, when nothing was ever released.
            //
            // So each part says whether it is owned, and every owned pointer is
            // released exactly where it is consumed.
            let mut acc: Option<(PointerValue, bool)> = None;
            for part in parts {
                let (p_ptr, p_owned) = match part {
                    FStrPart::Literal(s) => (low.str_literal_ptr(s)?, false),
                    FStrPart::Reg(r) => (low.str_of_any(*r), true),
                };
                acc = Some(match acc {
                    None => (p_ptr, p_owned),
                    Some((prev, prev_owned)) => {
                        let joined = low.concat_ptrs(prev, p_ptr);
                        if prev_owned {
                            low.free_str_ptr(prev);
                        }
                        if p_owned {
                            low.free_str_ptr(p_ptr);
                        }
                        (joined, true)
                    }
                });
            }
            let ptr = match acc {
                Some((p, true)) => p,
                // The only way to be here is a template that is one literal and
                // nothing else. A literal global is immortal, so storing it is
                // what `LoadStr` does anyway — no copy needed.
                Some((p, false)) => p,
                None => low.str_literal_ptr("")?,
            };
            low.store(*d, low.tag_str(ptr));
            Ok(false)
        }

        // ── Collections: kind-tagged arrays (A8) ──────────────────────────
        // MakeArray: allocate a kind-tagged array, push each element (a tagged
        // word), tag the pointer as a heap object. GetIndex/SetIndex dispatch on
        // the object's tag/kind in the runtime (string/array; dict is a later
        // sub-brick). len/print/str/f-strings are already collection-aware via
        // jrt_len_unknown / jrt_render_any.
        // A heap array (kind-tagged, reference-counted) OR — for MakeArrayArena —
        // an arena array (marked ARENA by the constructor, so incref/decref no-op
        // on it and only ArenaReset frees it). The arena constructor stores
        // elements without a retain; that is sound because the escape analysis
        // only marks arrays of immediate scalars, which carry no heap ownership.
        MakeArray(d, regs) | MakeArrayArena(d, regs) => {
            let (new_name, push_name) = match instr {
                MakeArrayArena(..) => ("jrt_karr_new_arena", "jrt_karr_push_arena"),
                _ => ("jrt_karr_new", "jrt_karr_push"),
            };
            let new_f = low.runtime_fn(new_name, low.ptrt().fn_type(&[], false));
            let arr = b
                .build_call(new_f, &[], "karr")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_pointer_value();
            let push_f = low.runtime_fn(
                push_name,
                low.ctx.void_type().fn_type(&[low.ptrt().into(), i64_ty.into()], false),
            );
            for r in regs {
                let v = low.load(*r);
                b.build_call(push_f, &[arr.into(), v.into()], "").map_err(|e| e.to_string())?;
            }
            low.store(*d, low.tag_ptr(arr));
            Ok(false)
        }
        // Open/close a per-region arena scope. The mark token is an even (int-like)
        // word, so the refcount ops around its register no-op on it.
        ArenaMark(d) => {
            let f = low.runtime_fn("jrt_arena_mark", i64_ty.fn_type(&[], false));
            let tok = b
                .build_call(f, &[], "arena_mark")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_int_value();
            low.store(*d, tok);
            Ok(false)
        }
        ArenaReset(r) => {
            let f = low.runtime_fn(
                "jrt_arena_reset",
                low.ctx.void_type().fn_type(&[i64_ty.into()], false),
            );
            b.build_call(f, &[low.load(*r).into()], "").map_err(|e| e.to_string())?;
            Ok(false)
        }
        GetIndex(d, obj, idx) => {
            let f = low.runtime_fn(
                "jrt_val_index",
                i64_ty.fn_type(&[i64_ty.into(), i64_ty.into()], false),
            );
            let r = b
                .build_call(f, &[low.load(*obj).into(), low.load(*idx).into()], "index")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_int_value();
            low.retain(r); // borrowed element (still owned by the container) → retain
            low.store(*d, r);
            Ok(false)
        }
        MakeDict(d, pairs) => {
            let new_f = low.runtime_fn("jrt_kdict_new", low.ptrt().fn_type(&[], false));
            let dict = b
                .build_call(new_f, &[], "kdict")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_pointer_value();
            let set_f = low.runtime_fn(
                "jrt_kdict_set",
                low.ctx
                    .void_type()
                    .fn_type(&[low.ptrt().into(), i64_ty.into(), i64_ty.into()], false),
            );
            let key_f = low.runtime_fn(
                "jrt_require_dict_key",
                low.ctx.void_type().fn_type(&[i64_ty.into()], false),
            );
            for (kr, vr) in pairs {
                let k = low.load(*kr);
                let v = low.load(*vr);
                // `jrt_kdict_set` reads the key word as a `char*`. A literal
                // key is a string, but a computed one need not be, and
                // `{true: "y"}` used to be a segfault rather than the VM's
                // "dict key must be str, got bool".
                b.build_call(key_f, &[k.into()], "").map_err(|e| e.to_string())?;
                b.build_call(set_f, &[dict.into(), k.into(), v.into()], "")
                    .map_err(|e| e.to_string())?;
            }
            low.store(*d, low.tag_ptr(dict));
            Ok(false)
        }
        // SetIndex returns the (possibly new) container word — an array is
        // mutated in place (same pointer), a dict is copied (value semantics) —
        // so we store it back into the object register. The emitter's following
        // write-back (`SetIndex` then a store of `obj`) then rebinds the variable.
        SetIndex(obj, idx, val) => {
            let f = low.runtime_fn(
                "jrt_val_set_index",
                i64_ty.fn_type(&[i64_ty.into(), i64_ty.into(), i64_ty.into()], false),
            );
            let new_word = b
                .build_call(
                    f,
                    &[low.load(*obj).into(), low.load(*idx).into(), low.load(*val).into()],
                    "setidx",
                )
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_int_value();
            low.store(*obj, new_word);
            Ok(false)
        }

        // The same call, reading and writing the global cell directly.
        //
        // Loading the cell *without* a retain is the whole point. A dict is
        // copy-on-write, so `jk_set_index` copies whenever anything else holds
        // the dict — and going through `GetGlobal`, which retains into a
        // register, made the register that second holder, so every write copied
        // and filling a global dict was quadratic. With the cell as sole owner
        // the write lands in place and hands the same pointer back;
        // `jrt_rc_replace` no-ops when old and new are equal, so storing it back
        // is correct whether the runtime mutated or copied.
        SetIndexGlobal(name, idx, val) => {
            let g = low.global_slot(name);
            let old = b.build_load(i64_ty, g, "gld").map_err(|e| e.to_string())?.into_int_value();
            let f = low.runtime_fn(
                "jrt_val_set_index",
                i64_ty.fn_type(&[i64_ty.into(), i64_ty.into(), i64_ty.into()], false),
            );
            let new_word = b
                .build_call(
                    f,
                    &[old.into(), low.load(*idx).into(), low.load(*val).into()],
                    "setidx",
                )
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_int_value();
            let rc = low.runtime_fn(
                "jrt_rc_replace",
                low.ctx.void_type().fn_type(&[i64_ty.into(), i64_ty.into()], false),
            );
            b.build_call(rc, &[old.into(), new_word.into()], "").map_err(|e| e.to_string())?;
            b.build_store(g, new_word).map_err(|e| e.to_string())?;
            Ok(false)
        }

        // ── Structs (data fields only; methods decline in resolve_user_calls) ──
        // MakeStruct: kind-tagged struct carrying the type name + explicit fields,
        // then fill any omitted optional field from its scalar default (the VM
        // fills these at runtime). GetField/SetField are data-field access on a
        // struct (a missing field / non-struct raises).
        MakeStruct(d, type_name, field_specs, base_reg) => {
            // Check the `...base` before allocating anything. `jrt_require_struct`
            // throws, and throwing past a half-built struct that no slot holds
            // yet strands it, along with every field already retained into it.
            if let Some(breg) = base_reg {
                let req_f = low.runtime_fn(
                    "jrt_require_struct",
                    low.ctx.void_type().fn_type(&[i64_ty.into()], false),
                );
                b.build_call(req_f, &[low.load(*breg).into()], "").map_err(|e| e.to_string())?;
            }
            let new_f =
                low.runtime_fn("jrt_kstruct_new", low.ptrt().fn_type(&[low.ptrt().into()], false));
            let tn = low.cstr(type_name);
            let s = b
                .build_call(new_f, &[tn.into()], "kstruct")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_pointer_value();
            let set_f = low.runtime_fn(
                "jrt_kstruct_set",
                low.ctx
                    .void_type()
                    .fn_type(&[low.ptrt().into(), low.ptrt().into(), i64_ty.into()], false),
            );
            // A `prompt` field holds a prompt value, not the string it wraps —
            // the same wrapping the VM does on this opcode. Without it a compiled
            // binary would read a plain string back out of `a.system`.
            let box_prompt = |w: IntValue<'ctx>| -> Result<IntValue<'ctx>, String> {
                let f =
                    low.runtime_fn("jrt_prompt_new", low.ptrt().fn_type(&[i64_ty.into()], false));
                let p = b
                    .build_call(f, &[w.into()], "fieldprompt")
                    .map_err(|e| e.to_string())?
                    .as_any_value_enum()
                    .into_pointer_value();
                Ok(low.tag_ptr(p))
            };

            for (fname, freg, is_prompt) in field_specs {
                let mut v = low.load(*freg);
                if *is_prompt {
                    v = box_prompt(v)?;
                }
                b.build_call(set_f, &[s.into(), low.cstr(fname).into(), v.into()], "")
                    .map_err(|e| e.to_string())?;
            }
            // `...base`: copy every field the type declares and the literal did
            // not name. Runs after the named fields are in and before the
            // defaults below, so a named field beats the base and the base beats
            // a default — the same order the VM applies.
            if let Some(breg) = base_reg {
                let bv = low.load(*breg);
                let copy_f = low.runtime_fn(
                    "jrt_kstruct_copy_field",
                    low.ctx
                        .void_type()
                        .fn_type(&[low.ptrt().into(), i64_ty.into(), low.ptrt().into()], false),
                );
                if let Some(names) = fnctx.struct_field_names.get(type_name) {
                    for fname in names {
                        if field_specs.iter().any(|(n, _, _)| n == fname) {
                            continue;
                        }
                        b.build_call(copy_f, &[s.into(), bv.into(), low.cstr(fname).into()], "")
                            .map_err(|e| e.to_string())?;
                    }
                }
            }
            // Fill omitted optional fields from their scalar defaults.
            //
            // With a `...base` the setter has to be the one that leaves an
            // occupied field alone: the base was copied in just above, and the
            // plain setter would overwrite every value it supplied with the
            // declared default. The VM gets this from its `get_field(..).is_none()`
            // guard; here it takes a second runtime entry point.
            let default_setter = if base_reg.is_some() {
                low.runtime_fn(
                    "jrt_kstruct_set_if_absent",
                    low.ctx
                        .void_type()
                        .fn_type(&[low.ptrt().into(), low.ptrt().into(), i64_ty.into()], false),
                )
            } else {
                set_f
            };
            if let Some(defaults) = fnctx.struct_defaults.get(type_name) {
                for (fname, dv, is_prompt) in defaults {
                    if field_specs.iter().all(|(n, _, _)| n != fname) {
                        let mut w = low.default_word(dv)?;
                        if *is_prompt {
                            w = box_prompt(w)?;
                        }
                        b.build_call(
                            default_setter,
                            &[s.into(), low.cstr(fname).into(), w.into()],
                            "",
                        )
                        .map_err(|e| e.to_string())?;
                    }
                }
            }
            low.store(*d, low.tag_ptr(s));
            Ok(false)
        }
        GetField(d, obj, field) => {
            let f = low.runtime_fn(
                "jrt_get_field",
                i64_ty.fn_type(&[i64_ty.into(), low.ptrt().into()], false),
            );
            let r = b
                .build_call(f, &[low.load(*obj).into(), low.cstr(field).into()], "getfield")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_int_value();
            // `jrt_get_field` hands back an *owned* reference, so there is no
            // retain here. A method used as a value (`let f = obj.greet`) is a
            // freshly minted bound method, not a borrow of anything, and
            // retaining it put it permanently out of reach of the collector.
            low.store(*d, r);
            Ok(false)
        }
        SetField(obj, field, val) => {
            let f = low.runtime_fn(
                "jrt_set_field",
                low.ctx
                    .void_type()
                    .fn_type(&[i64_ty.into(), low.ptrt().into(), i64_ty.into()], false),
            );
            b.build_call(
                f,
                &[low.load(*obj).into(), low.cstr(field).into(), low.load(*val).into()],
                "",
            )
            .map_err(|e| e.to_string())?;
            Ok(false)
        }
        // GetTypeName (typed `catch <Type>`): the caught value's struct type name
        // as a tagged string (empty for a non-struct), compared against the
        // expected name with CmpEqStr. Now lowerable via the JK_STRUCT type tag.
        GetTypeName(d, src) => {
            let f =
                low.runtime_fn("jrt_get_type_name", low.ptrt().fn_type(&[i64_ty.into()], false));
            let p = b
                .build_call(f, &[low.load(*src).into()], "typename")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_pointer_value();
            low.store(*d, low.tag_str(p));
            Ok(false)
        }

        // ── LLM prompts ───────────────────────────────────────────────────
        // A prompt is its own heap kind (`ObjKind::Prompt`), wrapping the tagged
        // string it will send. It used to be *stored as* that string, on the
        // reasoning that a prompt only ever flows to `PromptDeref`. It does not:
        // it can be printed, put in a struct field, passed, or returned, and at
        // each of those a compiled binary saw a string where the VM saw a prompt.
        // `print(p)` was the visible half — `<prompt>` under `jade run`, the raw
        // text once built — and `MakeStruct` refusing prompt fields was the other.
        MakePrompt(d, text) => {
            let f = low.runtime_fn("jrt_prompt_new", low.ptrt().fn_type(&[i64_ty.into()], false));
            let p = b
                .build_call(f, &[low.load(*text).into()], "promptnew")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_pointer_value();
            low.store(*d, low.tag_ptr(p));
            Ok(false)
        }
        // Run inference on the prompt string, dispatching like the VM:
        //   grammar (`|> <Grammar value>`) → jrt_prompt_grammar_obj (reads the
        //     runtime Grammar object, converts pattern→GBNF, constrains sampling);
        //   typed (`|> int/float/bool/str`) → jrt_prompt_typed (retries until the
        //     response parses) → coerce via jrt_{int,float,bool}_any;
        //   unconstrained (`?p`) → jrt_prompt (returns the full text, no streaming).
        // OUTPUT PARITY: the VM's unconstrained `?p` is a *lazy* TokenStream — no
        // output until it is consumed, at which point `print(r)` streams it once
        // and adds the newline. If the AOT streamed eagerly at the deref *and*
        // then printed the returned text, `let r = ?p; print(r)` would emit the
        // response twice. Using the non-streaming jrt_prompt (like the VM's typed
        // path) makes `print(r)` the single output site → byte-identical stdout
        // (the VM streams tokens live, the AOT prints them in one go — same bytes).
        // The jrt_prompt* helpers already return a tagged, trust-propagated string
        // (a trusted prompt yields a trusted response), so the result is tag_str'd
        // directly — no re-dup, no forced taint. The model is sent empty (the
        // daemon owns model selection).
        PromptDeref(d, prompt_reg, output_type, grammar_reg) => {
            let ptrt = low.ptrt();
            let e = |x: inkwell::builder::BuilderError| x.to_string();

            let prompt_ptr = low.prompt_text_ptr(*prompt_reg);
            // Empty model: the daemon owns model selection (see emit_stream_call).
            let model = low.cstr("");

            let raw = if let Some(gr) = grammar_reg {
                // Grammar-constrained: pass the Grammar object pointer.
                let gobj = low.untag_ptr(low.load(*gr));
                let f = low.runtime_fn(
                    "jrt_prompt_grammar_obj",
                    ptrt.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into()], false),
                );
                b.build_call(f, &[prompt_ptr.into(), model.into(), gobj.into()], "promptg")
                    .map_err(e)?
                    .as_any_value_enum()
                    .into_pointer_value()
            } else if let Some(t) =
                output_type.as_deref().filter(|t| fnctx.struct_field_names.contains_key(*t))
            {
                // A struct output type coerces the reply into a struct. This
                // returns a tagged *word*, not a string, so it bypasses the
                // tag_str + coerce tail below entirely. Without it the C
                // validator waved the raw reply through and the deref produced
                // a string, which then failed on any field access.
                let f = low.runtime_fn(
                    "jrt_prompt_struct",
                    i64_ty.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into()], false),
                );
                let tname = low.cstr(t);
                let w = b
                    .build_call(f, &[prompt_ptr.into(), model.into(), tname.into()], "prompts")
                    .map_err(e)?
                    .as_any_value_enum()
                    .into_int_value();
                low.store(*d, w);
                return Ok(false);
            } else if let Some(t) = output_type.as_deref() {
                // ..._checked, not the bare jrt_prompt_typed: that returns NULL
                // when the reply doesn't coerce, and tagging NULL as a string
                // segfaulted the program where the VM reported a clean error.
                let f = low.runtime_fn(
                    "jrt_prompt_typed_checked",
                    ptrt.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into()], false),
                );
                let tname = low.cstr(t);
                b.build_call(f, &[prompt_ptr.into(), model.into(), tname.into()], "promptt")
                    .map_err(e)?
                    .as_any_value_enum()
                    .into_pointer_value()
            } else {
                let f =
                    low.runtime_fn("jrt_prompt", ptrt.fn_type(&[ptrt.into(), ptrt.into()], false));
                b.build_call(f, &[prompt_ptr.into(), model.into()], "prompt")
                    .map_err(e)?
                    .as_any_value_enum()
                    .into_pointer_value()
            };

            let str_word = low.tag_str(raw);

            // Coerce the typed variants (the C helper already raised if the
            // reply didn't parse, so a value here is coercible).
            let result = match output_type.as_deref() {
                Some(ty @ ("int" | "float" | "bool")) => {
                    let name = match ty {
                        "int" => "jrt_int_any",
                        "float" => "jrt_float_any",
                        _ => "jrt_bool_any",
                    };
                    let f = low.runtime_fn(name, i64_ty.fn_type(&[i64_ty.into()], false));
                    b.build_call(f, &[str_word.into()], "pconv")
                        .map_err(e)?
                        .as_any_value_enum()
                        .into_int_value()
                }
                _ => str_word, // None or "str"
            };
            low.store(*d, result);
            Ok(false)
        }

        // ── Async: spawn / await / join ───────────────────────────────────
        // Spawn a known async function: pack args into a stack array and call
        // jade_spawn(jf_task_<uid>, args, n). The future is stored as a raw
        // pointer word (futures only flow to await/join, never generic ops).
        Spawn(dest, dyn_callee, dyn_args) => match user_calls.get(&idx) {
            Some(CallKind::Spawn { uid, args }) => {
                // Pack `params.len()` slots: provided args, then omitted trailing
                // defaults (the task wrapper unpacks exactly that many).
                let cf = &fnctx.defs[*uid];
                let n = cf.params.len();
                // Entry-block buffer, not an alloca here: spawning in a loop is
                // ordinary. Reuse across iterations is safe because `jrt_spawn`
                // copies the array into the task before it returns — the header
                // on `jade_spawn` says so, and `task.rs` does it. See
                // `Lowerer::entry_buf`.
                let arr = low.entry_buf("spawn_args", n.max(1))?;
                let store_slot = |slot_i: usize, val: IntValue| -> Result<(), String> {
                    let slot = unsafe {
                        b.build_in_bounds_gep(
                            i64_ty,
                            arr,
                            &[i64_ty.const_int(slot_i as u64, false)],
                            "sa",
                        )
                        .map_err(|e| e.to_string())?
                    };
                    b.build_store(slot, val).map_err(|e| e.to_string())?;
                    Ok(())
                };
                for (i, r) in args.iter().enumerate() {
                    store_slot(i, low.load(*r))?;
                }
                for j in args.len()..n {
                    let dv = cf.defaults[j].as_ref().ok_or("codegen: missing spawn default")?;
                    store_slot(j, low.default_word(dv)?)?;
                }
                let task = low
                    .module
                    .get_function(&format!("jf_task_{uid}"))
                    .ok_or("codegen: missing task wrapper")?;
                let spawn_f = low.runtime_fn(
                    "jade_spawn",
                    low.ptrt().fn_type(
                        &[low.ptrt().into(), low.ptrt().into(), low.ctx.i32_type().into()],
                        false,
                    ),
                );
                let fut = b
                    .build_call(
                        spawn_f,
                        &[
                            task.as_global_value().as_pointer_value().into(),
                            arr.into(),
                            low.ctx.i32_type().const_int(n as u64, false).into(),
                        ],
                        "spawn",
                    )
                    .map_err(|e| e.to_string())?
                    .as_any_value_enum()
                    .into_pointer_value();
                // A future is an ordinary tagged value, not a bare pointer.
                // Storing it untagged made it indistinguishable from an Int to
                // every dynamic op: `print(f)` rendered the pointer as a huge
                // integer, and `await 5` happily int_to_ptr'd an integer and
                // segfaulted. It now carries TAG_PTR and ObjKind::Future, so
                // the renderer and the await guard can both recognise it.
                low.store(*dest, low.tag_ptr(fut));
                Ok(false)
            }
            // A spawn whose callee is not statically known: through a local, a
            // collection, a function that returns one, or an `async fn` imported
            // from a module. `jrt_call_value` handles it, because the box says
            // it is async and starting the task is that function's decision
            // rather than this site's. The same path carries a wrong argument
            // count to the callee's own entry, which raises the way the
            // interpreter does instead of refusing to build.
            None => {
                let fut = low.indirect_call(*dyn_callee, dyn_args)?;
                low.store(*dest, fut);
                Ok(false)
            }
            _ => Err(format!("codegen: unsupported spawn at {idx}")),
        },
        Await(dest, fut) => {
            // Pass the tagged word through: the runtime checks the tag and the
            // ObjKind before touching the pointer, so awaiting a non-future
            // raises instead of dereferencing an integer.
            let await_f =
                low.runtime_fn("jade_await_word", i64_ty.fn_type(&[i64_ty.into()], false));
            let r = b
                .build_call(await_f, &[low.load(*fut).into()], "await")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_int_value();
            low.store(*dest, r);
            Ok(false)
        }
        Join(dest, futs) => {
            let n = futs.len();
            // Two entry-block buffers, not allocas here: a join can sit inside a
            // loop. They must stay distinct — `jade_join_words` reads one while
            // writing the other — which is what the differing tags buy. See
            // `Lowerer::entry_buf`.
            let futarr = low.entry_buf("join_futs", n.max(1))?;
            for (i, r) in futs.iter().enumerate() {
                let slot = unsafe {
                    b.build_in_bounds_gep(
                        i64_ty,
                        futarr,
                        &[i64_ty.const_int(i as u64, false)],
                        "jfs",
                    )
                    .map_err(|e| e.to_string())?
                };
                b.build_store(slot, low.load(*r)).map_err(|e| e.to_string())?;
            }
            let resarr = low.entry_buf("join_res", n.max(1))?;
            let join_f = low.runtime_fn(
                "jade_join_words",
                low.ctx.void_type().fn_type(
                    &[low.ptrt().into(), low.ctx.i32_type().into(), low.ptrt().into()],
                    false,
                ),
            );
            b.build_call(
                join_f,
                &[
                    futarr.into(),
                    low.ctx.i32_type().const_int(n as u64, false).into(),
                    resarr.into(),
                ],
                "",
            )
            .map_err(|e| e.to_string())?;
            // Collect results into a kind-tagged array.
            let new_f = low.runtime_fn("jrt_karr_new", low.ptrt().fn_type(&[], false));
            let arr = b
                .build_call(new_f, &[], "jarr")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_pointer_value();
            let push_f = low.runtime_fn(
                "jrt_karr_push",
                low.ctx.void_type().fn_type(&[low.ptrt().into(), i64_ty.into()], false),
            );
            for i in 0..n {
                let slot = unsafe {
                    b.build_in_bounds_gep(
                        i64_ty,
                        resarr,
                        &[i64_ty.const_int(i as u64, false)],
                        "jrs",
                    )
                    .map_err(|e| e.to_string())?
                };
                let v =
                    b.build_load(i64_ty, slot, "jr").map_err(|e| e.to_string())?.into_int_value();
                b.build_call(push_f, &[arr.into(), v.into()], "").map_err(|e| e.to_string())?;
                // `jrt_karr_push` takes the array's own reference, so the one
                // the join handed over is this frame's to give back. `Await`
                // above needs no counterpart: its single `store` *is* the
                // handover. Without this, `join` leaked one object per result
                // whose value lived on the heap — 10,000 iterations of
                // `join(mk(i))` left 10,001 live objects where the same loop
                // written with `await` left 1, so a service that joined in its
                // request loop grew without bound.
                low.decref(v);
            }
            low.store(*dest, low.tag_ptr(arr));
            Ok(false)
        }

        // ── Exceptions (raise side; catch side is a later brick) ──────────
        // Raise longjmps to the active handler (set up by SetupHandler) or, if
        // none, aborts with an explicit message — matching the VM's uncaught
        // path. It never returns, so it terminates the block.
        Raise(val) => {
            let v = low.load(*val);
            // No scope exit here. The throw releases what *every* frame it
            // erases was holding, this one included — see
            // `jade_frame_release_above`. Doing both would release twice.
            low.throw(v)?;
            Ok(true)
        }
        // Register a handler frame and split on `setjmp`: 0 → try body
        // (fallthrough, idx+1); non-zero (a longjmp arrived) → a landing block
        // that stores the caught value into `caught_reg` and enters the handler
        // block (idx+1+offset). cfg records the handler as a leader-but-not-
        // normal-successor, so the only edge into it is this landing.
        SetupHandler(caught_reg, off) => {
            let buf =
                handler_bufs.get(&idx).copied().ok_or("SetupHandler: no jmp_buf pre-allocated")?;
            low.push_frame(buf);
            let sj = low.setjmp_fn();
            let r = b
                .build_call(sj, &[buf.into()], "setjmp_r")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_int_value();
            let is_throw = b
                .build_int_compare(IntPredicate::NE, r, low.ctx.i32_type().const_zero(), "is_throw")
                .map_err(|e| e.to_string())?;
            let func = b.get_insert_block().and_then(|bb| bb.get_parent()).unwrap();
            let landing = low.ctx.append_basic_block(func, "exc_landing");
            b.build_conditional_branch(is_throw, landing, block_of(idx + 1))
                .map_err(|e| e.to_string())?;
            // Landing: bind the caught value, then enter the handler body.
            b.position_at_end(landing);
            // The longjmp that brought us here unwound the native stack
            // without running the intervening callees' own `emit_recur_restore`
            // calls, so the counter is still counting frames that no longer
            // exist. Reset it to what it was when *this* function was entered —
            // correct regardless of how many frames were skipped, since this
            // function's own frame is all that is left.
            low.emit_recur_resume()?;
            // Same reasoning one level up: the longjmp skipped the `jrt_yield_pop`
            // of every generator frame it unwound past, so their buffers are
            // still open and the next `yield` here would land in one of them.
            low.emit_yield_restore()?;
            // The exception carries a reference, and this is where it lands.
            //
            // A store takes ownership rather than retaining, so binding the
            // caught value here is the whole transfer: the `catch` variable
            // becomes its owner and releases it at scope exit. Retaining as well
            // would give one reference two owners and leak one per caught raise.
            //
            // This pairs exactly with the retain in `throw` and with the frame
            // release the throw performs, and none of the three works alone. A
            // raise in *this* frame leaves the raiser's own register owning its
            // reference, untouched because this frame is not one of the skipped
            // ones; a raise from deeper had that register released on the way
            // out. Either way the value arrives with exactly one owner to be.
            let caught = low.exc_value();
            low.store(*caught_reg, caught);
            b.build_unconditional_branch(block_of(target(*off))).map_err(|e| e.to_string())?;
            Ok(true)
        }
        PopHandler => {
            low.pop_frame();
            Ok(false)
        }

        // ── Control flow ──────────────────────────────────────────────────
        Jump(off) => {
            b.build_unconditional_branch(block_of(target(*off))).map_err(|e| e.to_string())?;
            Ok(true)
        }
        JumpIfFalse(r, off) => {
            let cond = low.truthy(low.load(*r));
            // Jump to target when false; fall through (idx+1) when true.
            b.build_conditional_branch(cond, block_of(idx + 1), block_of(target(*off)))
                .map_err(|e| e.to_string())?;
            Ok(true)
        }
        JumpIfTrue(r, off) => {
            let cond = low.truthy(low.load(*r));
            b.build_conditional_branch(cond, block_of(target(*off)), block_of(idx + 1))
                .map_err(|e| e.to_string())?;
            Ok(true)
        }
        Yield(src) => {
            let f = low.runtime_fn(
                "jrt_yield_append",
                low.ctx.void_type().fn_type(&[i64_ty.into()], false),
            );
            let v = low.load(*src);
            // No retain here. `jrt_yield_append` appends through `jrt_karr_push`,
            // which takes the buffer's reference itself — the same one every
            // other array write takes. Retaining as well gave the yielded value
            // three references and two owners, so `yield [1, 2]` leaked its array
            // on every pass.
            b.build_call(f, &[v.into()], "").map_err(|e| e.to_string())?;
            Ok(false)
        }
        Return(opt) => {
            // A generator's return value is its buffer, never the body's value.
            // Closing the frame here rather than at one exit point covers every
            // return path — an explicit `return`, the implicit one at the end,
            // and a `return` inside a `try`.
            // A generator hands back its buffer; everything else hands back a
            // register. The difference is who owns what: `jrt_yield_pop` gives up
            // the buffer's one reference, while a register keeps owning its value
            // until scope exit releases it just below.
            let (v, borrowed) = if low.is_generator {
                let f = low.runtime_fn("jrt_yield_pop", i64_ty.fn_type(&[], false));
                let popped = b
                    .build_call(f, &[], "ypop")
                    .map_err(|e| e.to_string())?
                    .as_any_value_enum()
                    .into_int_value();
                (popped, false)
            } else {
                let w = match opt {
                    Some(r) => low.load(*r),
                    None => i64_ty.const_int(NIL, false),
                };
                (w, true)
            };
            // Transfer the returned reference to the caller: retain it, then the
            // scope-exit release (which decrefs the source slot) nets an ownership
            // move rather than a free of a value the caller now holds.
            //
            // Not for a generator's buffer, which arrives already owned and is not
            // in any register for scope exit to release. Retaining it there gave
            // the buffer two references and one owner, so every call to a
            // `yield`ing function leaked its stream.
            if borrowed {
                low.incref(v);
            }
            low.emit_scope_exit();
            // Drop any handler this function opened but did not fall out of —
            // `return` inside a `try` skips the emitter's PopHandler.
            low.emit_exc_restore()?;
            // Close the recursion frame this function's prologue opened.
            low.emit_recur_restore()?;
            b.build_return(Some(&v)).map_err(|e| e.to_string())?;
            Ok(true)
        }
        // Program terminator (the VM breaks its dispatch loop). A lowered
        // chunk-function ends by returning nil.
        Halt => {
            low.emit_scope_exit();
            low.emit_exc_restore()?;
            low.emit_recur_restore()?;
            b.build_return(Some(&i64_ty.const_int(NIL, false))).map_err(|e| e.to_string())?;
            Ok(true)
        }

        // ── Function values (first-class: boxed function pointers) ────────
        // Materialize `jf_<uid>` as a callable value (used by escapes / indirect
        // calls; a devirtualized direct call ignores it). A closure is just a
        // plain function here — it captures only globals, read via GetGlobal.
        LoadFn(d, idx) | MakeClosure(d, idx) => {
            match fnctx.uid_of(fn_defs, *idx) {
                Some(uid) => {
                    // The box points at the indirect entry, which checks arity
                    // and fills defaults, not at the body.
                    let entry = low
                        .module
                        .get_function(&format!("jf_ind_{uid}"))
                        .ok_or(format!("codegen: missing indirect entry for fn {uid}"))?;
                    // The box says whether calling it starts a task, because the
                    // site that eventually calls it will only have the value.
                    let is_async = fnctx.defs[uid].is_async;
                    low.store(*d, low.fn_box_word(uid, entry, is_async))
                }
                None => return Err(format!("codegen: unknown fn_def index {idx}")),
            }
            Ok(false)
        }

        // ── Calls ─────────────────────────────────────────────────────────
        // Direct call to a known function (devirtualized), an indirect call
        // through a runtime function value, or a devirtualized builtin. A call to
        // an unlowered reserved builtin already declined in `resolve_user_calls`.
        Call(dest, callee, args) => {
            match user_calls.get(&idx) {
                Some(CallKind::Direct { uid, args: dargs }) => {
                    let f = fnctx.funcs[*uid];
                    let cf = &fnctx.defs[*uid];
                    let mut argv: Vec<BasicMetadataValueEnum> = Vec::with_capacity(cf.params.len());
                    for a in dargs {
                        argv.push(low.load(*a).into());
                    }
                    for j in dargs.len()..cf.params.len() {
                        let dv = cf.defaults[j]
                            .as_ref()
                            .ok_or("codegen: missing default at call site")?;
                        argv.push(low.default_word(dv)?.into());
                    }
                    let ret = b
                        .build_call(f, &argv, "callret")
                        .map_err(|e| e.to_string())?
                        .as_any_value_enum()
                        .into_int_value();
                    low.store(*dest, ret);
                    return Ok(false);
                }
                Some(CallKind::StreamCall { prompt, grammar }) => {
                    emit_stream_call(low, *dest, *prompt, *grammar)?;
                    return Ok(false);
                }
                // A struct method resolved to one implementation → direct call
                // with the receiver as `self` (param 0), the explicit args next,
                // then omitted trailing defaults.
                //
                // Guarded on the receiver's runtime type first. Resolution picks
                // by method name and arity, because bytecode carries no types,
                // so nothing in it establishes that the receiver *is* the type
                // that declares the method. It usually is, and then this is the
                // fast path. When it is not — another struct, an inherited
                // method, an array that happens to share a method name — the
                // guard falls through to dynamic dispatch, which looks the
                // implementation up against the type the receiver actually has
                // and raises if there is none. Without it, `fn call(o) { o.go() }`
                // ran type A's `go` on a B and answered with a number computed
                // from another type's fields.
                Some(CallKind::MethodDirect {
                    uid,
                    type_name,
                    method,
                    prim,
                    self_reg,
                    args: margs,
                }) => {
                    let f = fnctx.funcs[*uid];
                    let cf = &fnctx.defs[*uid];
                    let i32_ty = low.ctx.i32_type();
                    let isty = low.runtime_fn(
                        "jrt_struct_is_type",
                        i32_ty.fn_type(&[i64_ty.into(), low.ptrt().into()], false),
                    );
                    let same = b
                        .build_call(
                            isty,
                            &[low.load(*self_reg).into(), low.cstr(type_name).into()],
                            "isty",
                        )
                        .map_err(|e| e.to_string())?
                        .as_any_value_enum()
                        .into_int_value();
                    let cond = b
                        .build_int_compare(
                            inkwell::IntPredicate::NE,
                            same,
                            i32_ty.const_zero(),
                            "sametype",
                        )
                        .map_err(|e| e.to_string())?;
                    let cur = b.get_insert_block().unwrap().get_parent().unwrap();
                    let direct_bb = low.ctx.append_basic_block(cur, "m_direct");
                    let dyn_bb = low.ctx.append_basic_block(cur, "m_dynamic");
                    let merge_bb = low.ctx.append_basic_block(cur, "m_merge");
                    b.build_conditional_branch(cond, direct_bb, dyn_bb)
                        .map_err(|e| e.to_string())?;

                    b.position_at_end(direct_bb);
                    let mut argv: Vec<BasicMetadataValueEnum> = Vec::with_capacity(cf.params.len());
                    argv.push(low.load(*self_reg).into());
                    for a in margs {
                        argv.push(low.load(*a).into());
                    }
                    for j in (1 + margs.len())..cf.params.len() {
                        let dv = cf.defaults[j]
                            .as_ref()
                            .ok_or("codegen: missing method default at call site")?;
                        argv.push(low.default_word(dv)?.into());
                    }
                    let direct_ret = b
                        .build_call(f, &argv, "mcallret")
                        .map_err(|e| e.to_string())?
                        .as_any_value_enum()
                        .into_int_value();
                    b.build_unconditional_branch(merge_bb).map_err(|e| e.to_string())?;
                    let direct_end = b.get_insert_block().unwrap();

                    b.position_at_end(dyn_bb);
                    let dyn_ret = emit_method_fallback(low, *self_reg, method, margs, *prim)?;
                    b.build_unconditional_branch(merge_bb).map_err(|e| e.to_string())?;
                    let dyn_end = b.get_insert_block().unwrap();

                    b.position_at_end(merge_bb);
                    let phi = b.build_phi(i64_ty, "mret").map_err(|e| e.to_string())?;
                    phi.add_incoming(&[(&direct_ret, direct_end), (&dyn_ret, dyn_end)]);
                    low.store(*dest, phi.as_basic_value().into_int_value());
                    return Ok(false);
                }
                Some(CallKind::ModuleCall { module, method, args: margs }) => {
                    let ret = emit_module_call(low, module, method, margs)?;
                    low.store(*dest, ret);
                    return Ok(false);
                }
                Some(CallKind::NativeCall { pkgid, fname, args: margs }) => {
                    let ret = emit_native_call(low, *pkgid, fname, margs)?;
                    low.store(*dest, ret);
                    return Ok(false);
                }
                Some(CallKind::PrimStrMethod { recv, method, args: margs }) => {
                    let ret = emit_str_method(low, *recv, method, margs)?;
                    low.store(*dest, ret);
                    return Ok(false);
                }
                Some(CallKind::PrimValMethod { recv, method, args: margs }) => {
                    let ret = emit_val_method(low, *recv, method, margs)?;
                    low.store(*dest, ret);
                    return Ok(false);
                }
                Some(CallKind::MethodDynamic { recv, method, prim, args: margs }) => {
                    let ret = emit_method_fallback(low, *recv, method, margs, *prim)?;
                    low.store(*dest, ret);
                    return Ok(false);
                }
                Some(CallKind::Indirect) => {
                    let ret = low.indirect_call(*callee, args)?;
                    low.store(*dest, ret);
                    return Ok(false);
                }
                // A plain Call is never resolved as DirectNamed/Spawn.
                Some(CallKind::DirectNamed { .. }) | Some(CallKind::Spawn { .. }) | None => {}
            }
            match call_builtins.get(&idx) {
                Some(bc) if bc.name == "print" => {
                    let arg = low.load(bc.args[0]);
                    match bc.args.get(1) {
                        // `print(x, end)`: the second argument replaces the
                        // newline. Routed through the same entry the builtin's
                        // *value* uses, so both spellings agree.
                        Some(end) => {
                            let buf = low.entry_buf("printv", 2)?;
                            for (i, r) in [arg, low.load(*end)].into_iter().enumerate() {
                                let slot = unsafe {
                                    b.build_in_bounds_gep(
                                        i64_ty,
                                        buf,
                                        &[i64_ty.const_int(i as u64, false)],
                                        "pa",
                                    )
                                    .map_err(|e| e.to_string())?
                                };
                                b.build_store(slot, r).map_err(|e| e.to_string())?;
                            }
                            let f = low.runtime_fn(
                                "jrt_call_value",
                                i64_ty.fn_type(
                                    &[i64_ty.into(), i64_ty.into(), low.ptrt().into()],
                                    false,
                                ),
                            );
                            let bv = low.runtime_fn(
                                "jrt_builtin_value",
                                i64_ty.fn_type(&[low.ptrt().into()], false),
                            );
                            let callee = b
                                .build_call(bv, &[low.cstr("print").into()], "pbv")
                                .map_err(|e| e.to_string())?
                                .as_any_value_enum()
                                .into_int_value();
                            b.build_call(
                                f,
                                &[callee.into(), i64_ty.const_int(2, false).into(), buf.into()],
                                "pcall",
                            )
                            .map_err(|e| e.to_string())?;
                            low.store(*dest, i64_ty.const_int(NIL, false));
                        }
                        None => low.print_value(arg, *dest)?,
                    }
                    Ok(false)
                }
                // write(x) → print with no newline, flushed. Same renderer as
                // print, so the two agree on how a value looks.
                Some(bc) if bc.name == "write" => {
                    let arg = low.load(bc.args[0]);
                    low.write_value(arg, *dest)?;
                    Ok(false)
                }
                // str(x) → tagged string via jrt_str_of_any (VM-faithful
                // value_to_display for scalars/strings; an object arg renders the
                // runtime's placeholder, but such programs construct the object
                // and fall back before reaching here).
                Some(bc) if bc.name == "str" => {
                    let p = low.str_of_any(bc.args[0]);
                    low.store(*dest, low.tag_str(p));
                    Ok(false)
                }
                // int(x)/float(x)/bool(x) → tag-dispatching runtime conversions
                // (VM-faithful; int/float raise a catchable error on bad input).
                Some(bc) if matches!(bc.name, "int" | "float" | "bool" | "char") => {
                    let fname = match bc.name {
                        "int" => "jrt_int_any",
                        "float" => "jrt_float_any",
                        "char" => "jrt_char_any",
                        _ => "jrt_bool_any",
                    };
                    let f = low.runtime_fn(fname, i64_ty.fn_type(&[i64_ty.into()], false));
                    let arg = low.load(bc.args[0]);
                    let r = b
                        .build_call(f, &[arg.into()], "conv")
                        .map_err(|e| e.to_string())?
                        .as_any_value_enum()
                        .into_int_value();
                    low.store(*dest, r);
                    Ok(false)
                }
                // len(x) → jrt_len_chunk (tag-dispatched: strlen for a string, the
                // shared ObjHeader.len for a kind-tagged collection) → tag as int.
                // Collections now lower on the Chunk path (MakeArray/MakeDict), so
                // len() over them must read the header count — jrt_len_unknown reads
                // the legacy offset-8 length and would return the kind byte here.
                Some(bc) if bc.name == "len" => {
                    // `jade_len`, not `jrt_len_chunk`: the C forwarder is what
                    // raises for a value with no length, which the Rust core
                    // cannot do (a raise is a longjmp).
                    let f = low.runtime_fn("jade_len", i64_ty.fn_type(&[i64_ty.into()], false));
                    let arg = low.load(bc.args[0]);
                    let count = b
                        .build_call(f, &[arg.into()], "len")
                        .map_err(|e| e.to_string())?
                        .as_any_value_enum()
                        .into_int_value();
                    low.store(*dest, low.tag_int(count));
                    Ok(false)
                }
                _ => Err(format!("codegen: unsupported call at {idx}")),
            }
        }

        // ── Keyword-argument calls (direct to a known function only) ──────
        CallNamed(dest, _callee, _pairs) => match user_calls.get(&idx) {
            Some(CallKind::DirectNamed { uid, arg_slots }) => {
                let f = fnctx.funcs[*uid];
                let cf = &fnctx.defs[*uid];
                let mut argv: Vec<BasicMetadataValueEnum> = Vec::with_capacity(arg_slots.len());
                for (i, slot) in arg_slots.iter().enumerate() {
                    let w = match slot {
                        Some(r) => low.load(*r),
                        None => {
                            let dv = cf.defaults[i]
                                .as_ref()
                                .ok_or("codegen: missing default in keyword call")?;
                            low.default_word(dv)?
                        }
                    };
                    argv.push(w.into());
                }
                let ret = b
                    .build_call(f, &argv, "callret")
                    .map_err(|e| e.to_string())?
                    .as_any_value_enum()
                    .into_int_value();
                low.store(*dest, ret);
                Ok(false)
            }
            Some(CallKind::StreamCall { prompt, grammar }) => {
                emit_stream_call(low, *dest, *prompt, *grammar)?;
                Ok(false)
            }
            // A keyword module call pre-resolved to a module call (fs.read trust).
            Some(CallKind::ModuleCall { module, method, args: margs }) => {
                let ret = emit_module_call(low, module, method, margs)?;
                low.store(*dest, ret);
                Ok(false)
            }
            _ => Err(format!("codegen: unsupported keyword call at {idx}")),
        },

        // Everything else is added in later bricks; until then the daemon falls
        // back to the legacy lowering for programs that use it.
        other => Err(format!("codegen: unsupported opcode {other:?}")),
    }
}
