//! `std::math`: each function for the operand kinds the lowering settled.
//!
//! Most are one LLVM intrinsic, which compiles to an instruction or a call into
//! the C math library. The few LLVM 18 has no intrinsic for, such as `tan` and
//! `atan2`, call the C library directly. Results match v1's, which used Rust's
//! `f64` methods: `round` takes halves away from zero, a float becomes an int
//! by saturating, and `min` and `max` of a NaN and a number give the number.

use inkwell::values::{BasicValueEnum, FloatValue, IntValue};

use super::F;
use crate::mir::{MathFn, Ty};

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function calls an LLVM intrinsic overloaded on `f64`.
    ///
    /// Inputs:
    /// - `name`: the intrinsic, such as `llvm.floor`.
    /// - `args`: its arguments.
    ///
    /// Outputs: its result.
    fn float_intrinsic(&self, name: &str, args: &[BasicValueEnum<'ctx>]) -> BasicValueEnum<'ctx> {
        let intr = inkwell::intrinsics::Intrinsic::find(name).expect("LLVM has the intrinsic");
        let decl =
            intr.get_declaration(&self.cx.module, &[self.cx.f64t().into()]).expect("declaration");
        self.callv(decl, args)
    }

    /// Purpose: This function turns a float into an int, saturating at the
    /// ends and giving 0 for NaN.
    pub fn float_to_int_sat(&self, v: FloatValue<'ctx>) -> IntValue<'ctx> {
        let intr = inkwell::intrinsics::Intrinsic::find("llvm.fptosi.sat").expect("intrinsic");
        let decl = intr
            .get_declaration(&self.cx.module, &[self.cx.i64t().into(), self.cx.f64t().into()])
            .expect("declaration");
        self.call_int(decl, &[v.into()])
    }

    /// Purpose: This function computes a `std::math` function.
    ///
    /// Inputs:
    /// - `m`: the function.
    /// - `args`: its operands, of the kinds `m` names.
    ///
    /// Outputs: the result and its type. `PowInt` and `AbsInt` raise on overflow.
    pub fn math(&self, m: MathFn, args: &[BasicValueEnum<'ctx>]) -> (BasicValueEnum<'ctx>, Ty) {
        let fl = |i: usize| args[i].into_float_value();
        let int = |i: usize| args[i].into_int_value();
        let intr = |name: &str| self.float_intrinsic(name, args);
        let libm = |name: &str| self.callv(self.cx.libc(name), args);
        let float = |v: BasicValueEnum<'ctx>| (v, Ty::Float);
        match m {
            MathFn::Floor | MathFn::Ceil | MathFn::Round | MathFn::Trunc => {
                let name = match m {
                    MathFn::Floor => "llvm.floor",
                    MathFn::Ceil => "llvm.ceil",
                    MathFn::Round => "llvm.round",
                    _ => "llvm.trunc",
                };
                let r = intr(name).into_float_value();
                (self.float_to_int_sat(r).into(), Ty::Int)
            }
            MathFn::Sqrt => float(intr("llvm.sqrt")),
            MathFn::Ln => float(intr("llvm.log")),
            MathFn::Log2 => float(intr("llvm.log2")),
            MathFn::Log10 => float(intr("llvm.log10")),
            MathFn::Exp => float(intr("llvm.exp")),
            MathFn::Sin => float(intr("llvm.sin")),
            MathFn::Cos => float(intr("llvm.cos")),
            MathFn::Pow => float(intr("llvm.pow")),
            MathFn::Tan => float(libm("tan")),
            MathFn::Asin => float(libm("asin")),
            MathFn::Acos => float(libm("acos")),
            MathFn::Atan => float(libm("atan")),
            MathFn::Atan2 => float(libm("atan2")),
            MathFn::Hypot => float(libm("hypot")),
            MathFn::PowI => {
                // v1 passed the exponent as an i32, so a larger one wraps.
                let e = self.trunc(int(1), self.cx.i32t());
                let i = inkwell::intrinsics::Intrinsic::find("llvm.powi").expect("intrinsic");
                let decl = i
                    .get_declaration(
                        &self.cx.module,
                        &[self.cx.f64t().into(), self.cx.i32t().into()],
                    )
                    .expect("declaration");
                float(self.callv(decl, &[args[0], e.into()]))
            }
            MathFn::PowInt => (self.callv(self.cx.pow_int_fn(), args), Ty::Int),
            MathFn::MinInt => {
                (self.select_int(self.slt(int(0), int(1)), int(0), int(1)).into(), Ty::Int)
            }
            MathFn::MaxInt => {
                (self.select_int(self.sgt(int(0), int(1)), int(0), int(1)).into(), Ty::Int)
            }
            MathFn::MinFloat => float(intr("llvm.minnum")),
            MathFn::MaxFloat => float(intr("llvm.maxnum")),
            MathFn::AbsInt => {
                let x = int(0);
                self.raise_if(self.eq(x, self.i64(i64::MIN)), "integer overflow");
                let neg = self.sub(self.i64(0), x);
                (self.select_int(self.slt(x, self.i64(0)), neg, x).into(), Ty::Int)
            }
            MathFn::AbsFloat => float(intr("llvm.fabs")),
            MathFn::SignInt => {
                let x = int(0);
                let pos = self.zext(self.sgt(x, self.i64(0)), self.cx.i64t());
                let neg = self.zext(self.slt(x, self.i64(0)), self.cx.i64t());
                (self.sub(pos, neg).into(), Ty::Int)
            }
            MathFn::SignFloat => {
                // NaN stays NaN and both zeros give 0.0; anything else is ±1.0.
                let x = fl(0);
                let one = self
                    .float_intrinsic("llvm.copysign", &[self.f64(1.0).into(), x.into()])
                    .into_float_value();
                let zero = self.fcmp(inkwell::FloatPredicate::OEQ, x, self.f64(0.0));
                let nan = self.fcmp(inkwell::FloatPredicate::UNO, x, x);
                let r = self.select(zero, self.f64(0.0), one).into_float_value();
                float(self.select(nan, x, r))
            }
            MathFn::IsNan => {
                (self.fcmp(inkwell::FloatPredicate::UNO, fl(0), fl(0)).into(), Ty::Bool)
            }
            MathFn::IsInf => {
                let a = self.float_intrinsic("llvm.fabs", &[args[0]]).into_float_value();
                let inf = self.fcmp(inkwell::FloatPredicate::OEQ, a, self.f64(f64::INFINITY));
                (inf.into(), Ty::Bool)
            }
        }
    }
}

impl<'ctx> super::Cx<'ctx> {
    /// Purpose: This function returns `jade.math.pow_int(base, exp) -> int`,
    /// squaring and multiplying with overflow checks.
    fn pow_int_fn(&self) -> inkwell::values::FunctionValue<'ctx> {
        let i = self.i64t();
        self.support("jade.math.pow_int", Some(i.into()), &[i.into(), i.into()], |f| {
            let (base, exp) = (f.param_int(0), f.param_int(1));
            f.fail_if(f.slt(exp, f.i64(0)), "math.pow of two ints needs an exponent of 0 or more; pass a float to get a fraction");
            let acc = f.var(f.i64(1));
            let b = f.var(base);
            let e = f.var(exp);
            f.while_(
                |f| f.sgt(f.load_i64(e), f.i64(0)),
                |f, _| {
                    let odd = f.ne(f.and(f.load_i64(e), f.i64(1)), f.i64(0));
                    f.if_(odd, |f| {
                        let (r, o) = f.checked("smul", f.load_i64(acc), f.load_i64(b));
                        f.fail_if(o, "integer overflow");
                        f.store(acc, r);
                    });
                    let rest = f.ashr(f.load_i64(e), f.i64(1));
                    f.store(e, rest);
                    f.if_(f.sgt(rest, f.i64(0)), |f| {
                        let (r, o) = f.checked("smul", f.load_i64(b), f.load_i64(b));
                        f.fail_if(o, "integer overflow");
                        f.store(b, r);
                    });
                },
            );
            f.ret(f.load_i64(acc));
        })
    }
}
