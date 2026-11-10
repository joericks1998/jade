//! Native packages: shared libraries a program loads and calls at run time.
//!
//! A native package exports `jade_pkg_init`, which fills in a table of named
//! functions, and each function trades 16-byte `JadeVal`s: `int f(size_t argc,
//! const JadeVal* argv, JadeVal* out)`. That boundary is v1's, unchanged, so a
//! package built for v1 loads here too; `runtime_aot/runtime.h` documents it.
//! What changes is the Jade side. The checker gives every call static types
//! (`compiler::infer::native`), so each call marshals its arguments by those
//! types and reads its result back as one, and a result of the wrong kind
//! raises rather than flowing on.
//!
//! *Loading.* A package is opened the first time the program calls it, from
//! `JADE_LIBS` or the `libs/` beside the program when it came from the
//! project's dependencies, and from where it was at build time otherwise. Its
//! value ABI must match this one, as in v1.
//!
//! *Ownership.* What Jade sends is Jade's: a top-level string is lent for the
//! call, and everything nested is a copy Jade frees afterwards. What comes back
//! is copied into Jade values, tainted, and the package's tree freed, never the
//! pointee of a handle. A handle is `{ rc, ptr, name }`, and releasing one frees
//! only the wrapper.
//!
//! *Callbacks.* A Jade function goes over as a `JadeFn` whose `invoke` is a
//! trampoline generated for the function's static type. A raise inside it is
//! left in the error slot and reported through the status, so it never unwinds
//! through the library's frames, and the call that was running raises it once
//! the library returns. A `JadeFn` lives until the program exits, since a
//! library may keep it, and one is made per function rather than per call.

use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use super::layout::{ARR_DATA, LEN, TYPE_ID};
use super::{Cx, F};
use crate::compiler::infer::FnTy;
use crate::mir::{NativeCall, Ty};

/// The `JADE_FFI_*` tags.
/// The version of the 16-byte `JadeVal` format a native package and the
/// program trade values in, which a package reports from
/// `jade_pkg_abi_version`. It was v1's `jade_runtime::RUNTIME_ABI_VERSION`, and
/// stays 5 so a package built for v1 loads unchanged.
pub const VALUE_ABI_VERSION: u32 = 5;

const NIL: i64 = 0;
const INT: i64 = 1;
const FLOAT: i64 = 2;
const BOOL: i64 = 3;
const STR: i64 = 4;
const ERROR: i64 = 5;
const ARRAY: i64 = 6;
const DICT: i64 = 7;
const STRUCT: i64 = 8;
const BYTES: i64 = 9;
const HANDLE: i64 = 10;
const FN: i64 = 11;
const CHAR: i64 = 12;

/// A `JadeVal` is 16 bytes: the tag at 0, a spare byte at 1 that a char's trust
/// rides in, and the payload at 8.
const VAL: i64 = 16;
const PAD: i64 = 1;
const DATA: i64 = 8;

/// A handle object's pointer and name.
pub const HANDLE_PTR: i64 = 8;
pub const HANDLE_NAME: i64 = 16;

