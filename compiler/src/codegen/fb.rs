//! A small function-building layer over inkwell's `Builder`.
//!
//! The static backend writes a lot of code by hand: every support routine a
//! program uses (string concatenation, array growth, float printing, …) is
//! emitted as LLVM IR into the program itself. Written against the raw builder,
//! each line of that code would be three lines of `build_*(…).map_err(…)?` and
//! a block juggle per `if`. `F` keeps it readable: methods return values
//! directly, and `if_`, `if_else` and `while_` take closures, so a support
//! routine reads close to the C it replaces.
//!
//! Locals are stack slots made with `var`; LLVM's `mem2reg` turns them into
//! registers when the module is optimized, so there is no cost to writing code
//! this way.
//!
//! The builder only fails when it is not positioned in a block, which `F`
//! guarantees by construction, so those results are unwrapped here once rather
//! than threaded through every caller.

use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, FloatValue, FunctionValue, IntValue,
    PointerValue,
};
use inkwell::{AtomicOrdering, AtomicRMWBinOp, FloatPredicate, IntPredicate};

use super::Cx;

/// Purpose: This function unwraps a builder result.
///
/// Inputs:
/// - `r`: the result of a `build_*` call.
///
/// Outputs: the value. The builder errors only when it has no insertion
/// point, which `F` never allows, so an error here is a bug in this module.
fn ok<T, E: std::fmt::Debug>(r: Result<T, E>) -> T {
    r.expect("the static backend's builder is always positioned in a block")
}

/// Purpose: This struct builds the body of one LLVM function.
pub struct F<'a, 'ctx> {
    pub cx: &'a Cx<'ctx>,
    pub b: Builder<'ctx>,
    pub func: FunctionValue<'ctx>,
    entry: BasicBlock<'ctx>,
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function starts building a function's body.
    ///
    /// Inputs:
    /// - `cx`: the module being built.
    /// - `func`: the function, with no blocks yet.
    ///
    /// Outputs: a builder positioned at the start of its entry block.
    pub fn new(cx: &'a Cx<'ctx>, func: FunctionValue<'ctx>) -> Self {
        let b = cx.ctx.create_builder();
        let entry = cx.ctx.append_basic_block(func, "entry");
        b.position_at_end(entry);
        F { cx, b, func, entry }
    }

    // ── constants ────────────────────────────────────────────────────────

