//! Integer, float, and bitwise arithmetic, plus the comparison family.
//!
//! Split out of the former monolithic `lower.rs`; see this directory's README.

use super::*;

impl<'a, 'ctx> Lowerer<'a, 'ctx> {
    /// Integer divide (`is_mod == false`) or modulo, with the VM's catchable
    /// zero-divisor behavior: LLVM `sdiv`/`srem` by 0 is UB (traps on arm64),
    /// so we branch on a zero divisor and `raise` a "division/modulo by zero"
    /// string instead. Leaves the builder on the non-zero ("ok") block and
    /// returns the tagged result word.
    pub(super) fn int_div_mod(
        &self,
        l: Reg,
        r: Reg,
        is_mod: bool,
    ) -> Result<IntValue<'ctx>, String> {
        let (a, c) = self.int_operands(l, r);
        let func = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or("int_div_mod outside a function")?;
        let is_zero = self
            .builder
            .build_int_compare(IntPredicate::EQ, c, self.i64t().const_zero(), "divz")
            .map_err(|e| e.to_string())?;
        let throw_bb = self.ctx.append_basic_block(func, "divzero_throw");
        let ok_bb = self.ctx.append_basic_block(func, "divzero_ok");
        self.builder
            .build_conditional_branch(is_zero, throw_bb, ok_bb)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(throw_bb);
        self.throw_runtime(if is_mod { "modulo by zero" } else { "division by zero" })?;