/// `RTLD_NOW`, the same on macOS and Linux.
const RTLD_NOW: i64 = 2;

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns a process-wide global, made the first
    /// time with a zero value.
    fn native_global(
        &self,
        name: &str,
        t: inkwell::types::BasicTypeEnum<'ctx>,
    ) -> PointerValue<'ctx> {
        if let Some(g) = self.module.get_global(name) {
            return g.as_pointer_value();
        }
        let g = self.module.add_global(t, None, name);
        g.set_initializer(&super::layout::zero(t));
        g.set_linkage(inkwell::module::Linkage::Internal);
        g.as_pointer_value()
    }

    /// Purpose: This function returns the slot holding a package's function
    /// table once it is open: `{ ptr bindings, i64 count, i64 open }`.
    fn native_pkg_slot(&self, id: u32) -> PointerValue<'ctx> {
        let i = self.i64t();
        let t = self.ctx.struct_type(&[self.ptrt().into(), i.into(), i.into()], false);
        self.native_global(&format!("jade.native.pkg.{id}"), t.into())
    }

    /// Purpose: This function returns `jade.native.table.<id>() -> ptr`: the
    /// package's function table, opening it the first time.
    fn native_table_fn(&self, id: u32) -> FunctionValue<'ctx> {
        self.support(&format!("jade.native.table.{id}"), Some(self.ptrt().into()), &[], |f| {
            let slot = f.cx.native_pkg_slot(id);
            let lock = f.cx.native_global("jade.native.lock", f.cx.i64t().into());
            f.spin_lock(lock);
            let open = f.ne(f.load_i64(f.at_c(slot, 16)), f.i64(0));
            f.if_(f.not(open), |f| {
                f.call(f.cx.native_open_fn(id), &[]);
            });
            f.spin_unlock(lock);
            f.if_(f.err_pending(), |f| f.ret(f.null()));
            f.ret(slot);
        })
    }

    /// Purpose: This function returns `jade.native.open.<id>()`, which finds,
    /// opens and initializes a package, raising v1's messages when it cannot.
    fn native_open_fn(&self, id: u32) -> FunctionValue<'ctx> {
        let lib = self.natives.iter().find(|n| n.id == id).cloned();
        self.support(&format!("jade.native.open.{id}"), None, &[], |f| {
            let Some(lib) = lib else {
                f.fail("a native package the program never imported");
                return;
            };
            let path = f.native_path(&lib);
            f.if_(f.err_pending(), |f| f.ret_void());
            let h = f.call_ptr(f.cx.libc("dlopen"), &[f.str_data(path).into(), f.i32(RTLD_NOW).into()]);
            f.if_(f.is_null(h), |f| {
                let why = f.c_text(f.call_ptr(f.cx.libc("dlerror"), &[]));
                let m = f.str_cat(&[f.cx.lit("could not load native library '"), path, f.cx.lit("': "), why]);
                f.release(&Ty::Str, why.into());
                f.release(&Ty::Str, path.into());
                f.fail_str(m);
            });
            // A package built for another value ABI cannot be talked to.
            let abi = f.var(f.call_ptr(f.cx.libc("dlsym"), &[h.into(), f.cx.cstr("jade_pkg_abi_version").into()]));
            f.if_(f.is_null(f.load_ptr(abi)), |f| {
                f.store(abi, f.call_ptr(f.cx.libc("dlsym"), &[h.into(), f.cx.cstr("jrt_abi_version").into()]));
            });
            let abi = f.load_ptr(abi);
            f.if_(f.not(f.is_null(abi)), |f| {
                let ty = f.cx.i32t().fn_type(&[], false);
                let theirs = f.b.build_indirect_call(ty, abi, &[], "").expect("positioned");
                let theirs = theirs.try_as_basic_value().left_or_zero(f).into_int_value();
                let ours = VALUE_ABI_VERSION as i64;
                f.if_(f.ne(theirs, f.i32(ours)), |f| {
                    let older = f.icmp(inkwell::IntPredicate::ULT, theirs, f.i32(ours));
                    let a = f.str_cat(&[
                        f.cx.lit("native package '"),
                        path,
                        f.cx.lit("' speaks an older value ABI than this Jade. Rebuild it with this toolchain."),
                    ]);
                    let b = f.str_cat(&[
                        f.cx.lit("native package '"),
                        path,
                        f.cx.lit("' speaks a newer value ABI than this Jade. Upgrade with `jade upgrade`."),
                    ]);
                    let m = f.select(older, a, b).into_pointer_value();
                    let other = f.select(older, b, a).into_pointer_value();
                    f.release(&Ty::Str, other.into());
                    f.release(&Ty::Str, path.into());
                    f.fail_str(m);
                });
            });
            let init = f.call_ptr(f.cx.libc("dlsym"), &[h.into(), f.cx.cstr("jade_pkg_init").into()]);
            f.if_(f.is_null(init), |f| {
                let m = f.str_cat(&[
                    f.cx.lit("native library '"),
                    path,
                    f.cx.lit(
                        "' has no `jade_pkg_init`, so it is a plain C library rather than a Jade \
                         package.\n  Jade cannot load one directly — it needs a binding generated \
                         from the library's header:\n    jade pkg add <name> --path <the .dylib> \
                         --header <its header.h>",
                    ),
                ]);
                f.release(&Ty::Str, path.into());
                f.fail_str(m);
            });
            // `JadeNativePkg { const char* name; const JadeBinding* bindings; size_t count; }`.
            let pkg = f.alloca(f.cx.i64t().array_type(3));
            f.store(pkg, f.cx.i64t().array_type(3).const_zero());
            let ty = f.cx.i32t().fn_type(&[f.cx.ptrt().into()], false);
            let status = f.b.build_indirect_call(ty, init, &[pkg.into()], "").expect("positioned");
            let status = status.try_as_basic_value().left_or_zero(f).into_int_value();
            f.if_(f.ne(status, f.i32(0)), |f| {
                let m = f.str_cat(&[f.cx.lit("jade_pkg_init in '"), path, f.cx.lit("' returned an error")]);
                f.release(&Ty::Str, path.into());
                f.fail_str(m);
            });
            f.release(&Ty::Str, path.into());
            let slot = f.cx.native_pkg_slot(id);
            f.store(slot, f.load_ptr(f.at_c(pkg, 8)));
            f.store(f.at_c(slot, 8), f.load_i64(f.at_c(pkg, 16)));
            f.store(f.at_c(slot, 16), f.i64(1));
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.native.fn.<id>.<name>() -> ptr`:
    /// one function of a package, found by name the first time and kept.
    fn native_fn(&self, id: u32, name: &str) -> FunctionValue<'ctx> {
        let fname = format!("jade.native.fn.{id}.{name}");
        let name = name.to_string();
        self.support(&fname, Some(self.ptrt().into()), &[], |f| {
            let cache =
                f.cx.native_global(&format!("jade.native.ptr.{id}.{name}"), f.cx.ptrt().into());
            let have = f.load_ptr(cache);
            f.if_(f.not(f.is_null(have)), |f| f.ret(have));
            let slot = f.call_ptr(f.cx.native_table_fn(id), &[]);
            f.if_(f.err_pending(), |f| f.ret(f.null()));
            let table = f.load_ptr(slot);
            let n = f.load_i64(f.at_c(slot, 8));
            let want = f.cx.cstr(&name);
            f.for_range(f.i64(0), n, |f, i, _| {
                let entry = f.at(table, f.mul(i, f.i64(16)));
                let ename = f.load_ptr(entry);
                f.if_(f.not(f.is_null(ename)), |f| {
                    let same = f.call_int(f.cx.libc("strcmp"), &[ename.into(), want.into()]);
                    f.if_(f.eq(same, f.i32(0)), |f| {
                        let p = f.load_ptr(f.at_c(entry, 8));
                        f.store(cache, p);
                        f.ret(p);
                    });
                });
            });
            f.fail(&format!("native function '{name}' not found in package"));
        })
    }

    /// Purpose: This function returns the support function behind one native
    /// call: marshal, call, read the result back, release.
    ///
    /// Inputs:
    /// - `c`: the call, with its static types.
    ///
    /// Outputs: `jade.native.call.<id>.<fn>.<types>(args…) -> ret`.
    pub fn native_call_fn(&self, c: &NativeCall) -> FunctionValue<'ctx> {
        let key: Vec<String> = c.params.iter().map(super::rc::key).collect();
        let name = format!(
            "jade.native.call.{}.{}.{}.{}",
            c.pkg,
            c.func,
            key.join(","),
            super::rc::key(&c.ret)
        );
        let c = c.clone();
        let params: Vec<_> = c.params.iter().map(|t| self.llty(t)).collect();
        self.support(&name, Some(self.llty(&c.ret)), &params, |f| {
            let fp = f.call_ptr(f.cx.native_fn(c.pkg, &c.func), &[]);
            f.if_(f.err_pending(), |f| f.ret_zero());
            let n = c.params.len() as i64;
            let argv = f.alloca(f.cx.i8t().array_type((VAL * n.max(1)) as u32));
            for (i, t) in c.params.iter().enumerate() {
                let slot = f.at_c(argv, VAL * i as i64);
                f.call(f.cx.ffi_to_fn(t), &[f.param(i as u32), slot.into(), f.bool(false).into()]);
            }
            let out = f.alloca(f.cx.i8t().array_type(VAL as u32));
            f.store(out, f.i8(NIL));
            f.store(f.at_c(out, DATA), f.i64(0));
            let ty =
                f.cx.i32t()
                    .fn_type(&[f.cx.i64t().into(), f.cx.ptrt().into(), f.cx.ptrt().into()], false);
            let st =
                f.b.build_indirect_call(ty, fp, &[f.i64(n).into(), argv.into(), out.into()], "")
                    .expect("positioned");
            let status = st.try_as_basic_value().left_or_zero(f).into_int_value();
            let release_all = |f: &F<'_, 'ctx>| {
                for i in 0..n {
                    f.call(
                        f.cx.ffi_free_fn(),
                        &[f.at_c(argv, VAL * i).into(), f.bool(false).into()],
                    );
                }
                f.call(f.cx.ffi_free_fn(), &[out.into(), f.bool(false).into()]);
            };
            // A callback that raised has its error waiting; it is the one to
            // report, rather than the library's account of the failure.
            f.if_(f.err_pending(), |f| {
                release_all(f);
                f.ret_zero();
            });
            f.if_(f.ne(status, f.i32(0)), |f| {
                let tag = f.zext(f.load_i8(out), f.cx.i64t());
                let text = f.load_ptr(f.at_c(out, DATA));
                let has = f.and(
                    f.or(f.eq(tag, f.i64(STR)), f.eq(tag, f.i64(ERROR))),
                    f.not(f.is_null(text)),
                );
                let m = f.var(
                    f.cx.lit(&format!("native function '{}' returned a non-zero status", c.func)),
                );
                f.if_(has, |f| f.store(m, f.c_text(text)));
                let m = f.load_ptr(m);
                release_all(f);
                f.fail_str(m);
            });
            let r = f.callv(f.cx.ffi_from_fn(&c.ret), &[out.into(), f.cx.lit(&c.display).into()]);
            release_all(f);
            f.if_(f.err_pending(), |f| f.ret_zero());
            f.ret(r);
        })
    }

    // ── Jade to JadeVal ──────────────────────────────────────────────────

    /// Purpose: This function returns `jade.ffi.to.<T>(v, out, own)`, which
    /// writes a value into a `JadeVal`. A string is lent at the top and copied
    /// when `own` is set, as every nested one is; containers are deep copies.
    fn ffi_to_fn(&self, t: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.ffi.to.{}", super::rc::key(t));
        let t = t.clone();
        let params = [self.llty(&t), self.ptrt().into(), self.i1t().into()];
        self.support(&name, None, &params, |f| {
            let (v, out, own) = (f.param(0), f.param_ptr(1), f.param_int(2));
            let tag = |k: i64| f.store(out, f.i8(k));
            f.store(f.at_c(out, DATA), f.i64(0));
            match &t {
                Ty::Int => {
                    tag(INT);
                    f.store(f.at_c(out, DATA), v);
                }
                Ty::Float => {
                    tag(FLOAT);
                    f.store(f.at_c(out, DATA), v);
                }
                Ty::Bool => {
                    tag(BOOL);
                    f.store(f.at_c(out, DATA), f.zext(v.into_int_value(), f.cx.i8t()));
                }
                Ty::Char => {
                    tag(CHAR);
                    let c = v.into_int_value();
                    f.store(f.at_c(out, PAD), f.trunc(f.char_trust(c), f.cx.i8t()));
                    f.store(f.at_c(out, DATA), f.char_cp(c));
                }
                Ty::Str => {
                    tag(STR);
                    let s = v.into_pointer_value();
                    let lent = f.str_data(s);
                    let copy = f.var(lent);
                    f.if_(own, |f| f.store(copy, f.call_ptr(f.cx.libc("strdup"), &[lent.into()])));
                    f.store(f.at_c(out, DATA), f.load_ptr(copy));
                }
                Ty::Bytes => {
                    tag(BYTES);
                    let b = v.into_pointer_value();
                    let n = f.str_len(b);
                    let bx = f.malloc(16);
                    let data = f.call_ptr(
                        f.cx.libc("malloc"),
                        &[f.select_int(f.eq(n, f.i64(0)), f.i64(1), n).into()],
                    );
                    f.memcpy(data, f.str_data(b), n);
                    f.store(bx, data);
                    f.store(f.at_c(bx, 8), n);
                    f.store(f.at_c(out, DATA), bx);
                }
                Ty::Handle(_) => {
                    tag(HANDLE);
                    let h = v.into_pointer_value();
                    let hx = f.malloc(16);
                    f.store(hx, f.load_ptr(f.at_c(h, HANDLE_PTR)));
                    let name = f.load_ptr(f.at_c(h, HANDLE_NAME));
                    f.store(
                        f.at_c(hx, 8),
                        f.call_ptr(f.cx.libc("strdup"), &[f.str_data(name).into()]),
                    );
                    f.store(f.at_c(out, DATA), hx);
                }
                Ty::Opt(inner) => {
                    f.if_else(
                        f.is_nil(&t, v),
                        |f| f.store(out, f.i8(NIL)),
                        |f| {
                            let x = f.unwrap(&t, v);
                            f.call(f.cx.ffi_to_fn(inner), &[x, out.into(), own.into()]);
                        },
                    );
                }
                Ty::Array(el) => {
                    tag(ARRAY);
                    let a = v.into_pointer_value();
                    let n = f.load_i64(f.at_c(a, LEN));
                    let src = f.load_ptr(f.at_c(a, ARR_DATA));
                    let items = f.call_ptr(
                        f.cx.libc("malloc"),
                        &[f.mul(f.add(n, f.i64(1)), f.i64(VAL)).into()],
                    );
                    let et = f.cx.llty(el);
                    f.for_range(f.i64(0), n, |f, i, _| {
                        let x = f.load(et, f.elem(et, src, i));
                        let slot = f.at(items, f.mul(i, f.i64(VAL)));
                        f.call(f.cx.ffi_to_fn(el), &[x, slot.into(), f.bool(true).into()]);
                    });
                    let arr = f.malloc(16);
                    f.store(arr, items);
                    f.store(f.at_c(arr, 8), n);
                    f.store(f.at_c(out, DATA), arr);
                }
                Ty::Map(vt) => {
                    tag(DICT);
                    let d = v.into_pointer_value();
                    let keys = f.call_ptr(f.cx.dict_keys_fn(vt), &[d.into()]);
                    let n = f.load_i64(f.at_c(keys, LEN));
                    let kd = f.load_ptr(f.at_c(keys, ARR_DATA));
                    let ks = f.call_ptr(
                        f.cx.libc("malloc"),
                        &[f.mul(f.add(n, f.i64(1)), f.i64(8)).into()],
                    );
                    let vs = f.call_ptr(
                        f.cx.libc("malloc"),
                        &[f.mul(f.add(n, f.i64(1)), f.i64(VAL)).into()],
                    );
                    f.for_range(f.i64(0), n, |f, i, _| {
                        let k = f.load_ptr(f.elem(f.cx.ptrt(), kd, i));
                        f.store(
                            f.elem(f.cx.ptrt(), ks, i),
                            f.call_ptr(f.cx.libc("strdup"), &[f.str_data(k).into()]),
                        );
                        let x = f.dict_get(vt, d, k);
                        f.call(
                            f.cx.ffi_to_fn(vt),
                            &[x, f.at(vs, f.mul(i, f.i64(VAL))).into(), f.bool(true).into()],
                        );
                    });
                    f.release(&Ty::Array(Box::new(Ty::Str)), keys.into());
                    let m = f.malloc(24);
                    f.store(m, ks);
                    f.store(f.at_c(m, 8), vs);
                    f.store(f.at_c(m, 16), n);
                    f.store(f.at_c(out, DATA), m);
                }
                Ty::Struct(class) => {
                    tag(STRUCT);
                    let o = v.into_pointer_value();
                    let st = f.cx.struct_ty(class);
                    let fields = f.cx.struct_fields(class);
                    let names = f.cx.struct_field_names(class);
                    let n = fields.len() as i64;
                    let ks = f.call_ptr(f.cx.libc("malloc"), &[f.i64(8 * (n + 1)).into()]);
                    let vs = f.call_ptr(f.cx.libc("malloc"), &[f.i64(VAL * (n + 1)).into()]);
                    for (i, (ft, fname)) in fields.iter().zip(&names).enumerate() {
                        let i = i as i64;
                        f.store(
                            f.at_c(ks, 8 * i),
                            f.call_ptr(f.cx.libc("strdup"), &[f.cx.cstr(fname).into()]),
                        );
                        let x = f.load(f.cx.llty(ft), f.field(st, o, i as u32 + 2));
                        f.call(
                            f.cx.ffi_to_fn(ft),
                            &[x, f.at_c(vs, VAL * i).into(), f.bool(true).into()],
                        );
                    }
                    let shown = crate::compiler::infer::display_type_name(class).to_string();
                    let sx = f.malloc(32);
                    f.store(sx, f.call_ptr(f.cx.libc("strdup"), &[f.cx.cstr(&shown).into()]));
                    f.store(f.at_c(sx, 8), ks);
                    f.store(f.at_c(sx, 16), vs);
                    f.store(f.at_c(sx, 24), f.i64(n));
                    f.store(f.at_c(out, DATA), sx);
                }
                Ty::Fn(ft) => {
                    tag(FN);
                    let host = v.into_pointer_value();
                    let invoke = f.cx.ffi_invoke_fn(ft).as_global_value().as_pointer_value();
                    let jf = f.call_ptr(f.cx.jade_fn_fn(), &[host.into(), invoke.into()]);
                    f.store(f.at_c(out, DATA), jf);
                }
                Ty::Json => {
                    let j = v.into_pointer_value();
                    f.if_(f.is_null(j), |f| {
                        f.store(out, f.i8(NIL));
                        f.ret_void();
                    });
                    let kind = f.load_i64(f.at_c(j, super::json::KIND));
                    let w = f.load_i64(f.at_c(j, super::json::PAYLOAD));
                    let as_ptr = f.b.build_int_to_ptr(w, f.cx.ptrt(), "").expect("positioned");
                    let send = |f: &F<'_, 'ctx>, t: Ty, x: BasicValueEnum<'ctx>| {
                        f.call(f.cx.ffi_to_fn(&t), &[x, out.into(), own.into()]);
                        f.ret_void();
                    };
                    use super::json::{KIND_ARRAY, KIND_BOOL, KIND_FLOAT, KIND_INT, KIND_STR};
                    f.if_(f.eq(kind, f.i64(KIND_BOOL)), |f| {
                        send(f, Ty::Bool, f.trunc(w, f.cx.i1t()).into())
                    });
                    f.if_(f.eq(kind, f.i64(KIND_INT)), |f| send(f, Ty::Int, w.into()));
                    f.if_(f.eq(kind, f.i64(KIND_FLOAT)), |f| {
                        let x = f.b.build_bit_cast(w, f.cx.f64t(), "").expect("positioned");
                        send(f, Ty::Float, x)
                    });
                    f.if_(f.eq(kind, f.i64(KIND_STR)), |f| send(f, Ty::Str, as_ptr.into()));
                    f.if_(f.eq(kind, f.i64(KIND_ARRAY)), |f| {
                        send(f, Ty::Array(Box::new(Ty::Json)), as_ptr.into())
                    });
                    send(f, Ty::Map(Box::new(Ty::Json)), as_ptr.into());
                }
                // A future, an error or a stream has no JadeVal form.
                _ => tag(NIL),
            }
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.ffi.jade_fn(host, invoke) -> ptr`:
    /// the one `JadeFn` for a function and trampoline, made the first time.
    fn jade_fn_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.ffi.jade_fn", Some(p.into()), &[p.into(), p.into()], |f| {
            let (host, invoke) = (f.param_ptr(0), f.param_ptr(1));
            // A list of `{ host, invoke, next }`, read as a `JadeFn` by its
            // first two words.
            let head = f.cx.native_global("jade.ffi.fns", f.cx.ptrt().into());
            let lock = f.cx.native_global("jade.ffi.fns.lock", f.cx.i64t().into());
            f.spin_lock(lock);
            let node = f.var(f.load_ptr(head));
            f.while_(
                |f| f.not(f.is_null(f.load_ptr(node))),
                |f, _| {
                    let n = f.load_ptr(node);
                    let same = f.and(
                        f.ptr_eq(f.load_ptr(n), host),
                        f.ptr_eq(f.load_ptr(f.at_c(n, 8)), invoke),
                    );
                    f.if_(same, |f| {
                        f.spin_unlock(lock);
                        f.ret(n);
                    });
                    f.store(node, f.load_ptr(f.at_c(n, 16)));
                },
            );
            let n = f.malloc(24);
            f.store(n, host);
            f.store(f.at_c(n, 8), invoke);
            f.store(f.at_c(n, 16), f.load_ptr(head));
            f.store(head, n);
            f.spin_unlock(lock);
            f.ret(n);
        })
    }

    /// Purpose: This function returns the `invoke` for a function type:
    /// `jade.ffi.invoke.<T>(host, argc, argv, out) -> i32`, which reads the
    /// library's arguments as the function's parameter types, calls it, and
    /// writes back a scalar result. It returns 1 when the call cannot be made
    /// or raised, leaving any error in the slot.
    fn ffi_invoke_fn(&self, ft: &FnTy) -> FunctionValue<'ctx> {
        let name = format!("jade.ffi.invoke.{}", super::rc::key(&Ty::Fn(Box::new(ft.clone()))));
        let ft = ft.clone();
        let (p, i) = (self.ptrt(), self.i64t());
        self.support(
            &name,
            Some(self.i32t().into()),
            &[p.into(), i.into(), p.into(), p.into()],
            |f| {
                let (host, argc, argv, out) =
                    (f.param_ptr(0), f.param_int(1), f.param_ptr(2), f.param_ptr(3));
                f.store(out, f.i8(NIL));
                f.store(f.at_c(out, DATA), f.i64(0));
                let params: Vec<Ty> = ft.params.iter().map(crate::mir::lower::norm).collect();
                let ret = crate::mir::lower::norm(&ft.ret);
                f.if_(f.ne(argc, f.i64(params.len() as i64)), |f| f.ret(f.i32(1)));
                let mut vals: Vec<BasicValueEnum> = Vec::new();
                for (k, t) in params.iter().enumerate() {
                    let slot = f.at_c(argv, VAL * k as i64);
                    let v = f.callv(
                        f.cx.ffi_from_fn(t),
                        &[slot.into(), f.cx.lit("a callback's argument").into()],
                    );
                    f.if_(f.err_pending(), |f| {
                        for (pv, pt) in vals.iter().zip(&params) {
                            f.release(pt, *pv);
                        }
                        f.ret(f.i32(1));
                    });
                    vals.push(v);
                }
                let metas: Vec<inkwell::types::BasicMetadataTypeEnum> =
                    params.iter().map(|t| f.cx.llty(t).into()).collect();
                let fty = inkwell::types::BasicType::fn_type(&f.cx.llty(&ret), &metas, false);
                let args: Vec<inkwell::values::BasicMetadataValueEnum> =
                    vals.iter().map(|v| (*v).into()).collect();
                let cs = f.b.build_indirect_call(fty, host, &args, "").expect("positioned");
                let r = cs.try_as_basic_value().left_or_zero(f);
                for (v, t) in vals.iter().zip(&params) {
                    f.release(t, *v);
                }
                f.if_(f.err_pending(), |f| {
                    f.release(&ret, r);
                    f.ret(f.i32(1));
                });
                // Only a scalar comes back: anything else would have to be freed
                // inside the library's own frame.
                match &ret {
                    Ty::Int => {
                        f.store(out, f.i8(INT));
                        f.store(f.at_c(out, DATA), r);
                    }
                    Ty::Float => {
                        f.store(out, f.i8(FLOAT));
                        f.store(f.at_c(out, DATA), r);
                    }
                    Ty::Bool => {
                        f.store(out, f.i8(BOOL));
                        f.store(f.at_c(out, DATA), f.zext(r.into_int_value(), f.cx.i8t()));
                    }
                    Ty::Nil => {}
                    other => {
                        f.release(other, r);
                        f.ret(f.i32(1));
                    }
                }
                f.ret(f.i32(0));
            },
        )
    }

    /// Purpose: This function returns `jade.ffi.free(v, nested)`, which frees
    /// what a `JadeVal` owns. At the top, a string is lent and left alone;
    /// nested, every string is owned. A handle's pointee and a function's
    /// `JadeFn` are never freed.
    fn ffi_free_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.ffi.free", None, &[p.into(), self.i1t().into()], |f| {
            let (v, nested) = (f.param_ptr(0), f.param_int(1));
            let tag = f.zext(f.load_i8(v), f.cx.i64t());
            let data = f.load_ptr(f.at_c(v, DATA));
            let free = |f: &F<'_, 'ctx>, p: PointerValue<'ctx>| {
                f.call(f.cx.libc("free"), &[p.into()]);
            };
            let is = |k: i64| f.eq(tag, f.i64(k));
            f.if_(f.and(nested, f.or(is(STR), is(ERROR))), |f| free(f, data));
            f.if_(f.and(is(BYTES), f.not(f.is_null(data))), |f| {
                free(f, f.load_ptr(data));
                free(f, data);
            });
            f.if_(f.and(is(HANDLE), f.not(f.is_null(data))), |f| {
                free(f, f.load_ptr(f.at_c(data, 8)));
                free(f, data);
            });
            f.if_(f.and(is(ARRAY), f.not(f.is_null(data))), |f| {
                let items = f.load_ptr(data);
                f.for_range(f.i64(0), f.load_i64(f.at_c(data, 8)), |f, i, _| {
                    f.call(
                        f.cx.ffi_free_fn(),
                        &[f.at(items, f.mul(i, f.i64(VAL))).into(), f.bool(true).into()],
                    );
                });
                free(f, items);
                free(f, data);
            });
            // A dict and a struct share a shape past the struct's type name.
            let keyed = f.or(is(DICT), is(STRUCT));
            f.if_(f.and(keyed, f.not(f.is_null(data))), |f| {
                let base = f.select(is(STRUCT), f.at_c(data, 8), data).into_pointer_value();
                let (keys, vals) = (f.load_ptr(base), f.load_ptr(f.at_c(base, 8)));
                f.for_range(f.i64(0), f.load_i64(f.at_c(base, 16)), |f, i, _| {
                    free(f, f.load_ptr(f.elem(f.cx.ptrt(), keys, i)));
                    f.call(
                        f.cx.ffi_free_fn(),
                        &[f.at(vals, f.mul(i, f.i64(VAL))).into(), f.bool(true).into()],
                    );
                });
                free(f, keys);
                free(f, vals);
                f.if_(is(STRUCT), |f| free(f, f.load_ptr(data)));
                free(f, data);
            });
            f.ret_void();
        })
    }

    // ── JadeVal to Jade ──────────────────────────────────────────────────

    /// Purpose: This function returns `jade.ffi.from.<T>(v, what) -> T`, which
    /// reads a `JadeVal` as a static type, raising when it holds another kind.
    /// Text and bytes are copied and tainted; an error value raises its
    /// message.
    fn ffi_from_fn(&self, t: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.ffi.from.{}", super::rc::key(t));
        let t = t.clone();
        let p = self.ptrt();
        self.support(&name, Some(self.llty(&t)), &[p.into(), p.into()], |f| {
            let (v, what) = (f.param_ptr(0), f.param_ptr(1));
            let tag = f.zext(f.load_i8(v), f.cx.i64t());
            let data = f.at_c(v, DATA);
            f.if_(f.eq(tag, f.i64(ERROR)), |f| {
                let text = f.load_ptr(data);
                let m = f.var(f.cx.lit("native error"));
                f.if_(f.not(f.is_null(text)), |f| f.store(m, f.c_text(text)));
                f.fail_str(f.load_ptr(m));
            });
            let expect = |f: &F<'_, 'ctx>, want: i64| {
                f.if_(f.ne(tag, f.i64(want)), |f| f.ffi_mismatch(what, tag, &t));
            };
            match &t {
                Ty::Int => {
                    expect(f, INT);
                    f.ret(f.load_i64(data));
                }
                Ty::Float => {
                    // An int widens, as a C double parameter would take one.
                    f.if_(f.eq(tag, f.i64(INT)), |f| f.ret(f.itof(f.load_i64(data))));
                    expect(f, FLOAT);
                    f.ret(f.load_f64(data));
                }
                Ty::Bool => {
                    expect(f, BOOL);
                    f.ret(f.ne(f.load_i8(data), f.i8(0)));
                }
                Ty::Char => {
                    expect(f, CHAR);
                    let cp = f.load_i32(data);
                    let bad = f.or(
                        f.icmp(inkwell::IntPredicate::UGT, cp, f.i32(0x10FFFF)),
                        f.and(
                            f.icmp(inkwell::IntPredicate::UGE, cp, f.i32(0xD800)),
                            f.icmp(inkwell::IntPredicate::ULE, cp, f.i32(0xDFFF)),
                        ),
                    );
                    f.fail_if(bad, "native function returned a char that is not a Unicode scalar");
                    f.ret(f.char_with_trust(cp, f.i64(1)));
                }
                Ty::Str => {
                    expect(f, STR);
                    let text = f.load_ptr(data);
                    f.if_(f.is_null(text), |f| f.ret(f.cx.lit("")));
                    let s = f.c_text(text);
                    f.set_trust(s, f.i64(1));
                    f.ret(s);
                }
                Ty::Nil => f.ret(f.i8(0)),
                Ty::Bytes => {
                    expect(f, BYTES);
                    let bx = f.load_ptr(data);
                    f.if_(f.is_null(bx), |f| f.ret(f.str_new(f.i64(0))));
                    let src = f.load_ptr(bx);
                    let n = f.select_int(f.is_null(src), f.i64(0), f.load_i64(f.at_c(bx, 8)));
                    f.ret(f.str_from_trust(src, n, f.i64(1)));
                }
                Ty::Handle(want) => {
                    expect(f, HANDLE);
                    let hx = f.load_ptr(data);
                    f.fail_if(f.is_null(hx), "native function returned an empty handle");
                    let raw = f.load_ptr(f.at_c(hx, 8));
                    let name = f.var(f.cx.lit(""));
                    f.if_(f.not(f.is_null(raw)), |f| f.store(name, f.c_text(raw)));
                    let name = f.load_ptr(name);
                    if !want.is_empty() {
                        let same =
                            f.call_int(f.cx.str_eq_fn(), &[name.into(), f.cx.lit(want).into()]);
                        f.if_(f.not(same), |f| {
                            let m = f.str_cat(&[
                                f.cx.lit("native function '"),
                                what,
                                f.cx.lit("' returned a handle<"),
                                name,
                                f.cx.lit(&format!(">, but its declaration says handle<{want}>")),
                            ]);
                            f.release(&Ty::Str, name.into());
                            f.fail_str(m);
                        });
                    }
                    let h = f.malloc(24);
                    f.store(h, f.i64(1));
                    f.store(f.at_c(h, HANDLE_PTR), f.load_ptr(hx));
                    f.store(f.at_c(h, HANDLE_NAME), name);
                    f.ret(h);
                }
                Ty::Opt(inner) => {
                    f.if_(f.eq(tag, f.i64(NIL)), |f| f.ret(super::layout::zero(f.cx.llty(&t))));
                    let x = f.callv(f.cx.ffi_from_fn(inner), &[v.into(), what.into()]);
                    f.if_(f.err_pending(), |f| f.ret_zero());
                    f.ret(f.wrap(&t, x));
                }
                Ty::Array(el) => {
                    expect(f, ARRAY);
                    let ax = f.load_ptr(data);
                    let n = f.var(f.i64(0));
                    f.if_(f.not(f.is_null(ax)), |f| f.store(n, f.load_i64(f.at_c(ax, 8))));
                    let n = f.load_i64(n);
                    let arr = f.array_new(el, n);
                    f.for_range(f.i64(0), n, |f, i, _| {
                        let item = f.at(f.load_ptr(ax), f.mul(i, f.i64(VAL)));
                        let x = f.callv(f.cx.ffi_from_fn(el), &[item.into(), what.into()]);
                        f.if_(f.err_pending(), |f| {
                            f.release(&t, arr.into());
                            f.ret_zero();
                        });
                        f.array_push_owned(el, arr, x);
                    });
                    f.ret(arr);
                }
                Ty::Map(vt) => {
                    expect(f, DICT);
                    let m = f.load_ptr(data);
                    let n = f.var(f.i64(0));
                    f.if_(f.not(f.is_null(m)), |f| f.store(n, f.load_i64(f.at_c(m, 16))));
                    let n = f.load_i64(n);
                    let vsize = f.size_of(f.cx.llty(vt));
                    let d = f.call_ptr(f.cx.dict_new_fn(), &[n.into(), vsize.into()]);
                    f.for_range(f.i64(0), n, |f, i, _| {
                        let k = f.c_text(f.load_ptr(f.elem(f.cx.ptrt(), f.load_ptr(m), i)));
                        f.set_trust(k, f.i64(1));
                        let item = f.at(f.load_ptr(f.at_c(m, 8)), f.mul(i, f.i64(VAL)));
                        let x = f.callv(f.cx.ffi_from_fn(vt), &[item.into(), what.into()]);
                        f.if_(f.err_pending(), |f| {
                            f.release(&Ty::Str, k.into());
                            f.release(&t, d.into());
                            f.ret_zero();
                        });
                        f.call(f.cx.dict_fn("set", vt), &[d.into(), k.into(), x]);
                    });
                    f.ret(d);
                }
                Ty::Struct(class) => {
                    expect(f, STRUCT);
                    let sx = f.load_ptr(data);
                    f.fail_if(f.is_null(sx), "native function returned an empty struct");
                    let st = f.cx.struct_ty(class);
                    let fields = f.cx.struct_fields(class);
                    let names = f.cx.struct_field_names(class);
                    let size = f.size_of(st.into());
                    let o = f.call_ptr(f.cx.libc("malloc"), &[size.into()]);
                    f.store(o, f.i64(1));
                    f.store(f.at_c(o, TYPE_ID), f.i64(f.cx.struct_id(class) as i64));
                    let (keys, vals, n) = (
                        f.load_ptr(f.at_c(sx, 8)),
                        f.load_ptr(f.at_c(sx, 16)),
                        f.load_i64(f.at_c(sx, 24)),
                    );
                    for (idx, (ft, fname)) in fields.iter().zip(&names).enumerate() {
                        let slot = f.field(st, o, idx as u32 + 2);
                        f.store(slot, super::layout::zero(f.cx.llty(ft)));
                        // A field the package left out keeps its zero.
                        let want = f.cx.cstr(fname);
                        f.for_range(f.i64(0), n, |f, i, done| {
                            let k = f.load_ptr(f.elem(f.cx.ptrt(), keys, i));
                            let same = f.call_int(f.cx.libc("strcmp"), &[k.into(), want.into()]);
                            f.if_(f.eq(same, f.i32(0)), |f| {
                                let item = f.at(vals, f.mul(i, f.i64(VAL)));
                                let x = f.callv(f.cx.ffi_from_fn(ft), &[item.into(), what.into()]);
                                f.store(slot, x);
                                f.br(done);
                            });
                        });
                        f.if_(f.err_pending(), |f| {
                            f.release(&t, o.into());
                            f.ret_zero();
                        });
                    }
                    f.ret(o);
                }
                Ty::Json => {
                    use super::json::{
                        KIND_ARRAY, KIND_BOOL, KIND_FLOAT, KIND_INT, KIND_OBJECT, KIND_STR,
                    };
                    f.if_(f.eq(tag, f.i64(NIL)), |f| f.ret(f.null()));
                    let word = |f: &F<'_, 'ctx>, x: BasicValueEnum<'ctx>| -> IntValue<'ctx> {
                        match x {
                            BasicValueEnum::PointerValue(p) => {
                                f.b.build_ptr_to_int(p, f.cx.i64t(), "").expect("positioned")
                            }
                            BasicValueEnum::FloatValue(d) => {
                                f.b.build_bit_cast(d, f.cx.i64t(), "")
                                    .expect("positioned")
                                    .into_int_value()
                            }
                            BasicValueEnum::IntValue(i) => f.zext(i, f.cx.i64t()),
                            _ => f.i64(0),
                        }
                    };
                    for (k, kind, ty) in [
                        (BOOL, KIND_BOOL, Ty::Bool),
                        (INT, KIND_INT, Ty::Int),
                        (FLOAT, KIND_FLOAT, Ty::Float),
                        (STR, KIND_STR, Ty::Str),
                        (ARRAY, KIND_ARRAY, Ty::Array(Box::new(Ty::Json))),
                        (DICT, KIND_OBJECT, Ty::Map(Box::new(Ty::Json))),
                    ] {
                        f.if_(f.eq(tag, f.i64(k)), |f| {
                            let x = f.callv(f.cx.ffi_from_fn(&ty), &[v.into(), what.into()]);
                            f.if_(f.err_pending(), |f| f.ret(f.null()));
                            f.ret(f.json_wrap(kind, word(f, x)));
                        });
                    }
                    f.ffi_mismatch(what, tag, &t);
                }
                other => {
                    let _ = other;
                    f.ffi_mismatch(what, tag, &t);
                }
            }
        })
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function finds a native package's file, as v1 did: a
    /// project dependency under `JADE_LIBS` or the `libs/` beside the
    /// program, and any other library where it was at build time.
    ///
    /// Outputs: the path, owned, or nothing with an error raised.
    fn native_path(&self, lib: &crate::mir::NativeLib) -> PointerValue<'ctx> {
        let Some(rel) = &lib.rel else {
            let real = self.realpath(self.cx.cstr(&lib.abs));
            let out = self.var(self.cx.lit(&lib.abs));
            self.if_(self.not(self.is_null(real)), |f| {
                f.store(out, f.c_text(real));
                f.call(f.cx.libc("free"), &[real.into()]);
            });
            return self.load_ptr(out);
        };
        let env = self.call_ptr(self.cx.libc("getenv"), &[self.cx.cstr("JADE_LIBS").into()]);
        let root = self.var(self.null());
        let from_env =
            self.and(self.not(self.is_null(env)), self.ne(self.load_i8(env), self.i8(0)));
        self.if_else(
            from_env,
            |f| f.store(root, f.c_text(env)),
            |f| {
                // The `libs/` the build copied beside this program.
                let info = f.alloca(f.cx.i64t().array_type(4));
                let me = f.func.as_global_value().as_pointer_value();
                let ok = f.call_int(f.cx.libc("dladdr"), &[me.into(), info.into()]);
                let file = f.load_ptr(info);
                f.if_(f.and(f.ne(ok, f.i32(0)), f.not(f.is_null(file))), |f| {
                    let path = f.c_text(file);
                    let slash =
                        f.call_int(f.cx.str_rfind_fn(), &[path.into(), f.cx.lit("/").into()]);
                    let dir = f.copy_bytes(
                        path,
                        f.i64(0),
                        f.select_int(f.slt(slash, f.i64(0)), f.i64(0), slash),
                    );
                    let cand = f.str_cat(&[dir, f.cx.lit("/libs")]);
                    let real = f.realpath(f.str_data(cand));
                    f.if_(f.not(f.is_null(real)), |f| {
                        f.store(root, f.c_text(real));
                        f.call(f.cx.libc("free"), &[real.into()]);
                    });
                    for s in [path, dir, cand] {
                        f.release(&Ty::Str, s.into());
                    }
                });
            },
        );
        let root = self.load_ptr(root);
        self.if_(self.is_null(root), |f| {
            f.fail(&format!(
                "cannot load the dependency '{rel}': this program has no libraries directory. It \
                 was built without one, and JADE_LIBS is not set."
            ));
        });
        let cand = self.str_cat(&[root, self.cx.lit("/"), self.cx.lit(rel)]);
        let real = self.realpath(self.str_data(cand));
        self.release(&Ty::Str, cand.into());
        self.if_(self.is_null(real), |f| {
            let m = f.str_cat(&[
                f.cx.lit(&format!("cannot load the dependency '{rel}': it is not in '")),
                root,
                f.cx.lit("', which is where this program looks for it."),
            ]);
            f.release(&Ty::Str, root.into());
            f.fail_str(m);
        });
        self.release(&Ty::Str, root.into());
        let out = self.c_text(real);
        self.call(self.cx.libc("free"), &[real.into()]);
        out
    }

    /// Purpose: This function resolves a path with `realpath`, which mallocs
    /// its answer, or gives null.
    fn realpath(&self, p: PointerValue<'ctx>) -> PointerValue<'ctx> {
        self.call_ptr(self.cx.libc("realpath"), &[p.into(), self.null().into()])
    }

    /// Purpose: This function copies a C string into a new Jade string.
    pub fn c_text(&self, p: PointerValue<'ctx>) -> PointerValue<'ctx> {
        let n = self.call_int(self.cx.libc("strlen"), &[p.into()]);
        self.call_ptr(self.cx.str_from_fn(), &[p.into(), n.into()])
    }

    fn malloc(&self, n: i64) -> PointerValue<'ctx> {
        self.call_ptr(self.cx.libc("malloc"), &[self.i64(n).into()])
    }

    fn ptr_eq(&self, a: PointerValue<'ctx>, b: PointerValue<'ctx>) -> IntValue<'ctx> {
        let i = self.cx.i64t();
        let (x, y) = (
            self.b.build_ptr_to_int(a, i, "").expect("positioned"),
            self.b.build_ptr_to_int(b, i, "").expect("positioned"),
        );
        self.eq(x, y)
    }

    /// Purpose: This function raises, from support code, that a native value
    /// is not of the kind its static type says.
    fn ffi_mismatch(&self, what: PointerValue<'ctx>, tag: IntValue<'ctx>, want: &Ty) {
        let names = [
            "nil",
            "an int",
            "a float",
            "a bool",
            "a str",
            "an error",
            "an array",
            "a dict",
            "a struct",
            "bytes",
            "a handle",
            "a function",
            "a char",
        ];
        let got = self.var(self.cx.lit("a value of an unknown kind"));
        for (k, n) in names.iter().enumerate() {
            self.if_(self.eq(tag, self.i64(k as i64)), |f| f.store(got, f.cx.lit(n)));
        }
        let m = self.str_cat(&[
            self.cx.lit("native function '"),
            what,
            self.cx.lit("' returned "),
            self.load_ptr(got),
            self.cx.lit(&format!(" where {want} was expected")),
        ]);
        self.fail_str(m);
    }

    /// Purpose: This function frees what a handle object holds. The pointee
    /// is the library's, and is never touched.
    pub fn handle_destroy(&self, h: PointerValue<'ctx>) {
        let name = self.load_ptr(self.at_c(h, HANDLE_NAME));
        self.release(&Ty::Str, name.into());
        self.call(self.cx.libc("free"), &[h.into()]);
    }
}

/// Purpose: This trait reads a call's result as a value, or a zero byte for
/// a void call.
trait LeftOrZero<'ctx> {
    fn left_or_zero(self, f: &F<'_, 'ctx>) -> BasicValueEnum<'ctx>;
}

impl<'ctx> LeftOrZero<'ctx> for inkwell::values::ValueKind<'ctx> {
    fn left_or_zero(self, f: &F<'_, 'ctx>) -> BasicValueEnum<'ctx> {
        match self {
            inkwell::values::ValueKind::Basic(v) => v,
            inkwell::values::ValueKind::Instruction(_) => f.i8(0).into(),
        }
    }
}
