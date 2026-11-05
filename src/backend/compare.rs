//! Equality and ordering, per type.
//!
//! `==` works on every type: numbers, bools and chars by value, strings by
//! their bytes, arrays element by element, dicts entry by entry, optionals by
//! presence and then value, and structs by identity, since a struct is a
//! reference. Ordering exists for numbers, strings, chars and bools, which is
//! also what `sort` accepts.

use inkwell::values::{BasicValueEnum, FunctionValue, IntValue};
use inkwell::{FloatPredicate, IntPredicate};

use super::layout::{ARR_DATA, DICT_KEYS, DICT_VALS, LEN};
use super::{Cx, F};
use crate::mir::Ty;

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns `jade.eq.<type>(a, b) -> bool` for a type
    /// whose equality needs a loop.
    fn eq_fn(&self, t: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.eq.{}", super::rc::key(t));
        let lt = self.llty(t);
        let t = t.clone();
        self.support(&name, Some(self.i1t().into()), &[lt, lt], |f| {
            let (a, b) = (f.param(0), f.param(1));
            match &t {
                Ty::Array(el) => {
                    let (pa, pb) = (a.into_pointer_value(), b.into_pointer_value());
                    let len = f.load_i64(f.at_c(pa, LEN));
                    f.if_(f.ne(len, f.load_i64(f.at_c(pb, LEN))), |f| f.ret(f.bool(false)));
                    let et = f.cx.llty(el);
                    let (da, db) =
                        (f.load_ptr(f.at_c(pa, ARR_DATA)), f.load_ptr(f.at_c(pb, ARR_DATA)));
                    f.for_range(f.i64(0), len, |f, i, _| {
                        let x = f.load(et, f.elem(et, da, i));
                        let y = f.load(et, f.elem(et, db, i));
                        f.if_(f.not(f.eq_values(el, x, y)), |f| f.ret(f.bool(false)));
                    });
                    f.ret(f.bool(true));
                }
                Ty::Map(vt) => {
                    let (pa, pb) = (a.into_pointer_value(), b.into_pointer_value());
                    let len = f.load_i64(f.at_c(pa, LEN));
                    f.if_(f.ne(len, f.load_i64(f.at_c(pb, LEN))), |f| f.ret(f.bool(false)));
                    let et = f.cx.llty(vt);
                    let keys = f.load_ptr(f.at_c(pa, DICT_KEYS));
                    let vals = f.load_ptr(f.at_c(pa, DICT_VALS));
                    f.for_range(f.i64(0), len, |f, i, _| {
                        let k = f.load_ptr(f.elem(f.cx.ptrt(), keys, i));
                        let at = f.call_int(f.cx.dict_find_fn(), &[pb.into(), k.into()]);
                        f.if_(f.slt(at, f.i64(0)), |f| f.ret(f.bool(false)));
                        let x = f.load(et, f.elem(et, vals, i));
                        let bvals = f.load_ptr(f.at_c(pb, DICT_VALS));
                        let y = f.load(et, f.elem(et, bvals, at));
                        f.if_(f.not(f.eq_values(vt, x, y)), |f| f.ret(f.bool(false)));
                    });
                    f.ret(f.bool(true));
                }
                Ty::Opt(inner) => {
                    let (na, nb) = (f.is_nil(&t, a), f.is_nil(&t, b));
                    f.if_(f.and(na, nb), |f| f.ret(f.bool(true)));
                    f.if_(f.or(na, nb), |f| f.ret(f.bool(false)));
                    let (x, y) = (f.unwrap(&t, a), f.unwrap(&t, b));
                    f.ret(f.eq_values(inner, x, y));
                }
                _ => f.ret(f.bool(false)),
            }
        })
    }

    /// Purpose: This function returns `jade.qcmp.<type>(pa, pb) -> i32`, which
    /// compares two elements in memory, for `qsort`.
    pub fn qsort_cmp_fn(&self, t: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.qcmp.{}", super::rc::key(t));
        let ptr = self.ptrt();
        let t = t.clone();
        self.support(&name, Some(self.i32t().into()), &[ptr.into(), ptr.into()], |f| {
            let et = f.cx.llty(&t);
            let a = f.load(et, f.param_ptr(0));
            let b = f.load(et, f.param_ptr(1));
            f.ret(f.cmp_values(&t, a, b));
        })
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function compares two values of a type for equality.
    ///
    /// Inputs:
    /// - `t`: their type.
    /// - `a`, `b`: the values.
    ///
    /// Outputs: an `i1`.
    pub fn eq_values(
        &self,
        t: &Ty,
        a: BasicValueEnum<'ctx>,
        b: BasicValueEnum<'ctx>,
    ) -> IntValue<'ctx> {
        match t {
            Ty::Int | Ty::Bool | Ty::Char | Ty::Nil => {
                self.eq(a.into_int_value(), b.into_int_value())
            }
            Ty::Float => self.fcmp(FloatPredicate::OEQ, a.into_float_value(), b.into_float_value()),
            Ty::Str => self.call_int(self.cx.str_eq_fn(), &[a, b]),
            Ty::Struct(_) => {
                let ia = self
                    .b
                    .build_ptr_to_int(a.into_pointer_value(), self.cx.i64t(), "")
                    .expect("positioned");
                let ib = self
                    .b
                    .build_ptr_to_int(b.into_pointer_value(), self.cx.i64t(), "")
                    .expect("positioned");
                self.eq(ia, ib)
            }
            Ty::Array(_) | Ty::Map(_) | Ty::Opt(_) => self.call_int(self.cx.eq_fn(t), &[a, b]),
            _ => self.bool(false),
        }
    }

    /// Purpose: This function orders two values of an ordered type.
    ///
    /// Inputs:
    /// - `t`: their type: int, float, bool, char or str.
    /// - `a`, `b`: the values.
    ///
    /// Outputs: an `i32`: -1, 0 or 1.
    pub fn cmp_values(
        &self,
        t: &Ty,
        a: BasicValueEnum<'ctx>,
        b: BasicValueEnum<'ctx>,
    ) -> IntValue<'ctx> {
        match t {
            Ty::Str => self.call_int(self.cx.str_cmp_fn(), &[a, b]),
            Ty::Float => {
                let (x, y) = (a.into_float_value(), b.into_float_value());
                let lt = self.fcmp(FloatPredicate::OLT, x, y);
                let gt = self.fcmp(FloatPredicate::OGT, x, y);
                let r = self.select_int(gt, self.i32(1), self.i32(0));
                self.select_int(lt, self.i32(-1), r)
            }
            _ => {
                let (x, y) = (a.into_int_value(), b.into_int_value());
                // Ints compare signed; bools and chars have no sign to keep.
                let (pl, pg) = if *t == Ty::Int {
                    (IntPredicate::SLT, IntPredicate::SGT)
                } else {
                    (IntPredicate::ULT, IntPredicate::UGT)
                };
                let lt = self.icmp(pl, x, y);
                let gt = self.icmp(pg, x, y);
                let r = self.select_int(gt, self.i32(1), self.i32(0));
                self.select_int(lt, self.i32(-1), r)
            }
        }
    }
}