        self.builder.position_at_end(ok_bb);
        let res = if is_mod {
            self.builder.build_int_signed_rem(a, c, "iremi").map_err(|e| e.to_string())?
        } else {
            self.builder.build_int_signed_div(a, c, "idivi").map_err(|e| e.to_string())?
        };
        Ok(self.tag_int(res))
    }

    /// Float divide, with the VM's catchable zero-divisor behavior: a raw LLVM
    /// `fdiv` by `0.0` is well-defined IEEE 754 (`inf`/`-inf`/`NaN`), but the
    /// language doesn't want that — the VM's `DivFloat` raises the same
    /// `division by zero` a float divisor of 0 raises through the dynamic
    /// (`Op::Div`) path, so the statically-typed fast path has to match rather
    /// than let a well-typed program produce a silent `NaN` compiled that
    /// would have raised interpreted. Branches on a zero divisor and `raise`s
    /// instead of dividing. Leaves the builder on the non-zero ("ok") block
    /// and returns the boxed result.
    pub(super) fn float_div(&self, l: Reg, r: Reg) -> Result<IntValue<'ctx>, String> {
        let (a, c) = self.float_operands(l, r);
        let func = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or("float_div outside a function")?;
        let is_zero = self
            .builder
            .build_float_compare(FloatPredicate::OEQ, c, self.f64t().const_zero(), "fdivz")
            .map_err(|e| e.to_string())?;
        let throw_bb = self.ctx.append_basic_block(func, "fdivzero_throw");
        let ok_bb = self.ctx.append_basic_block(func, "fdivzero_ok");
        self.builder
            .build_conditional_branch(is_zero, throw_bb, ok_bb)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(throw_bb);
        self.throw_runtime("division by zero")?;

        self.builder.position_at_end(ok_bb);
        let res = self.builder.build_float_div(a, c, "divf").map_err(|e| e.to_string())?;
        Ok(self.box_float(res))
    }

    /// Compute a checked integer result: do the arithmetic in i128, verify it
    /// fits the 63-bit tagged range, and raise "integer overflow" if not.
    ///
    /// `AddInt`/`SubInt`/`MulInt`/`NegInt` used to emit a bare LLVM add/sub/mul
    /// and tag whatever came out, so compiled integer arithmetic silently
    /// produced a wrong number on overflow while the VM — which uses
    /// `checked_add`/`checked_mul` — raised. Two distinct gaps: the result could
    /// exceed i64, and even an in-i64 result may not fit a tagged word, because
    /// one bit goes to the tag.
    ///
    /// i128 closes both at once: two 63-bit operands cannot overflow it under
    /// any of these operations, so one range check against the tagged bounds is
    /// exact. The widening costs a couple of instructions and buys agreement
    /// with the VM on every integer operation.
    pub(super) fn checked_int_result(
        &self,
        wide: IntValue<'ctx>,
        what: &str,
    ) -> Result<IntValue<'ctx>, String> {
        let i128t = self.ctx.i128_type();
        let func = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or("checked_int_result outside a function")?;

        // The inclusive bounds of a tagged integer: -(1<<62) ..= (1<<62)-1.
        let max = i128t.const_int((1u64 << 62) - 1, false);
        let min = self
            .builder
            .build_int_neg(i128t.const_int(1u64 << 62, false), "imin")
            .map_err(|e| e.to_string())?;

        let too_big = self
            .builder
            .build_int_compare(IntPredicate::SGT, wide, max, "ovf_hi")
            .map_err(|e| e.to_string())?;
        let too_small = self
            .builder
            .build_int_compare(IntPredicate::SLT, wide, min, "ovf_lo")
            .map_err(|e| e.to_string())?;
        let overflowed =
            self.builder.build_or(too_big, too_small, "ovf").map_err(|e| e.to_string())?;

        let throw_bb = self.ctx.append_basic_block(func, "intovf_throw");
        let ok_bb = self.ctx.append_basic_block(func, "intovf_ok");
        self.builder
            .build_conditional_branch(overflowed, throw_bb, ok_bb)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(throw_bb);
        self.throw_runtime("integer overflow")?;

        self.builder.position_at_end(ok_bb);
        let narrowed =
            self.builder.build_int_truncate(wide, self.i64t(), what).map_err(|e| e.to_string())?;
        Ok(self.tag_int(narrowed))
    }

    /// Sign-extend an untagged i64 operand to i128 for checked arithmetic.
    pub(super) fn widen(&self, v: IntValue<'ctx>) -> Result<IntValue<'ctx>, String> {
        self.builder.build_int_s_extend(v, self.ctx.i128_type(), "w").map_err(|e| e.to_string())
    }

    pub(super) fn int_cmp(&self, l: Reg, r: Reg, pred: IntPredicate) -> IntValue<'ctx> {
        let (a, c) = self.int_operands(l, r);
        let b = self.builder.build_int_compare(pred, a, c, "icmp").unwrap();
        self.bool_word(b)
    }

    pub(super) fn float_cmp(&self, l: Reg, r: Reg, pred: FloatPredicate) -> IntValue<'ctx> {
        let (a, c) = self.float_operands(l, r);
        let b = self.builder.build_float_compare(pred, a, c, "fcmp").unwrap();
        self.bool_word(b)
    }

    pub(super) fn bool_cmp(&self, l: Reg, r: Reg, pred: IntPredicate) -> IntValue<'ctx> {
        let (a, c) = (self.zext_bool(l), self.zext_bool(r));
        let b = self.builder.build_int_compare(pred, a, c, "bcmp").unwrap();
        self.bool_word(b)
    }

    /// Mixed int/float ordering: widen whichever side is an int, then `fcmp`.
    pub(super) fn mixed_cmp(
        &self,
        l: Reg,
        l_int: bool,
        r: Reg,
        r_int: bool,
        pred: FloatPredicate,
    ) -> IntValue<'ctx> {
        let a = self.as_float(l, l_int);
        let c = self.as_float(r, r_int);
        let b = self.builder.build_float_compare(pred, a, c, "mcmp").unwrap();
        self.bool_word(b)
    }

    /// Native i64 bitwise/shift op on two untagged int operands, re-tagged.
    pub(super) fn int_bitop(
        &self,
        l: Reg,
        r: Reg,
        f: impl FnOnce(IntValue<'ctx>, IntValue<'ctx>) -> Result<IntValue<'ctx>, String>,
    ) -> Result<IntValue<'ctx>, String> {
        let (a, c) = self.int_operands(l, r);
        Ok(self.tag_int(f(a, c)?))
    }

    /// Dynamic binary arithmetic `jrt_<name>(i64, i64) -> i64` (tagged result).
    pub(super) fn any2(&self, name: &str, l: Reg, r: Reg) -> IntValue<'ctx> {
        let f = self.runtime_fn(
            name,
            self.i64t().fn_type(&[self.i64t().into(), self.i64t().into()], false),
        );
        self.builder
            .build_call(f, &[self.load(l).into(), self.load(r).into()], "any2")
            .unwrap()
            .as_any_value_enum()
            .into_int_value()
    }

    /// Like [`any2`], but with an inline **both-int fast path**: when both operand
    /// words are tagged ints (low bit 0), compute the result with native checked
    /// i128 arithmetic (identical to `AddInt`/`SubInt`/`MulInt`, so overflow still
    /// raises exactly as the VM does) instead of calling the runtime. Anything
    /// else falls through to `jrt_<name>` unchanged. `slow_name` must be one of
    /// `jrt_add_any` / `jrt_sub_any` / `jrt_mul_any`.
    ///
    /// This is what makes untyped integer code (recursive `fib`, whose params are
    /// `Unknown`) run on native adds/subs rather than a tag-dispatching call.
    pub(super) fn any2_int_fast(
        &self,
        slow_name: &str,
        l: Reg,
        r: Reg,
    ) -> Result<IntValue<'ctx>, String> {
        let b = self.builder;
        let e = |x: inkwell::builder::BuilderError| x.to_string();
        let lw = self.load(l);
        let rw = self.load(r);
        let func = b.get_insert_block().unwrap().get_parent().unwrap();
        let fast_bb = self.ctx.append_basic_block(func, "arith_fast");
        let slow_bb = self.ctx.append_basic_block(func, "arith_slow");
        let merge_bb = self.ctx.append_basic_block(func, "arith_merge");

        // both int  ⇔  (lw | rw) & 1 == 0  (an int is the only tag with bit0 == 0).
        let orw = b.build_or(lw, rw, "arith_or").map_err(e)?;
        let low1 = b.build_and(orw, self.i64t().const_int(1, false), "arith_low1").map_err(e)?;
        let both_int = b
            .build_int_compare(IntPredicate::EQ, low1, self.i64t().const_zero(), "arith_bothint")
            .map_err(e)?;
        b.build_conditional_branch(both_int, fast_bb, slow_bb).map_err(e)?;

        // Fast: native checked arithmetic on the untagged operands.
        b.position_at_end(fast_bb);
        let wa = self.widen(self.untag_int(lw))?;
        let wc = self.widen(self.untag_int(rw))?;
        let s = match slow_name {
            "jrt_add_any" => b.build_int_add(wa, wc, "faddi").map_err(e)?,
            "jrt_sub_any" => b.build_int_sub(wa, wc, "fsubi").map_err(e)?,
            "jrt_mul_any" => b.build_int_mul(wa, wc, "fmuli").map_err(e)?,
            other => return Err(format!("any2_int_fast: unsupported op {other}")),
        };
        let fast_res = self.checked_int_result(s, "fint")?; // splits on overflow, ends at ok block
        let fast_end = b.get_insert_block().unwrap();
        b.build_unconditional_branch(merge_bb).map_err(e)?;

        // Slow: the tag-dispatching runtime helper (handles float/str/mixed/error).
        b.position_at_end(slow_bb);
        let f = self.runtime_fn(
            slow_name,
            self.i64t().fn_type(&[self.i64t().into(), self.i64t().into()], false),
        );
        let slow_res = b
            .build_call(f, &[lw.into(), rw.into()], "any2slow")
            .map_err(e)?
            .as_any_value_enum()
            .into_int_value();
        let slow_end = b.get_insert_block().unwrap();
        b.build_unconditional_branch(merge_bb).map_err(e)?;

        b.position_at_end(merge_bb);
        let phi = b.build_phi(self.i64t(), "arith_res").map_err(e)?;
        phi.add_incoming(&[(&fast_res, fast_end), (&slow_res, slow_end)]);
        Ok(phi.as_basic_value().into_int_value())
    }

    /// `jrt_eq_any(i64, i64) -> i32` (1 when equal).
    pub(super) fn eq_any(&self, l: Reg, r: Reg) -> IntValue<'ctx> {
        let f = self.runtime_fn(
            "jrt_eq_any",
            self.ctx.i32_type().fn_type(&[self.i64t().into(), self.i64t().into()], false),
        );
        self.builder
            .build_call(f, &[self.load(l).into(), self.load(r).into()], "eqany")
            .unwrap()
            .as_any_value_enum()
            .into_int_value()
    }

    /// `jrt_cmp_any(i64, i64) -> i32` (three-way: -1 / 0 / 1), with an inline
    /// both-int fast path: untag and compare natively, folding to `(a>c)-(a<c)`,
    /// instead of a runtime call. Non-int operands fall through to `jrt_cmp_any`.
    /// `op` is the source operator (`"'<'"`), passed through so a cross-kind
    /// failure names it the way the VM does rather than saying "comparison".
    pub(super) fn cmp_any(&self, l: Reg, r: Reg, op: &str) -> IntValue<'ctx> {
        let b = self.builder;
        let i32t = self.ctx.i32_type();
        let lw = self.load(l);
        let rw = self.load(r);
        let func = b.get_insert_block().unwrap().get_parent().unwrap();
        let fast_bb = self.ctx.append_basic_block(func, "cmp_fast");
        let slow_bb = self.ctx.append_basic_block(func, "cmp_slow");
        let merge_bb = self.ctx.append_basic_block(func, "cmp_merge");

        let orw = b.build_or(lw, rw, "cmp_or").unwrap();
        let low1 = b.build_and(orw, self.i64t().const_int(1, false), "cmp_low1").unwrap();
        let both_int = b
            .build_int_compare(IntPredicate::EQ, low1, self.i64t().const_zero(), "cmp_bothint")
            .unwrap();
        b.build_conditional_branch(both_int, fast_bb, slow_bb).unwrap();

        // Fast: native three-way `(a > c) - (a < c)` on untagged ints → i32.
        b.position_at_end(fast_bb);
        let a = self.untag_int(lw);
        let c = self.untag_int(rw);
        let gt = b.build_int_compare(IntPredicate::SGT, a, c, "cmp_gt").unwrap();
        let lt = b.build_int_compare(IntPredicate::SLT, a, c, "cmp_lt").unwrap();
        let gt_i = b.build_int_z_extend(gt, i32t, "cmp_gti").unwrap();
        let lt_i = b.build_int_z_extend(lt, i32t, "cmp_lti").unwrap();
        let fast_res = b.build_int_sub(gt_i, lt_i, "cmp_three").unwrap();
        b.build_unconditional_branch(merge_bb).unwrap();

        // Slow: tag-dispatching runtime comparison (float/str/mixed/nil ordering).
        b.position_at_end(slow_bb);
        let ptrt = self.ctx.ptr_type(inkwell::AddressSpace::default());
        let f = self.runtime_fn(
            "jrt_cmp_any_op",
            i32t.fn_type(&[self.i64t().into(), self.i64t().into(), ptrt.into()], false),
        );
        let op_str = self.cstr(op);
        let slow_res = b
            .build_call(f, &[lw.into(), rw.into(), op_str.into()], "cmpany")
            .unwrap()
            .as_any_value_enum()
            .into_int_value();
        b.build_unconditional_branch(merge_bb).unwrap();

        b.position_at_end(merge_bb);
        let phi = b.build_phi(i32t, "cmp_res").unwrap();
        phi.add_incoming(&[(&fast_res, fast_bb), (&slow_res, slow_bb)]);
        phi.as_basic_value().into_int_value()
    }

    /// `jrt_neg_any(i64) -> i64` (tagged result).
    pub(super) fn neg_any(&self, s: Reg) -> IntValue<'ctx> {
        let f = self.runtime_fn("jrt_neg_any", self.i64t().fn_type(&[self.i64t().into()], false));
        self.builder
            .build_call(f, &[self.load(s).into()], "negany")
            .unwrap()
            .as_any_value_enum()
            .into_int_value()
    }

    /// Fold an i32 (from `eq_any`/`cmp_any`) compared to zero into a bool word.
    pub(super) fn i32cmp_word(&self, v: IntValue<'ctx>, pred: IntPredicate) -> IntValue<'ctx> {
        let z = self.ctx.i32_type().const_zero();
        let b = self.builder.build_int_compare(pred, v, z, "i32c").unwrap();
        self.bool_word(b)
    }
}