    pub fn i64(&self, v: i64) -> IntValue<'ctx> {
        self.cx.i64t().const_int(v as u64, true)
    }

    pub fn i32(&self, v: i64) -> IntValue<'ctx> {
        self.cx.i32t().const_int(v as u64, true)
    }

    pub fn i8(&self, v: i64) -> IntValue<'ctx> {
        self.cx.i8t().const_int(v as u64, true)
    }

    pub fn bool(&self, v: bool) -> IntValue<'ctx> {
        self.cx.i1t().const_int(v as u64, false)
    }

    pub fn f64(&self, v: f64) -> FloatValue<'ctx> {
        self.cx.f64t().const_float(v)
    }

    pub fn null(&self) -> PointerValue<'ctx> {
        self.cx.ptrt().const_null()
    }

    /// Purpose: This function returns a parameter.
    ///
    /// Inputs:
    /// - `i`: its position.
    ///
    /// Outputs: the parameter's value.
    pub fn param(&self, i: u32) -> BasicValueEnum<'ctx> {
        self.func.get_nth_param(i).expect("parameter exists")
    }

    pub fn param_int(&self, i: u32) -> IntValue<'ctx> {
        self.param(i).into_int_value()
    }

    pub fn param_ptr(&self, i: u32) -> PointerValue<'ctx> {
        self.param(i).into_pointer_value()
    }

    // ── integers ─────────────────────────────────────────────────────────

    pub fn add(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_int_add(a, b, ""))
    }

    pub fn sub(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_int_sub(a, b, ""))
    }

    pub fn mul(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_int_mul(a, b, ""))
    }

    pub fn sdiv(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_int_signed_div(a, b, ""))
    }

    pub fn udiv(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_int_unsigned_div(a, b, ""))
    }

    pub fn srem(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_int_signed_rem(a, b, ""))
    }

    pub fn urem(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_int_unsigned_rem(a, b, ""))
    }

    pub fn and(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_and(a, b, ""))
    }

    pub fn or(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_or(a, b, ""))
    }

    pub fn xor(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_xor(a, b, ""))
    }

    pub fn not(&self, a: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_not(a, ""))
    }

    pub fn shl(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_left_shift(a, b, ""))
    }

    pub fn ashr(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_right_shift(a, b, true, ""))
    }

    pub fn lshr(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_right_shift(a, b, false, ""))
    }

    pub fn icmp(&self, p: IntPredicate, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_int_compare(p, a, b, ""))
    }

    pub fn eq(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        self.icmp(IntPredicate::EQ, a, b)
    }

    pub fn ne(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        self.icmp(IntPredicate::NE, a, b)
    }

    pub fn slt(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        self.icmp(IntPredicate::SLT, a, b)
    }

    pub fn sle(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        self.icmp(IntPredicate::SLE, a, b)
    }

    pub fn sgt(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        self.icmp(IntPredicate::SGT, a, b)
    }

    pub fn sge(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        self.icmp(IntPredicate::SGE, a, b)
    }

    pub fn ult(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        self.icmp(IntPredicate::ULT, a, b)
    }

    pub fn uge(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        self.icmp(IntPredicate::UGE, a, b)
    }

    pub fn zext(&self, v: IntValue<'ctx>, t: inkwell::types::IntType<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_int_z_extend(v, t, ""))
    }

    pub fn sext(&self, v: IntValue<'ctx>, t: inkwell::types::IntType<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_int_s_extend(v, t, ""))
    }

    pub fn trunc(&self, v: IntValue<'ctx>, t: inkwell::types::IntType<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_int_truncate(v, t, ""))
    }

    /// Purpose: This function widens or narrows an int to 64 bits, zero-extending.
    pub fn to_i64(&self, v: IntValue<'ctx>) -> IntValue<'ctx> {
        let w = v.get_type().get_bit_width();
        if w == 64 {
            v
        } else if w < 64 {
            self.zext(v, self.cx.i64t())
        } else {
            self.trunc(v, self.cx.i64t())
        }
    }

    pub fn select<V: BasicValue<'ctx>>(
        &self,
        c: IntValue<'ctx>,
        a: V,
        b: V,
    ) -> BasicValueEnum<'ctx> {
        ok(self.b.build_select(c, a, b, ""))
    }

    pub fn select_int(
        &self,
        c: IntValue<'ctx>,
        a: IntValue<'ctx>,
        b: IntValue<'ctx>,
    ) -> IntValue<'ctx> {
        self.select(c, a, b).into_int_value()
    }

    // ── floats ───────────────────────────────────────────────────────────

    pub fn fadd(&self, a: FloatValue<'ctx>, b: FloatValue<'ctx>) -> FloatValue<'ctx> {
        ok(self.b.build_float_add(a, b, ""))
    }

    pub fn fsub(&self, a: FloatValue<'ctx>, b: FloatValue<'ctx>) -> FloatValue<'ctx> {
        ok(self.b.build_float_sub(a, b, ""))
    }

    pub fn fmul(&self, a: FloatValue<'ctx>, b: FloatValue<'ctx>) -> FloatValue<'ctx> {
        ok(self.b.build_float_mul(a, b, ""))
    }

    pub fn fdiv(&self, a: FloatValue<'ctx>, b: FloatValue<'ctx>) -> FloatValue<'ctx> {
        ok(self.b.build_float_div(a, b, ""))
    }

    pub fn frem(&self, a: FloatValue<'ctx>, b: FloatValue<'ctx>) -> FloatValue<'ctx> {
        ok(self.b.build_float_rem(a, b, ""))
    }

    pub fn fneg(&self, a: FloatValue<'ctx>) -> FloatValue<'ctx> {
        ok(self.b.build_float_neg(a, ""))
    }

    pub fn fcmp(
        &self,
        p: FloatPredicate,
        a: FloatValue<'ctx>,
        b: FloatValue<'ctx>,
    ) -> IntValue<'ctx> {
        ok(self.b.build_float_compare(p, a, b, ""))
    }

    pub fn itof(&self, v: IntValue<'ctx>) -> FloatValue<'ctx> {
        ok(self.b.build_signed_int_to_float(v, self.cx.f64t(), ""))
    }

    pub fn ftoi(&self, v: FloatValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_float_to_signed_int(v, self.cx.i64t(), ""))
    }

    // ── memory ───────────────────────────────────────────────────────────

    /// Purpose: This function makes a stack slot in the entry block.
    ///
    /// Inputs:
    /// - `t`: the slot's type.
    ///
    /// Outputs: a pointer to it. The entry block is where `mem2reg` looks.
    pub fn alloca<T: BasicType<'ctx>>(&self, t: T) -> PointerValue<'ctx> {
        let tmp = self.cx.ctx.create_builder();
        match self.entry.get_first_instruction() {
            Some(i) => tmp.position_before(&i),
            None => tmp.position_at_end(self.entry),
        }
        ok(tmp.build_alloca(t, ""))
    }

    /// Purpose: This function makes a local variable holding a starting value.
    ///
    /// Inputs:
    /// - `v`: the starting value.
    ///
    /// Outputs: a pointer to the variable's slot.
    pub fn var<V: BasicValue<'ctx>>(&self, v: V) -> PointerValue<'ctx> {
        let v = v.as_basic_value_enum();
        let p = self.alloca(v.get_type());
        self.store(p, v);
        p
    }

    pub fn load<T: BasicType<'ctx>>(&self, t: T, p: PointerValue<'ctx>) -> BasicValueEnum<'ctx> {
        ok(self.b.build_load(t, p, ""))
    }

    pub fn load_i64(&self, p: PointerValue<'ctx>) -> IntValue<'ctx> {
        self.load(self.cx.i64t(), p).into_int_value()
    }

    pub fn load_i32(&self, p: PointerValue<'ctx>) -> IntValue<'ctx> {
        self.load(self.cx.i32t(), p).into_int_value()
    }

    pub fn load_i8(&self, p: PointerValue<'ctx>) -> IntValue<'ctx> {
        self.load(self.cx.i8t(), p).into_int_value()
    }

    pub fn load_ptr(&self, p: PointerValue<'ctx>) -> PointerValue<'ctx> {
        self.load(self.cx.ptrt(), p).into_pointer_value()
    }

    pub fn load_f64(&self, p: PointerValue<'ctx>) -> FloatValue<'ctx> {
        self.load(self.cx.f64t(), p).into_float_value()
    }

    pub fn store<V: BasicValue<'ctx>>(&self, p: PointerValue<'ctx>, v: V) {
        ok(self.b.build_store(p, v));
    }

    /// Purpose: This function offsets a pointer by a number of bytes.
    ///
    /// Inputs:
    /// - `p`: the base.
    /// - `off`: the offset in bytes.
    ///
    /// Outputs: the new pointer.
    pub fn at(&self, p: PointerValue<'ctx>, off: IntValue<'ctx>) -> PointerValue<'ctx> {
        // SAFETY: the support code only offsets within objects it allocated.
        ok(unsafe { self.b.build_gep(self.cx.i8t(), p, &[off], "") })
    }

    /// Purpose: This function offsets a pointer by a constant number of bytes.
    pub fn at_c(&self, p: PointerValue<'ctx>, off: i64) -> PointerValue<'ctx> {
        self.at(p, self.i64(off))
    }

    /// Purpose: This function addresses element `i` of an array of `t`.
    ///
    /// Inputs:
    /// - `t`: the element type.
    /// - `p`: the array's first element.
    /// - `i`: the index.
    ///
    /// Outputs: a pointer to the element.
    pub fn elem<T: BasicType<'ctx>>(
        &self,
        t: T,
        p: PointerValue<'ctx>,
        i: IntValue<'ctx>,
    ) -> PointerValue<'ctx> {
        // SAFETY: callers bounds-check `i` before addressing user data.
        ok(unsafe { self.b.build_gep(t, p, &[i], "") })
    }

    /// Purpose: This function addresses a field of an LLVM struct.
    pub fn field(
        &self,
        t: inkwell::types::StructType<'ctx>,
        p: PointerValue<'ctx>,
        i: u32,
    ) -> PointerValue<'ctx> {
        ok(self.b.build_struct_gep(t, p, i, ""))
    }

    pub fn memcpy(&self, dst: PointerValue<'ctx>, src: PointerValue<'ctx>, n: IntValue<'ctx>) {
        ok(self.b.build_memcpy(dst, 1, src, 1, n));
    }

    pub fn memmove(&self, dst: PointerValue<'ctx>, src: PointerValue<'ctx>, n: IntValue<'ctx>) {
        ok(self.b.build_memmove(dst, 1, src, 1, n));
    }

    pub fn is_null(&self, p: PointerValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_is_null(p, ""))
    }

    /// Purpose: This function reads an int with monotonic atomic ordering.
    pub fn load_i64_atomic(&self, p: PointerValue<'ctx>) -> IntValue<'ctx> {
        let v = ok(self.b.build_load(self.cx.i64t(), p, ""));
        if let Some(i) = v.as_instruction_value() {
            let _ = i.set_alignment(8);
            let _ = i.set_atomic_ordering(AtomicOrdering::Monotonic);
        }
        v.into_int_value()
    }

    /// Purpose: This function adds to an int in memory atomically.
    ///
    /// Inputs:
    /// - `p`: the int.
    /// - `delta`: what to add.
    ///
    /// Outputs: the value it held before.
    pub fn atomic_add(&self, p: PointerValue<'ctx>, delta: i64) -> IntValue<'ctx> {
        ok(self.b.build_atomicrmw(
            AtomicRMWBinOp::Add,
            p,
            self.i64(delta),
            AtomicOrdering::AcquireRelease,
        ))
    }

    // ── calls ────────────────────────────────────────────────────────────

    /// Purpose: This function calls a function and returns its value, if any.
    pub fn call(
        &self,
        f: FunctionValue<'ctx>,
        args: &[BasicValueEnum<'ctx>],
    ) -> Option<BasicValueEnum<'ctx>> {
        let args: Vec<BasicMetadataValueEnum> = args.iter().map(|a| (*a).into()).collect();
        let cs = ok(self.b.build_call(f, &args, ""));
        match cs.try_as_basic_value() {
            inkwell::values::ValueKind::Basic(v) => Some(v),
            inkwell::values::ValueKind::Instruction(_) => None,
        }
    }

    /// Purpose: This function calls a function that returns a value.
    pub fn callv(
        &self,
        f: FunctionValue<'ctx>,
        args: &[BasicValueEnum<'ctx>],
    ) -> BasicValueEnum<'ctx> {
        self.call(f, args).expect("support function returns a value")
    }

    pub fn call_int(
        &self,
        f: FunctionValue<'ctx>,
        args: &[BasicValueEnum<'ctx>],
    ) -> IntValue<'ctx> {
        self.callv(f, args).into_int_value()
    }

    pub fn call_ptr(
        &self,
        f: FunctionValue<'ctx>,
        args: &[BasicValueEnum<'ctx>],
    ) -> PointerValue<'ctx> {
        self.callv(f, args).into_pointer_value()
    }

    // ── control flow ─────────────────────────────────────────────────────

    pub fn block(&self) -> BasicBlock<'ctx> {
        self.cx.ctx.append_basic_block(self.func, "")
    }

    pub fn position(&self, bb: BasicBlock<'ctx>) {
        self.b.position_at_end(bb);
    }

    /// Purpose: This function says whether the current block already ends.
    pub fn terminated(&self) -> bool {
        self.b.get_insert_block().and_then(|b| b.get_terminator()).is_some()
    }

    pub fn br(&self, bb: BasicBlock<'ctx>) {
        if !self.terminated() {
            ok(self.b.build_unconditional_branch(bb));
        }
    }

    pub fn cond_br(&self, c: IntValue<'ctx>, a: BasicBlock<'ctx>, b: BasicBlock<'ctx>) {
        if !self.terminated() {
            ok(self.b.build_conditional_branch(c, a, b));
        }
    }

    pub fn ret<V: BasicValue<'ctx>>(&self, v: V) {
        if !self.terminated() {
            ok(self.b.build_return(Some(&v)));
        }
    }

    pub fn ret_void(&self) {
        if !self.terminated() {
            ok(self.b.build_return(None));
        }
    }

    pub fn unreachable(&self) {
        if !self.terminated() {
            ok(self.b.build_unreachable());
        }
    }

    pub fn switch(
        &self,
        v: IntValue<'ctx>,
        default: BasicBlock<'ctx>,
        cases: &[(IntValue<'ctx>, BasicBlock<'ctx>)],
    ) {
        if !self.terminated() {
            ok(self.b.build_switch(v, default, cases));
        }
    }

    /// Purpose: This function runs code only when a condition holds.
    ///
    /// Inputs:
    /// - `c`: the condition.
    /// - `then`: the code.
    ///
    /// Outputs: none. Building continues after the `if`.
    pub fn if_(&self, c: IntValue<'ctx>, then: impl FnOnce(&Self)) {
        let t = self.block();
        let end = self.block();
        self.cond_br(c, t, end);
        self.position(t);
        then(self);
        self.br(end);
        self.position(end);
    }

    /// Purpose: This function runs one of two pieces of code.
    pub fn if_else(&self, c: IntValue<'ctx>, then: impl FnOnce(&Self), els: impl FnOnce(&Self)) {
        let t = self.block();
        let e = self.block();
        let end = self.block();
        self.cond_br(c, t, e);
        self.position(t);
        then(self);
        self.br(end);
        self.position(e);
        els(self);
        self.br(end);
        self.position(end);
    }

    /// Purpose: This function repeats code while a condition holds.
    ///
    /// Inputs:
    /// - `cond`: builds the condition, evaluated before each pass.
    /// - `body`: builds the body. It receives the block after the loop, to
    ///   branch to for an early exit.
    ///
    /// Outputs: none. Building continues after the loop.
    pub fn while_(
        &self,
        cond: impl FnOnce(&Self) -> IntValue<'ctx>,
        body: impl FnOnce(&Self, BasicBlock<'ctx>),
    ) {
        let head = self.block();
        let bb = self.block();
        let end = self.block();
        self.br(head);
        self.position(head);
        let c = cond(self);
        self.cond_br(c, bb, end);
        self.position(bb);
        body(self, end);
        self.br(head);
        self.position(end);
    }

    /// Purpose: This function runs code once for each int in `start..end`.
    ///
    /// Inputs:
    /// - `start`, `end`: the range, end exclusive.
    /// - `body`: builds the body, given the current int and the exit block.
    ///
    /// Outputs: none.
    pub fn for_range(
        &self,
        start: IntValue<'ctx>,
        end: IntValue<'ctx>,
        body: impl FnOnce(&Self, IntValue<'ctx>, BasicBlock<'ctx>),
    ) {
        let i = self.var(start);
        self.while_(
            |f| {
                let cur = f.load_i64(i);
                f.slt(cur, end)
            },
            |f, exit| {
                let cur = f.load_i64(i);
                body(f, cur, exit);
                if f.terminated() {
                    let dead = f.block();
                    f.position(dead);
                }
                let cur = f.load_i64(i);
                f.store(i, f.add(cur, f.i64(1)));
            },
        );
    }

    /// Purpose: This function returns a type's size in bytes, as an i64.
    pub fn size_of(&self, t: BasicTypeEnum<'ctx>) -> IntValue<'ctx> {
        let s = t.size_of().expect("sized type");
        self.to_i64(s)
    }
}
