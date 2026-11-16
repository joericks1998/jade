//! The sized number types: arithmetic at each width, and conversions between
//! number types.
//!
//! The rules are in `design/v2-number-types.md`, and follow assembly: both
//! operands have one width, the type picks the signed or unsigned instruction,
//! and the flags an instruction would set are checked after it, so overflow
//! raises rather than wrapping. A conversion raises when the value does not fit
//! the type it converts to.

use inkwell::types::IntType;
use inkwell::values::{BasicValueEnum, FloatValue, IntValue};
use inkwell::{FloatPredicate, IntPredicate};

use crate::codegen::fb::F;
use crate::mir::{ArithOp, BitOp, SizedNum, Ty};

/// Purpose: This enum holds a number read for conversion: an int, the bits of a
/// `ulong`, which may be past the int's range, or a float.
#[derive(Clone, Copy)]
enum Src<'ctx> {
    Signed(IntValue<'ctx>),
    Unsigned(IntValue<'ctx>),
    Float(FloatValue<'ctx>),
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function computes `+ - * / %` on two values of one sized
    /// type, raising on overflow and on division by zero.
    ///
    /// Inputs:
    /// - `o`: the operator.
    /// - `s`: the sized type.
    /// - `x`, `y`: the operands.
    ///
    /// Outputs: the result.
    pub fn sized_arith(
        &self,
        o: ArithOp,
        s: SizedNum,
        x: BasicValueEnum<'ctx>,
        y: BasicValueEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        if s.is_float() {
            let (x, y) = (x.into_float_value(), y.into_float_value());
            let zero = self.cx.f32t().const_zero();
            return match o {
                ArithOp::Add => self.fadd(x, y),
                ArithOp::Sub => self.fsub(x, y),
                ArithOp::Mul => self.fmul(x, y),
                ArithOp::Div => {
                    self.raise_if(self.fcmp(FloatPredicate::OEQ, y, zero), "division by zero");
                    self.fdiv(x, y)
                }
                ArithOp::Rem => {
                    self.raise_if(self.fcmp(FloatPredicate::OEQ, y, zero), "modulo by zero");
                    self.frem(x, y)
                }
            }
            .into();
        }
        let (x, y) = (x.into_int_value(), y.into_int_value());
        let it = x.get_type();
        let unsigned = s.is_unsigned();
        let over = format!("{} overflow", s.name());
        let zero = it.const_zero();
        let one = it.const_int(1, false);
        let all_ones = it.const_all_ones();
        let min = it.const_int(1u64 << (s.bits() - 1), false);
        match o {
            ArithOp::Add | ArithOp::Sub | ArithOp::Mul => {
                let name = match (o, unsigned) {
                    (ArithOp::Add, false) => "sadd",
                    (ArithOp::Add, true) => "uadd",
                    (ArithOp::Sub, false) => "ssub",
                    (ArithOp::Sub, true) => "usub",
                    (_, false) => "smul",
                    (_, true) => "umul",
                };
                let (r, flag) = self.checked(name, x, y);
                self.raise_if(flag, &over);
                r
            }
            // A raise only marks the error, so the division still runs; it
            // divides by something safe and the result is discarded.
            ArithOp::Div => {
                let z = self.eq(y, zero);
                self.raise_if(z, "division by zero");
                if unsigned {
                    self.udiv(x, self.select_int(z, one, y))
                } else {
                    let edge = self.and(self.eq(x, min), self.eq(y, all_ones));
                    self.raise_if(edge, &over);
                    self.sdiv(x, self.select_int(self.or(z, edge), one, y))
                }
            }
            ArithOp::Rem => {
                let z = self.eq(y, zero);
                self.raise_if(z, "modulo by zero");
                if unsigned {
                    self.urem(x, self.select_int(z, one, y))
                } else {
                    // The smallest value % -1 is 0, but `srem` would trap on it.
                    let edge = self.eq(y, all_ones);
                    let r = self.srem(x, self.select_int(self.or(z, edge), one, y));
                    self.select_int(edge, zero, r)
                }
            }
        }
        .into()
    }

    /// Purpose: This function computes a bitwise operator on a sized integer
    /// type. A shift's amount is an int, and must be less than the width.
    ///
    /// Inputs:
    /// - `o`: the operator.
    /// - `s`: the sized type.
    /// - `x`: the value.
    /// - `y`: the other value, or the shift amount as an int.
    ///
    /// Outputs: the result.
    pub fn sized_bit(
        &self,
        o: BitOp,
        s: SizedNum,
        x: IntValue<'ctx>,
        y: IntValue<'ctx>,
    ) -> IntValue<'ctx> {
        match o {
            BitOp::And => self.and(x, y),
            BitOp::Or => self.or(x, y),
            BitOp::Xor => self.xor(x, y),
            BitOp::Shl | BitOp::Shr => {
                let bits = s.bits() as i64;
                let bad = self.or(self.slt(y, self.i64(0)), self.sge(y, self.i64(bits)));
                self.if_(bad, |f| {
                    let text = f.int_text(y);
                    let sb = f.sb_new();
                    f.sb_push_lit(sb, "invalid shift amount ");
                    f.sb_push_str(sb, text);
                    f.release(&Ty::Str, text.into());
                    f.sb_push_lit(sb, &format!(" for a {}", s.name()));
                    let m = f.sb_finish(sb);
                    f.raise_str(m);
                });
                // Kept in range even when refused, since LLVM leaves a shift
                // past the width undefined.
                let n = self.fit(self.and(y, self.i64(bits - 1)), x.get_type());
                let back = |f: &Self, r| if s.is_unsigned() { f.lshr(r, n) } else { f.ashr(r, n) };
                if o == BitOp::Shl {
                    let r = self.shl(x, n);
                    let lost = self.and(self.not(bad), self.ne(back(self, r), x));
                    self.raise_if(lost, &format!("{} overflow", s.name()));
                    r
                } else {
                    back(self, x)
                }
            }
        }
    }

    /// Purpose: This function negates a signed sized number, raising when the
    /// smallest value has no positive counterpart.
    ///
    /// Inputs:
    /// - `s`: the sized type, signed or `float32`.
    /// - `v`: the value.
    ///
    /// Outputs: the negated value.
    pub fn sized_neg(&self, s: SizedNum, v: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        if s.is_float() {
            return self.fneg(v.into_float_value()).into();
        }
        let x = v.into_int_value();
        let (r, flag) = self.checked("ssub", x.get_type().const_zero(), x);
        self.raise_if(flag, &format!("{} overflow", s.name()));
        r.into()
    }

    /// Purpose: This function converts a value to a number type, where either
    /// the value's type or the target is a sized number type. It raises when
    /// the value does not fit, or a string is not a number.
    ///
    /// Inputs:
    /// - `to`: the target: `int`, `float` or a sized type.
    /// - `from`: the value's type: a number type, `bool`, `char` or `str`.
    /// - `v`: the value.
    ///
    /// Outputs: the converted value.
    pub fn convert(&self, to: &Ty, from: &Ty, v: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        let i64t = self.cx.i64t();
        let to_float = matches!(to, Ty::Float | Ty::Sized(SizedNum::Float32));
        let src = match from {
            Ty::Int => Src::Signed(v.into_int_value()),
            Ty::Float => Src::Float(v.into_float_value()),
            Ty::Sized(SizedNum::Float32) => Src::Float(self.widen(v.into_float_value())),
            Ty::Sized(SizedNum::ULong) => Src::Unsigned(v.into_int_value()),
            Ty::Sized(s) if s.is_unsigned() => Src::Signed(self.zext(v.into_int_value(), i64t)),
            Ty::Sized(_) => Src::Signed(self.sext(v.into_int_value(), i64t)),
            Ty::Bool => Src::Signed(self.zext(v.into_int_value(), i64t)),
            Ty::Char => Src::Signed(self.zext(self.char_cp(v.into_int_value()), i64t)),
            Ty::Str if to_float => {
                Src::Float(self.callv(self.cx.tofloat_str_fn(), &[v]).into_float_value())
            }
            Ty::Str => Src::Signed(self.call_int(self.cx.toint_str_fn(), &[v])),
            _ => Src::Signed(self.i64(0)),
        };
        match to {
            Ty::Float => match src {
                Src::Signed(w) => self.itof(w).into(),
                Src::Unsigned(u) => self
                    .b
                    .build_unsigned_int_to_float(u, self.cx.f64t(), "")
                    .expect("positioned")
                    .into(),
                Src::Float(x) => x.into(),
            },
            Ty::Sized(SizedNum::Float32) => {
                let f32t = self.cx.f32t();
                match src {
                    Src::Signed(w) => {
                        self.b.build_signed_int_to_float(w, f32t, "").expect("positioned").into()
                    }
                    Src::Unsigned(u) => {
                        self.b.build_unsigned_int_to_float(u, f32t, "").expect("positioned").into()
                    }
                    Src::Float(x) => {
                        // A finite float past float32's range does not fit;
                        // infinity and NaN stay what they are.
                        let max = self.f64(f32::MAX as f64);
                        let inf = self.f64(f64::INFINITY);
                        let ninf = self.f64(f64::NEG_INFINITY);
                        let high = self.and(
                            self.fcmp(FloatPredicate::OGT, x, max),
                            self.fcmp(FloatPredicate::OLT, x, inf),
                        );
                        let low = self.and(
                            self.fcmp(FloatPredicate::OLT, x, self.fneg(max)),
                            self.fcmp(FloatPredicate::OGT, x, ninf),
                        );
                        self.if_(self.or(high, low), |f| {
                            f.raise_unfit(
                                f.call_ptr(f.cx.float_tostr_fn(), &[x.into()]),
                                "float32",
                                "",
                            );
                        });
                        self.b.build_float_trunc(x, f32t, "").expect("positioned").into()
                    }
                }
            }
            Ty::Sized(s) => self.to_int_type(src, *s).into(),
            _ => self.to_range(src, i64::MIN as i128, i64::MAX as i128, 64, false, "int").into(),
        }
    }

    /// Purpose: This function converts a number read for conversion to an
    /// integer type, raising when it does not fit.
    ///
    /// Inputs:
    /// - `src`: the number.
    /// - `s`: the target, a sized integer type.
    ///
    /// Outputs: the converted value.
    fn to_int_type(&self, src: Src<'ctx>, s: SizedNum) -> IntValue<'ctx> {
        let (lo, hi) = s.range().unwrap_or((0, 0));
        self.to_range(src, lo, hi, s.bits(), s.is_unsigned(), s.name())
    }

    /// Purpose: This function converts a number to an integer type given by
    /// its range, raising when the value is outside it.
    ///
    /// Inputs:
    /// - `src`: the number.
    /// - `lo`, `hi`: the type's range.
    /// - `bits`: its width.
    /// - `unsigned`: whether it is unsigned.
    /// - `name`: its name, for the message.
    ///
    /// Outputs: the converted value, at the type's width.
    fn to_range(
        &self,
        src: Src<'ctx>,
        lo: i128,
        hi: i128,
        bits: u32,
        unsigned: bool,
        name: &str,
    ) -> IntValue<'ctx> {
        let it = self.cx.ctx.custom_width_int_type(bits);
        let holds = format!(", which holds {lo} to {hi}");
        match src {
            Src::Signed(w) => {
                let mut ok = self.bool(true);
                if lo > i64::MIN as i128 {
                    ok = self.and(ok, self.sge(w, self.i64(lo as i64)));
                }
                if hi < i64::MAX as i128 {
                    ok = self.and(ok, self.icmp(IntPredicate::SLE, w, self.i64(hi as i64)));
                }
                self.if_(self.not(ok), |f| f.raise_unfit(f.int_text(w), name, &holds));
                self.fit(w, it)
            }
            Src::Unsigned(u) => {
                if hi >= u64::MAX as i128 {
                    return u;
                }
                let ok = self.icmp(IntPredicate::ULE, u, self.i64(hi as u64 as i64));
                self.if_(self.not(ok), |f| f.raise_unfit(f.uint_text(u), name, &holds));
                self.fit(u, it)
            }
            Src::Float(x) => {
                // Truncation toward zero keeps any value above lo - 1 and
                // below hi + 1. For a 64-bit signed type lo - 1 is not a float,
                // and the test is x >= lo instead.
                let lower = if bits == 64 && !unsigned {
                    self.fcmp(FloatPredicate::OGE, x, self.f64(lo as f64))
                } else {
                    self.fcmp(FloatPredicate::OGT, x, self.f64(lo as f64 - 1.0))
                };
                let upper = self.fcmp(FloatPredicate::OLT, x, self.f64((hi + 1) as f64));
                let ok = self.and(lower, upper);
                self.if_(self.not(ok), |f| {
                    f.raise_unfit(f.call_ptr(f.cx.float_tostr_fn(), &[x.into()]), name, &holds)
                });
                let intr = if unsigned { "llvm.fptoui.sat" } else { "llvm.fptosi.sat" };
                let decl = inkwell::intrinsics::Intrinsic::find(intr)
                    .expect("intrinsic")
                    .get_declaration(&self.cx.module, &[it.into(), self.cx.f64t().into()])
                    .expect("declaration");
                self.call_int(decl, &[x.into()])
            }
        }
    }

    /// Purpose: This function raises `<value> does not fit a <type>`.
    ///
    /// Inputs:
    /// - `text`: the value's text, owned; it is released.
    /// - `name`: the type's name.
    /// - `holds`: what follows, such as `, which holds 0 to 255`.
    ///
    /// Outputs: none.
    fn raise_unfit(&self, text: inkwell::values::PointerValue<'ctx>, name: &str, holds: &str) {
        let sb = self.sb_new();
        self.sb_push_str(sb, text);
        self.release(&Ty::Str, text.into());
        let a = crate::typechecker::infer::with_article(name);
        self.sb_push_lit(sb, &format!(" does not fit {a}{holds}"));
        let m = self.sb_finish(sb);
        self.raise_str(m);
    }

    /// Purpose: This function narrows an int to a width, or keeps it when the
    /// width is its own.
    ///
    /// Inputs:
    /// - `v`: the int.
    /// - `t`: the width's type.
    ///
    /// Outputs: the int at that width.
    fn fit(&self, v: IntValue<'ctx>, t: IntType<'ctx>) -> IntValue<'ctx> {
        if v.get_type().get_bit_width() == t.get_bit_width() { v } else { self.trunc(v, t) }
    }

    /// Purpose: This function widens a float32 to a float.
    fn widen(&self, x: FloatValue<'ctx>) -> FloatValue<'ctx> {
        self.b.build_float_ext(x, self.cx.f64t(), "").expect("positioned")
    }
}
