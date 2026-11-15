//! Calls into Rust packages.
//!
//! `#[jade::export]` gives every exported function an `extern "C"` wrapper,
//! `__jade_<crate>_<fn>(a0, …, out, err) -> i32`, and a companion,
//! `__jade_<crate>_<fn>__free(out, err)`. Each argument is passed as a pointer to
//! its C form and the result comes back through `out`, so nothing here depends
//! on a platform's rules for passing a struct or a narrow integer by value. The
//! C forms are the ones `jade-rs/src/c.rs` defines, built from the Jade type
//! alone:
//!
//! | Jade | C form |
//! |------|--------|
//! | `int`, `float`, a sized number | the number at its width |
//! | `bool` | `uint8_t`, 0 or 1 |
//! | `char` | `uint32_t`, the scalar without Jade's taint bit |
//! | `str`, `bytes`, `[T]` | `{ ptr, len, cap }`, of `T`'s C form for an array |
//! | `T?` | `{ uint8_t some; T value }` |
//! | a Rust struct's class | its fields' C forms, in order, as C lays them out |
//!
//! *Ownership.* What the program sends is lent for the call: a string argument
//! points at the Jade string's own bytes, and an array, optional or struct is
//! built in memory this code allocates and frees once the call returns. What
//! comes back is Rust's: it is copied into Jade values, text tainted as all text
//! from outside the program is, and then handed to the companion, which drops it
//! with Rust's allocator. So no allocator crosses the boundary.
//!
//! *Errors.* Status 1 means the call raised: an `Err`, a panic the wrapper
//! caught, or an argument Rust refused. The message in `err` is raised here as
//! any failing operation raises, after the companion frees it.
//!
//! The functions are linked in from the project's staticlib (`link.rs`); there
//! is nothing to load when the program runs.

use inkwell::types::{BasicTypeEnum, StructType};

use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};

