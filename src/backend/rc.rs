//! Reference counting, emitted per type.
//!
//! Retaining is the same for every heap object: add one to the count at offset
//! 0, unless the object is immortal. Releasing differs by type, because the
//! last release frees what the object holds: a string frees itself, an array of
//! strings releases each string first, and a struct releases each field that
//! needs it. So each heap type gets its own `jade.release.<type>`, generated the
//! first time a program releases a value of that type.
//!
//! A struct value may hold any struct descending from its static type, so
//! struct release dispatches on the object's type id to that struct's own
//! `jade.destroy.<name>`.
//!
//! Counts are atomic because tasks will share one heap.

use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use super::layout::{ARR_DATA, DICT_INDEX, DICT_KEYS, DICT_VALS, LEN, TYPE_ID};
use super::{Cx, F};
use crate::mir::{Ty, is_heap};

/// Purpose: This function names a type uniquely for a support function's name.
///
/// Inputs:
/// - `t`: the type.
///
/// Outputs: a string no other type produces.
pub fn key(t: &Ty) -> String {
    format!("{t:?}")
}

/// Purpose: This function strips an optional down to the type it holds, for
/// release: a nil pointer releases as nothing.
fn base(t: &Ty) -> &Ty {
    match t {
        Ty::Opt(inner) => inner,
        other => other,
    }
}

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns `jade.retain(p)`.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the function. It ignores null and immortal objects.
    pub fn retain_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.retain", None, &[self.ptrt().into()], |f| {
            let p = f.param_ptr(0);
            f.if_(f.not(f.is_null(p)), |f| {
                let rc = f.load_i64_atomic(p);
                f.if_(f.sge(rc, f.i64(0)), |f| {
                    f.atomic_add(p, 1);
                });
            });
            f.ret_void();
        })
    }

    /// Purpose: This function returns the release function for a heap type.
    ///
    /// Inputs:
    /// - `t`: the type; an optional releases as the type it holds.
    ///
    /// Outputs: `jade.release.<type>(p)`.
    pub fn release_fn(&self, t: &Ty) -> FunctionValue<'ctx> {
        let t = base(t).clone();
        let name = match &t {
            Ty::Struct(_) => "jade.release.struct".to_string(),
            other => format!("jade.release.{}", key(other)),
        };
        self.support(&name, None, &[self.ptrt().into()], |f| {
            let p = f.param_ptr(0);
            f.if_(f.is_null(p), |f| f.ret_void());
            let rc = f.load_i64_atomic(p);
            f.if_(f.slt(rc, f.i64(0)), |f| f.ret_void());
            let old = f.atomic_add(p, -1);
            f.if_(f.eq(old, f.i64(1)), |f| f.destroy(&t, p));
            f.ret_void();
        })
    }

    /// Purpose: This function returns the function that frees one struct type's
    /// object, releasing its fields first.
    ///
    /// Inputs:
    /// - `name`: the struct.
    ///
    /// Outputs: `jade.destroy.<name>(p)`.
    fn destroy_struct_fn(&self, name: &str) -> FunctionValue<'ctx> {
        let fields = self.struct_fields(name);
        let st = self.struct_ty(name);
        self.support(&format!("jade.destroy.{name}"), None, &[self.ptrt().into()], |f| {
            let p = f.param_ptr(0);
            for (i, ft) in fields.iter().enumerate() {
                if is_heap(ft) {
                    let fp = f.field(st, p, i as u32 + 2);
                    let v = f.load(f.cx.llty(ft), fp);
                    f.release(ft, v);
                }
            }
            f.call(f.cx.libc("free"), &[p.into()]);
            f.ret_void();
        })
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function takes a reference to a value, when its type has one.
    ///
    /// Inputs:
    /// - `t`: the value's type.
    /// - `v`: the value.
    ///
    /// Outputs: none.
    pub fn retain(&self, t: &Ty, v: BasicValueEnum<'ctx>) {
        if is_heap(t) {
            self.call(self.cx.retain_fn(), &[v]);
        }
    }

    /// Purpose: This function gives up a reference to a value, freeing it when
    /// it was the last.
    ///
    /// Inputs:
    /// - `t`: the value's type.
    /// - `v`: the value.
    ///
    /// Outputs: none.
    pub fn release(&self, t: &Ty, v: BasicValueEnum<'ctx>) {
        if is_heap(t) {
            self.call(self.cx.release_fn(t), &[v]);
        }
    }

    /// Purpose: This function frees an object whose count reached zero.
    ///
    /// Inputs:
    /// - `t`: its type, not optional.
    /// - `p`: the object.
    ///
    /// Outputs: none.
    fn destroy(&self, t: &Ty, p: PointerValue<'ctx>) {
        let free = self.cx.libc("free");
        match t {
            Ty::Str => {
                self.call(free, &[p.into()]);
            }
            Ty::Array(el) => {
                let data = self.load_ptr(self.at_c(p, ARR_DATA));
                if is_heap(el) {
                    let len = self.load_i64(self.at_c(p, LEN));
                    let et = self.cx.llty(el);
                    self.for_range(self.i64(0), len, |f, i, _| {
                        let v = f.load(et, f.elem(et, data, i));
                        f.release(el, v);
                    });
                }
                self.call(free, &[data.into()]);
                self.call(free, &[p.into()]);
            }
            Ty::Map(vt) => {
                let len = self.load_i64(self.at_c(p, LEN));
                let keys = self.load_ptr(self.at_c(p, DICT_KEYS));
                let vals = self.load_ptr(self.at_c(p, DICT_VALS));
                let et = self.cx.llty(vt);
                self.for_range(self.i64(0), len, |f, i, _| {
                    let k = f.load(f.cx.ptrt(), f.elem(f.cx.ptrt(), keys, i));
                    f.release(&Ty::Str, k);
                    if is_heap(vt) {
                        let v = f.load(et, f.elem(et, vals, i));
                        f.release(vt, v);
                    }
                });
                for off in [DICT_KEYS, DICT_VALS, DICT_INDEX] {
                    let b = self.load_ptr(self.at_c(p, off));
                    self.call(free, &[b.into()]);
                }
                self.call(free, &[p.into()]);
            }
            Ty::Struct(_) => {
                let tid = self.load_i64(self.at_c(p, TYPE_ID));
                let default = self.block();
                let end = self.block();
                let names: Vec<String> = self.cx.structs.iter().map(|s| s.name.clone()).collect();
                let mut cases: Vec<(IntValue, inkwell::basic_block::BasicBlock)> = Vec::new();
                let mut bodies = Vec::new();
                for (i, n) in names.iter().enumerate() {
                    let b = self.block();
                    cases.push((self.i64(i as i64), b));
                    bodies.push((b, n.clone()));
                }
                self.switch(tid, default, &cases);
                for (b, n) in bodies {
                    self.position(b);
                    let d = self.cx.destroy_struct_fn(&n);
                    self.call(d, &[p.into()]);
                    self.br(end);
                }
                self.position(default);
                self.call(free, &[p.into()]);
                self.br(end);
                self.position(end);
            }
            _ => {}
        }
    }
}