use super::layout::{ARR_DATA, LEN, TYPE_ID};
use super::{Cx, F};
use crate::mir::{NativeCall, Ty};

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns the C form's LLVM type for a Jade type.
    ///
    /// Inputs:
    /// - `t`: the type, one a Rust package can export.
    ///
    /// Outputs: the type, laid out by LLVM as C lays it out.
    pub fn c_ty(&self, t: &Ty) -> BasicTypeEnum<'ctx> {
        match t {
            Ty::Bool | Ty::Nil => self.i8t().into(),
            Ty::Str | Ty::Bytes | Ty::Array(_) => self.c_vec_ty().into(),
            Ty::Opt(inner) => {
                self.ctx.struct_type(&[self.i8t().into(), self.c_ty(inner)], false).into()
            }
            Ty::Struct(class) => self.c_struct_ty(class).into(),
            other => self.llty(other),
        }
    }

    /// Purpose: This function returns `{ ptr, len, cap }`, the C form of a
    /// string, bytes or an array.
    fn c_vec_ty(&self) -> StructType<'ctx> {
        self.ctx.struct_type(&[self.ptrt().into(), self.i64t().into(), self.i64t().into()], false)
    }

    /// Purpose: This function returns a Rust struct's C form: its fields' C forms
    /// in order.
    fn c_struct_ty(&self, class: &str) -> StructType<'ctx> {
        let fields: Vec<BasicTypeEnum> =
            self.struct_fields(class).iter().map(|t| self.c_ty(t)).collect();
        self.ctx.struct_type(&fields, false)
    }

    /// Purpose: This function declares a function the staticlib defines.
    ///
    /// Inputs:
    /// - `name`: its symbol.
    /// - `params`: how many pointer parameters it takes.
    /// - `status`: whether it returns an `i32` status, or nothing.
    ///
    /// Outputs: the declaration.
    fn rust_extern(&self, name: &str, params: usize, status: bool) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function(name) {
            return f;
        }
        let ps: Vec<inkwell::types::BasicMetadataTypeEnum> =
            (0..params).map(|_| self.ptrt().into()).collect();
        let ty = if status {
            self.i32t().fn_type(&ps, false)
        } else {
            self.ctx.void_type().fn_type(&ps, false)
        };
        self.module.add_function(name, ty, Some(inkwell::module::Linkage::External))
    }

    /// Purpose: This function returns the support function behind one call into
    /// a Rust package: build each argument's C form, call the wrapper, raise or
    /// read the result back, and free what crossed.
    ///
    /// Inputs:
    /// - `c`: the call, with its static types.
    ///
    /// Outputs: `jade.rust.call.<symbol>(args…) -> ret`.
    pub fn native_call_fn(&self, c: &NativeCall) -> FunctionValue<'ctx> {
        let name = format!("jade.rust.call.{}", c.symbol);
        let c = c.clone();
        let params: Vec<_> = c.params.iter().map(|t| self.llty(t)).collect();
        self.support(&name, Some(self.llty(&c.ret)), &params, |f| {
            let n = c.params.len();
            let call = f.cx.rust_extern(&c.symbol, n + 2, true);
            let free = f.cx.rust_extern(&format!("{}__free", c.symbol), 2, false);
            let mut slots = Vec::with_capacity(n);
            for (i, t) in c.params.iter().enumerate() {
                let slot = f.alloca(f.cx.c_ty(t));
                f.call(f.cx.to_c_fn(t), &[f.param(i as u32), slot.into()]);
                slots.push(slot);
            }
            let out = f.alloca(f.cx.c_ty(&c.ret));
            f.store(out, super::layout::zero(f.cx.c_ty(&c.ret)));
            let err = f.alloca(f.cx.c_vec_ty());
            f.store(err, f.cx.c_vec_ty().const_zero());
            let mut args: Vec<BasicValueEnum<'ctx>> = slots.iter().map(|s| (*s).into()).collect();
            args.push(out.into());
            args.push(err.into());
            let status = f.call_int(call, &args);
            for (t, slot) in c.params.iter().zip(&slots) {
                if needs_release(t) {
                    f.call(f.cx.release_c_fn(t), &[(*slot).into()]);
                }
            }
            f.if_(f.ne(status, f.i32(0)), |f| {
                let ptr = f.load_ptr(err);
                let len = f.load_i64(f.at_c(err, 8));
                let m = f.str_from_trust(ptr, len, f.i64(1));
                f.call(free, &[f.null().into(), err.into()]);
                f.fail_str(m);
            });
            let r = f.callv(f.cx.from_c_fn(&c.ret), &[out.into()]);
            f.call(free, &[out.into(), f.null().into()]);
            f.ret(r);
        })
    }

    /// Purpose: This function returns `jade.rust.to.<T>(v, slot)`, which writes
    /// the C form of a Jade value into `slot`, lending its text and allocating
    /// what an array needs.
    fn to_c_fn(&self, t: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.rust.to.{}", super::rc::key(t));
        let t = t.clone();
        let params = [self.llty(&t), self.ptrt().into()];
        self.support(&name, None, &params, |f| {
            let (v, slot) = (f.param(0), f.param_ptr(1));
            match &t {
                Ty::Bool => f.store(slot, f.zext(v.into_int_value(), f.cx.i8t())),
                Ty::Char => f.store(slot, f.char_cp(v.into_int_value())),
                Ty::Str | Ty::Bytes => {
                    let s = v.into_pointer_value();
                    f.store(slot, f.str_data(s));
                    f.store(f.at_c(slot, 8), f.str_len(s));
                    f.store(f.at_c(slot, 16), f.i64(0));
                }
                Ty::Array(el) => {
                    let a = v.into_pointer_value();
                    let n = f.load_i64(f.at_c(a, LEN));
                    let src = f.load_ptr(f.at_c(a, ARR_DATA));
                    let (jt, ct) = (f.cx.llty(el), f.cx.c_ty(el));
                    let bytes = f.mul(f.select_int(f.eq(n, f.i64(0)), f.i64(1), n), f.size_of(ct));
                    let buf = f.call_ptr(f.cx.libc("malloc"), &[bytes.into()]);
                    f.for_range(f.i64(0), n, |f, i, _| {
                        let x = f.load(jt, f.elem(jt, src, i));
                        f.call(f.cx.to_c_fn(el), &[x.into(), f.elem(ct, buf, i).into()]);
                    });
                    f.store(slot, buf);
                    f.store(f.at_c(slot, 8), n);
                    f.store(f.at_c(slot, 16), f.i64(0));
                }
                Ty::Opt(inner) => {
                    let st = f.cx.c_ty(&t).into_struct_type();
                    f.store(slot, super::layout::zero(st.into()));
                    f.if_(f.not(f.is_nil(&t, v)), |f| {
                        f.store(f.field(st, slot, 0), f.i8(1));
                        let x = f.unwrap(&t, v);
                        f.call(f.cx.to_c_fn(inner), &[x.into(), f.field(st, slot, 1).into()]);
                    });
                }
                Ty::Struct(class) => {
                    let o = v.into_pointer_value();
                    let (obj, cst) = (f.cx.struct_ty(class), f.cx.c_struct_ty(class));
                    for (i, ft) in f.cx.struct_fields(class).iter().enumerate() {
                        let x = f.load(f.cx.llty(ft), f.field(obj, o, i as u32 + 2));
                        let at = f.field(cst, slot, i as u32);
                        f.call(f.cx.to_c_fn(ft), &[x.into(), at.into()]);
                    }
                }
                _ => f.store(slot, v),
            }
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.rust.release.<T>(slot)`, which frees
    /// what [`Cx::to_c_fn`] allocated: an array's element buffer, after what its
    /// elements hold. Text is lent and left alone.
    fn release_c_fn(&self, t: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.rust.release.{}", super::rc::key(t));
        let t = t.clone();
        self.support(&name, None, &[self.ptrt().into()], |f| {
            let slot = f.param_ptr(0);
            match &t {
                Ty::Array(el) => {
                    let buf = f.load_ptr(slot);
                    if needs_release(el) {
                        let ct = f.cx.c_ty(el);
                        f.for_range(f.i64(0), f.load_i64(f.at_c(slot, 8)), |f, i, _| {
                            f.call(f.cx.release_c_fn(el), &[f.elem(ct, buf, i).into()]);
                        });
                    }
                    f.call(f.cx.libc("free"), &[buf.into()]);
                }
                Ty::Opt(inner) if needs_release(inner) => {
                    let st = f.cx.c_ty(&t).into_struct_type();
                    let some = f.ne(f.load_i8(f.field(st, slot, 0)), f.i8(0));
                    f.if_(some, |f| {
                        f.call(f.cx.release_c_fn(inner), &[f.field(st, slot, 1).into()]);
                    });
                }
                Ty::Struct(class) => {
                    let cst = f.cx.c_struct_ty(class);
                    for (i, ft) in f.cx.struct_fields(class).iter().enumerate() {
                        if needs_release(ft) {
                            let at = f.field(cst, slot, i as u32);
                            f.call(f.cx.release_c_fn(ft), &[at.into()]);
                        }
                    }
                }
                _ => {}
            }
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.rust.from.<T>(slot) -> T`, which
    /// copies a C form Rust handed over into a new Jade value. Text and chars are
    /// tainted.
    fn from_c_fn(&self, t: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.rust.from.{}", super::rc::key(t));
        let t = t.clone();
        self.support(&name, Some(self.llty(&t)), &[self.ptrt().into()], |f| {
            let slot = f.param_ptr(0);
            let r: BasicValueEnum<'ctx> = match &t {
                Ty::Nil => f.i8(0).into(),
                Ty::Bool => f.ne(f.load_i8(slot), f.i8(0)).into(),
                Ty::Char => f.char_with_trust(f.load_i32(slot), f.i64(1)).into(),
                Ty::Str | Ty::Bytes => {
                    let ptr = f.load_ptr(slot);
                    let len = f.load_i64(f.at_c(slot, 8));
                    f.str_from_trust(ptr, len, f.i64(1)).into()
                }
                Ty::Array(el) => {
                    let ptr = f.load_ptr(slot);
                    let n = f.load_i64(f.at_c(slot, 8));
                    let ct = f.cx.c_ty(el);
                    let arr = f.array_new(el, n);
                    f.for_range(f.i64(0), n, |f, i, _| {
                        let x = f.callv(f.cx.from_c_fn(el), &[f.elem(ct, ptr, i).into()]);
                        f.array_push_owned(el, arr, x);
                    });
                    arr.into()
                }
                Ty::Opt(inner) => {
                    let st = f.cx.c_ty(&t).into_struct_type();
                    let none = f.eq(f.load_i8(f.field(st, slot, 0)), f.i8(0));
                    f.if_(none, |f| f.ret(super::layout::zero(f.cx.llty(&t))));
                    let x = f.callv(f.cx.from_c_fn(inner), &[f.field(st, slot, 1).into()]);
                    f.wrap(&t, x)
                }
                Ty::Struct(class) => {
                    let (obj, cst) = (f.cx.struct_ty(class), f.cx.c_struct_ty(class));
                    let o = f.call_ptr(f.cx.libc("malloc"), &[f.size_of(obj.into()).into()]);
                    f.store(o, f.i64(1));
                    f.store(f.at_c(o, TYPE_ID), f.i64(f.cx.struct_id(class) as i64));
                    for (i, ft) in f.cx.struct_fields(class).iter().enumerate() {
                        let at = f.field(cst, slot, i as u32);
                        let x = f.callv(f.cx.from_c_fn(ft), &[at.into()]);
                        f.store(f.field(obj, o, i as u32 + 2), x);
                    }
                    o.into()
                }
                other => f.load(f.cx.llty(other), slot),
            };
            f.ret(r);
        })
    }
}

/// Purpose: This function says whether a C form the program built holds memory
/// it allocated, which only an array does.
fn needs_release(t: &Ty) -> bool {
    match t {
        Ty::Array(_) => true,
        Ty::Opt(inner) => needs_release(inner),
        Ty::Struct(_) => true,
        _ => false,
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function copies a C string into a new Jade string.
    pub fn c_text(&self, p: PointerValue<'ctx>) -> PointerValue<'ctx> {
        let n = self.call_int(self.cx.libc("strlen"), &[p.into()]);
        self.call_ptr(self.cx.str_from_fn(), &[p.into(), n.into()])
    }
}
