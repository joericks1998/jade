//! Arrays and dicts.
//!
//! An array is `{ i64 rc, i64 len, i64 cap, ptr data }`, with its elements
//! stored unboxed in `data` at their own size: an `[int]` is a buffer of
//! `i64`s and a `[str]` a buffer of pointers. Growing reallocates `data`, never
//! the object, so every reference to an array keeps seeing the same array.
//!
//! A dict is `{ i64 rc, i64 len, i64 cap, ptr keys, ptr vals, ptr index, i64
//! icap }`. Entries live in insertion order in two parallel buffers, keys and
//! values. Up to eight entries are found by scanning; past that an
//! open-addressed index of `i32` positions, hashed with FNV-1a, finds them.
//! Nothing removes an entry, so the index never needs a tombstone.
//!
//! A dict is a value: a write to one that another place also holds copies it
//! first (`jade.dict.unique`). An array is a reference and is written in place.
//!
//! Functions that do not depend on the element type (`jade.array.new`,
//! `jade.dict.find`) are generated once; the rest once per element type.

use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use super::layout::{
    ARR_DATA, ARR_SIZE, CAP, DICT_ICAP, DICT_INDEX, DICT_KEYS, DICT_SIZE, DICT_VALS, LEN,
};
use super::rc::key;
use super::{Cx, F};
use crate::mir::{Ty, is_heap};

/// Dicts at or below this many entries are scanned rather than indexed.
const DICT_SCAN_MAX: i64 = 8;

impl<'ctx> Cx<'ctx> {
    // ── arrays ───────────────────────────────────────────────────────────

    /// Purpose: This function returns `jade.array.new(cap, esize) -> array`.
    pub fn array_new_fn(&self) -> FunctionValue<'ctx> {
        let i64t = self.i64t();
        self.support("jade.array.new", Some(self.ptrt().into()), &[i64t.into(), i64t.into()], |f| {
            let (cap, esize) = (f.param_int(0), f.param_int(1));
            let cap = f.select_int(f.slt(cap, f.i64(4)), f.i64(4), cap);
            let a = f.call_ptr(f.cx.libc("malloc"), &[f.i64(ARR_SIZE).into()]);
            let data = f.call_ptr(f.cx.libc("malloc"), &[f.mul(cap, esize).into()]);
            f.store(a, f.i64(1));
            f.store(f.at_c(a, LEN), f.i64(0));
            f.store(f.at_c(a, CAP), cap);
            f.store(f.at_c(a, ARR_DATA), data);
            f.ret(a);
        })
    }

    /// Purpose: This function returns `jade.array.reserve(a, need, esize)`,
    /// which grows an array's buffer to hold at least `need` elements.
    fn array_reserve_fn(&self) -> FunctionValue<'ctx> {
        let (ptr, i64t) = (self.ptrt(), self.i64t());
        self.support("jade.array.reserve", None, &[ptr.into(), i64t.into(), i64t.into()], |f| {
            let (a, need, esize) = (f.param_ptr(0), f.param_int(1), f.param_int(2));
            let cap = f.load_i64(f.at_c(a, CAP));
            f.if_(f.sgt(need, cap), |f| {
                let doubled = f.mul(cap, f.i64(2));
                let ncap = f.select_int(f.sgt(need, doubled), need, doubled);
                let data = f.load_ptr(f.at_c(a, ARR_DATA));
                let nd =
                    f.call_ptr(f.cx.libc("realloc"), &[data.into(), f.mul(ncap, esize).into()]);
                f.store(f.at_c(a, ARR_DATA), nd);
                f.store(f.at_c(a, CAP), ncap);
            });
            f.ret_void();
        })
    }

    /// Purpose: This function returns the per-element-type array function for
    /// one operation, building it the first time.
    ///
    /// Inputs:
    /// - `op`: `get`, `set`, `push`, `pop`, `contains`, `sort`, `reverse`,
    ///   `copy` or `join`.
    /// - `t`: the element type.
    ///
    /// Outputs: `jade.array.<op>.<type>`.
    pub fn array_fn(&self, op: &'static str, t: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.array.{op}.{}", key(t));
        let (ptr, i64t, et) = (self.ptrt(), self.i64t(), self.llty(t));
        let t = t.clone();
        let (ret, params): (
            Option<inkwell::types::BasicTypeEnum>,
            Vec<inkwell::types::BasicTypeEnum>,
        ) = match op {
            "get" => (Some(et), vec![ptr.into(), i64t.into()]),
            "set" => (None, vec![ptr.into(), i64t.into(), et]),
            "push" => (None, vec![ptr.into(), et]),
            "pop" => (Some(et), vec![ptr.into()]),
            "contains" => (Some(self.i1t().into()), vec![ptr.into(), et]),
            "sort" | "reverse" => (None, vec![ptr.into()]),
            "copy" => (Some(ptr.into()), vec![ptr.into()]),
            "join" => (Some(ptr.into()), vec![ptr.into(), ptr.into()]),
            other => panic!("no array operation {other}"),
        };
        self.support(&name, ret, &params, |f| {
            let a = f.param_ptr(0);
            let len_p = f.at_c(a, LEN);
            let len = f.load_i64(len_p);
            let data = f.load_ptr(f.at_c(a, ARR_DATA));
            let bounds = |f: &F<'_, 'ctx>, i: IntValue<'ctx>| {
                let out = f.or(f.slt(i, f.i64(0)), f.sge(i, len));
                f.fail_if(out, "index out of bounds");
            };
            match op {
                "get" => {
                    let i = f.param_int(1);
                    bounds(f, i);
                    f.ret(f.load(et, f.elem(et, data, i)));
                }
                "set" => {
                    let (i, v) = (f.param_int(1), f.param(2));
                    bounds(f, i);
                    let slot = f.elem(et, data, i);
                    let old = f.load(et, slot);
                    f.store(slot, v);
                    f.release(&t, old);
                    f.ret_void();
                }
                "push" => {
                    let v = f.param(1);
                    let need = f.add(len, f.i64(1));
                    f.call(f.cx.array_reserve_fn(), &[a.into(), need.into(), f.size_of(et).into()]);
                    let data = f.load_ptr(f.at_c(a, ARR_DATA));
                    f.store(f.elem(et, data, len), v);
                    f.store(len_p, need);
                    f.ret_void();
                }
                "pop" => {
                    f.fail_if(f.eq(len, f.i64(0)), "pop from an empty array");
                    let last = f.sub(len, f.i64(1));
                    f.store(len_p, last);
                    f.ret(f.load(et, f.elem(et, data, last)));
                }
                "contains" => {
                    let v = f.param(1);
                    f.for_range(f.i64(0), len, |f, i, _| {
                        let x = f.load(et, f.elem(et, data, i));
                        f.if_(f.eq_values(&t, x, v), |f| f.ret(f.bool(true)));
                    });
                    f.ret(f.bool(false));
                }
                "sort" => {
                    let cmp = f.cx.qsort_cmp_fn(&t);
                    f.call(
                        f.cx.libc("qsort"),
                        &[
                            data.into(),
                            len.into(),
                            f.size_of(et).into(),
                            cmp.as_global_value().as_pointer_value().into(),
                        ],
                    );
                    f.ret_void();
                }
                "reverse" => {
                    let half = f.sdiv(len, f.i64(2));
                    f.for_range(f.i64(0), half, |f, i, _| {
                        let j = f.sub(f.sub(len, f.i64(1)), i);
                        let (pi, pj) = (f.elem(et, data, i), f.elem(et, data, j));
                        let (x, y) = (f.load(et, pi), f.load(et, pj));
                        f.store(pi, y);
                        f.store(pj, x);
                    });
                    f.ret_void();
                }
                "copy" => {
                    let b = f.array_new(&t, len);
                    let bd = f.load_ptr(f.at_c(b, ARR_DATA));
                    f.for_range(f.i64(0), len, |f, i, _| {
                        let x = f.load(et, f.elem(et, data, i));
                        f.retain(&t, x);
                        f.store(f.elem(et, bd, i), x);
                    });
                    f.store(f.at_c(b, LEN), len);
                    f.ret(b);
                }
                "join" => {
                    let sep = f.param_ptr(1);
                    let sb = f.sb_new();
                    f.for_range(f.i64(0), len, |f, i, _| {
                        f.if_(f.sgt(i, f.i64(0)), |f| f.sb_push_str(sb, sep));
                        let x = f.load(et, f.elem(et, data, i));
                        f.sb_push_value(sb, &t, x);
                    });
                    f.ret(f.sb_finish(sb));
                }
                _ => f.unreachable(),
            }
        })
    }

    // ── dicts ────────────────────────────────────────────────────────────

    /// Purpose: This function returns `jade.dict.new(cap, vsize) -> dict`.
    pub fn dict_new_fn(&self) -> FunctionValue<'ctx> {
        let i64t = self.i64t();
        self.support("jade.dict.new", Some(self.ptrt().into()), &[i64t.into(), i64t.into()], |f| {
            let (cap, vsize) = (f.param_int(0), f.param_int(1));
            let cap = f.select_int(f.slt(cap, f.i64(4)), f.i64(4), cap);
            let malloc = f.cx.libc("malloc");
            let d = f.call_ptr(malloc, &[f.i64(DICT_SIZE).into()]);
            f.store(d, f.i64(1));
            f.store(f.at_c(d, LEN), f.i64(0));
            f.store(f.at_c(d, CAP), cap);
            f.store(f.at_c(d, DICT_KEYS), f.call_ptr(malloc, &[f.mul(cap, f.i64(8)).into()]));
            f.store(f.at_c(d, DICT_VALS), f.call_ptr(malloc, &[f.mul(cap, vsize).into()]));
            f.store(f.at_c(d, DICT_INDEX), f.null());
            f.store(f.at_c(d, DICT_ICAP), f.i64(0));
            f.ret(d);
        })
    }

    /// Purpose: This function returns `jade.str.hash(s) -> i64`, FNV-1a.
    fn str_hash_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.str.hash", Some(self.i64t().into()), &[self.ptrt().into()], |f| {
            let s = f.param_ptr(0);
            let data = f.str_data(s);
            let h = f.var(f.i64(0xcbf29ce484222325u64 as i64));
            f.for_range(f.i64(0), f.str_len(s), |f, i, _| {
                let b = f.zext(f.load_i8(f.at(data, i)), f.cx.i64t());
                let x = f.xor(f.load_i64(h), b);
                f.store(h, f.mul(x, f.i64(0x100000001b3)));
            });
            f.ret(f.load_i64(h));
        })
    }

    /// Purpose: This function returns `jade.dict.reindex(d)`, which rebuilds a
    /// dict's index at four times its entry count.
    fn dict_reindex_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.dict.reindex", None, &[self.ptrt().into()], |f| {
            let d = f.param_ptr(0);
            let len = f.load_i64(f.at_c(d, LEN));
            let icap = f.var(f.i64(16));
            f.while_(
                |f| f.slt(f.load_i64(icap), f.mul(len, f.i64(4))),
                |f, _| {
                    f.store(icap, f.mul(f.load_i64(icap), f.i64(2)));
                },
            );
            let icap = f.load_i64(icap);
            let old = f.load_ptr(f.at_c(d, DICT_INDEX));
            f.call(f.cx.libc("free"), &[old.into()]);
            let idx = f.call_ptr(f.cx.libc("malloc"), &[f.mul(icap, f.i64(4)).into()]);
            f.for_range(f.i64(0), icap, |f, i, _| f.store(f.elem(f.cx.i32t(), idx, i), f.i32(-1)));
            let keys = f.load_ptr(f.at_c(d, DICT_KEYS));
            let mask = f.sub(icap, f.i64(1));
            f.for_range(f.i64(0), len, |f, i, _| {
                let k = f.load_ptr(f.elem(f.cx.ptrt(), keys, i));
                let h = f.var(f.and(f.call_int(f.cx.str_hash_fn(), &[k.into()]), mask));
                f.while_(
                    |f| {
                        let slot = f.load_i32(f.elem(f.cx.i32t(), idx, f.load_i64(h)));
                        f.ne(slot, f.i32(-1))
                    },
                    |f, _| f.store(h, f.and(f.add(f.load_i64(h), f.i64(1)), mask)),
                );
                f.store(f.elem(f.cx.i32t(), idx, f.load_i64(h)), f.trunc(i, f.cx.i32t()));
            });
            f.store(f.at_c(d, DICT_INDEX), idx);
            f.store(f.at_c(d, DICT_ICAP), icap);
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.dict.find(d, key) -> i64`: the
    /// entry's position, or -1.
    pub fn dict_find_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support("jade.dict.find", Some(self.i64t().into()), &[ptr.into(), ptr.into()], |f| {
            let (d, k) = (f.param_ptr(0), f.param_ptr(1));
            let keys = f.load_ptr(f.at_c(d, DICT_KEYS));
            let idx = f.load_ptr(f.at_c(d, DICT_INDEX));
            f.if_(f.is_null(idx), |f| {
                let len = f.load_i64(f.at_c(d, LEN));
                f.for_range(f.i64(0), len, |f, i, _| {
                    let ki = f.load_ptr(f.elem(f.cx.ptrt(), keys, i));
                    f.if_(f.call_int(f.cx.str_eq_fn(), &[ki.into(), k.into()]), |f| f.ret(i));
                });
                f.ret(f.i64(-1));
            });
            let mask = f.sub(f.load_i64(f.at_c(d, DICT_ICAP)), f.i64(1));
            let h = f.var(f.and(f.call_int(f.cx.str_hash_fn(), &[k.into()]), mask));
            f.while_(
                |_| f.bool(true),
                |f, _| {
                    let slot = f.load_i32(f.elem(f.cx.i32t(), idx, f.load_i64(h)));
                    f.if_(f.eq(slot, f.i32(-1)), |f| f.ret(f.i64(-1)));
                    let at = f.sext(slot, f.cx.i64t());
                    let ki = f.load_ptr(f.elem(f.cx.ptrt(), keys, at));
                    f.if_(f.call_int(f.cx.str_eq_fn(), &[ki.into(), k.into()]), |f| f.ret(at));
                    f.store(h, f.and(f.add(f.load_i64(h), f.i64(1)), mask));
                },
            );
            f.unreachable();
        })
    }

    /// Purpose: This function returns `jade.dict.addkey(d, key, vsize) -> i64`:
    /// a new entry for a key the dict does not have, taking the key's reference.
    fn dict_addkey_fn(&self) -> FunctionValue<'ctx> {
        let (ptr, i64t) = (self.ptrt(), self.i64t());
        self.support(
            "jade.dict.addkey",
            Some(i64t.into()),
            &[ptr.into(), ptr.into(), i64t.into()],
            |f| {
                let (d, k, vsize) = (f.param_ptr(0), f.param_ptr(1), f.param_int(2));
                let len = f.load_i64(f.at_c(d, LEN));
                let cap = f.load_i64(f.at_c(d, CAP));
                f.if_(f.eq(len, cap), |f| {
                    let ncap = f.mul(cap, f.i64(2));
                    let realloc = f.cx.libc("realloc");
                    let keys = f.load_ptr(f.at_c(d, DICT_KEYS));
                    let vals = f.load_ptr(f.at_c(d, DICT_VALS));
                    f.store(
                        f.at_c(d, DICT_KEYS),
                        f.call_ptr(realloc, &[keys.into(), f.mul(ncap, f.i64(8)).into()]),
                    );
                    f.store(
                        f.at_c(d, DICT_VALS),
                        f.call_ptr(realloc, &[vals.into(), f.mul(ncap, vsize).into()]),
                    );
                    f.store(f.at_c(d, CAP), ncap);
                });
                let keys = f.load_ptr(f.at_c(d, DICT_KEYS));
                f.store(f.elem(f.cx.ptrt(), keys, len), k);
                let n = f.add(len, f.i64(1));
                f.store(f.at_c(d, LEN), n);
                f.if_(f.sgt(n, f.i64(DICT_SCAN_MAX)), |f| {
                    let icap = f.load_i64(f.at_c(d, DICT_ICAP));
                    f.if_else(
                        f.sgt(f.mul(n, f.i64(2)), icap),
                        |f| {
                            f.call(f.cx.dict_reindex_fn(), &[d.into()]);
                        },
                        |f| {
                            let idx = f.load_ptr(f.at_c(d, DICT_INDEX));
                            let mask = f.sub(icap, f.i64(1));
                            let h = f.var(f.and(f.call_int(f.cx.str_hash_fn(), &[k.into()]), mask));
                            f.while_(
                                |f| {
                                    f.ne(
                                        f.load_i32(f.elem(f.cx.i32t(), idx, f.load_i64(h))),
                                        f.i32(-1),
                                    )
                                },
                                |f, _| f.store(h, f.and(f.add(f.load_i64(h), f.i64(1)), mask)),
                            );
                            f.store(
                                f.elem(f.cx.i32t(), idx, f.load_i64(h)),
                                f.trunc(len, f.cx.i32t()),
                            );
                        },
                    );
                });
                f.ret(len);
            },
        )
    }

    /// Purpose: This function returns the per-value-type dict function for one
    /// operation, building it the first time.
    ///
    /// Inputs:
    /// - `op`: `set`, `unique`, `get`, `getopt`, `keys`, `values`, `merge` or `clone`.
    /// - `t`: the value type.
    ///
    /// Outputs: `jade.dict.<op>.<type>`.
    pub fn dict_fn(&self, op: &'static str, t: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.dict.{op}.{}", key(t));
        let (ptr, et) = (self.ptrt(), self.llty(t));
        let opt = Ty::opt(t.clone());
        let t = t.clone();
        let (ret, params): (
            Option<inkwell::types::BasicTypeEnum>,
            Vec<inkwell::types::BasicTypeEnum>,
        ) = match op {
            "set" => (None, vec![ptr.into(), ptr.into(), et]),
            "unique" | "keys" | "values" | "clone" => (Some(ptr.into()), vec![ptr.into()]),
            "get" => (Some(et), vec![ptr.into(), ptr.into()]),
            "getopt" => (Some(self.llty(&opt)), vec![ptr.into(), ptr.into()]),
            "merge" => (Some(ptr.into()), vec![ptr.into(), ptr.into()]),
            other => panic!("no dict operation {other}"),
        };
        self.support(&name, ret, &params, |f| {
            let d = f.param_ptr(0);
            let vsize = f.size_of(et);
            match op {
                "set" => {
                    let (k, v) = (f.param_ptr(1), f.param(2));
                    let at = f.call_int(f.cx.dict_find_fn(), &[d.into(), k.into()]);
                    f.if_else(
                        f.sge(at, f.i64(0)),
                        |f| {
                            f.release(&Ty::Str, k.into());
                            let vals = f.load_ptr(f.at_c(d, DICT_VALS));
                            let slot = f.elem(et, vals, at);
                            let old = f.load(et, slot);
                            f.store(slot, v);
                            f.release(&t, old);
                        },
                        |f| {
                            let at = f.call_int(
                                f.cx.dict_addkey_fn(),
                                &[d.into(), k.into(), vsize.into()],
                            );
                            let vals = f.load_ptr(f.at_c(d, DICT_VALS));
                            f.store(f.elem(et, vals, at), v);
                        },
                    );
                    f.ret_void();
                }
                "clone" => {
                    let len = f.load_i64(f.at_c(d, LEN));
                    let c = f.call_ptr(f.cx.dict_new_fn(), &[len.into(), vsize.into()]);
                    let (keys, vals) =
                        (f.load_ptr(f.at_c(d, DICT_KEYS)), f.load_ptr(f.at_c(d, DICT_VALS)));
                    let (ck, cv) =
                        (f.load_ptr(f.at_c(c, DICT_KEYS)), f.load_ptr(f.at_c(c, DICT_VALS)));
                    f.for_range(f.i64(0), len, |f, i, _| {
                        let k = f.load_ptr(f.elem(f.cx.ptrt(), keys, i));
                        f.retain(&Ty::Str, k.into());
                        f.store(f.elem(f.cx.ptrt(), ck, i), k);
                        let v = f.load(et, f.elem(et, vals, i));
                        f.retain(&t, v);
                        f.store(f.elem(et, cv, i), v);
                    });
                    f.store(f.at_c(c, LEN), len);
                    f.if_(f.sgt(len, f.i64(DICT_SCAN_MAX)), |f| {
                        f.call(f.cx.dict_reindex_fn(), &[c.into()]);
                    });
                    f.ret(c);
                }
                "unique" => {
                    let rc = f.load_i64_atomic(d);
                    f.if_(f.eq(rc, f.i64(1)), |f| f.ret(d));
                    let c = f.call_ptr(f.cx.dict_fn("clone", &t), &[d.into()]);
                    f.release(&Ty::Map(Box::new(t.clone())), d.into());
                    f.ret(c);
                }
                "get" => {
                    let k = f.param_ptr(1);
                    let at = f.call_int(f.cx.dict_find_fn(), &[d.into(), k.into()]);
                    f.fail_if(f.slt(at, f.i64(0)), "key not found");
                    let vals = f.load_ptr(f.at_c(d, DICT_VALS));
                    f.ret(f.load(et, f.elem(et, vals, at)));
                }
                "getopt" => {
                    let k = f.param_ptr(1);
                    let at = f.call_int(f.cx.dict_find_fn(), &[d.into(), k.into()]);
                    f.if_(f.slt(at, f.i64(0)), |f| f.ret(super::layout::zero(f.cx.llty(&opt))));
                    let vals = f.load_ptr(f.at_c(d, DICT_VALS));
                    let v = f.load(et, f.elem(et, vals, at));
                    f.retain(&t, v);
                    f.ret(f.wrap(&opt, v));
                }
                "keys" => {
                    let st = Ty::Str;
                    let len = f.load_i64(f.at_c(d, LEN));
                    let arr = f.array_new(&st, len);
                    let ad = f.load_ptr(f.at_c(arr, ARR_DATA));
                    let keys = f.load_ptr(f.at_c(d, DICT_KEYS));
                    f.for_range(f.i64(0), len, |f, i, _| {
                        let k = f.load_ptr(f.elem(f.cx.ptrt(), keys, i));
                        f.retain(&st, k.into());
                        f.store(f.elem(f.cx.ptrt(), ad, i), k);
                    });
                    f.store(f.at_c(arr, LEN), len);
                    f.call(f.cx.array_fn("sort", &st), &[arr.into()]);
                    f.ret(arr);
                }
                "values" => {
                    let keys = f.call_ptr(f.cx.dict_fn("keys", &t), &[d.into()]);
                    let len = f.load_i64(f.at_c(keys, LEN));
                    let arr = f.array_new(&t, len);
                    let kd = f.load_ptr(f.at_c(keys, ARR_DATA));
                    let ad = f.load_ptr(f.at_c(arr, ARR_DATA));
                    f.for_range(f.i64(0), len, |f, i, _| {
                        let k = f.load_ptr(f.elem(f.cx.ptrt(), kd, i));
                        let v = f.dict_get(&t, d, k);
                        f.retain(&t, v);
                        f.store(f.elem(et, ad, i), v);
                    });
                    f.store(f.at_c(arr, LEN), len);
                    f.release(&Ty::Array(Box::new(Ty::Str)), keys.into());
                    f.ret(arr);
                }
                "merge" => {
                    let b = f.param_ptr(1);
                    let c = f.call_ptr(f.cx.dict_fn("clone", &t), &[d.into()]);
                    let len = f.load_i64(f.at_c(b, LEN));
                    let (keys, vals) =
                        (f.load_ptr(f.at_c(b, DICT_KEYS)), f.load_ptr(f.at_c(b, DICT_VALS)));
                    f.for_range(f.i64(0), len, |f, i, _| {
                        let k = f.load_ptr(f.elem(f.cx.ptrt(), keys, i));
                        let v = f.load(et, f.elem(et, vals, i));
                        f.retain(&Ty::Str, k.into());
                        f.retain(&t, v);
                        f.call(f.cx.dict_fn("set", &t), &[c.into(), k.into(), v]);
                    });
                    f.ret(c);
                }
                _ => f.unreachable(),
            }
        })
    }

    /// Purpose: This function returns the sorted-keys function for a dict type.
    pub fn dict_keys_fn(&self, t: &Ty) -> FunctionValue<'ctx> {
        self.dict_fn("keys", t)
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function makes an empty array with room for `cap` elements.
    pub fn array_new(&self, t: &Ty, cap: IntValue<'ctx>) -> PointerValue<'ctx> {
        let es = self.size_of(self.cx.llty(t));
        self.call_ptr(self.cx.array_new_fn(), &[cap.into(), es.into()])
    }

    /// Purpose: This function appends a value to an array, taking its reference.
    pub fn array_push_owned(&self, t: &Ty, a: PointerValue<'ctx>, v: BasicValueEnum<'ctx>) {
        self.call(self.cx.array_fn("push", t), &[a.into(), v]);
    }

    /// Purpose: This function makes a dict from owned keys and values.
    pub fn dict_literal(
        &self,
        t: &Ty,
        entries: &[(BasicValueEnum<'ctx>, BasicValueEnum<'ctx>)],
    ) -> PointerValue<'ctx> {
        let vsize = self.size_of(self.cx.llty(t));
        let d = self.call_ptr(
            self.cx.dict_new_fn(),
            &[self.i64(entries.len() as i64).into(), vsize.into()],
        );
        for (k, v) in entries {
            self.call(self.cx.dict_fn("set", t), &[d.into(), *k, *v]);
        }
        d
    }

    /// Purpose: This function reads a dict's value for a key, borrowed.
    pub fn dict_get(
        &self,
        t: &Ty,
        d: PointerValue<'ctx>,
        k: PointerValue<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        self.callv(self.cx.dict_fn("get", t), &[d.into(), k.into()])
    }

    /// Purpose: This function says whether a dict holds a key.
    pub fn dict_has(&self, d: PointerValue<'ctx>, k: PointerValue<'ctx>) -> IntValue<'ctx> {
        let at = self.call_int(self.cx.dict_find_fn(), &[d.into(), k.into()]);
        self.sge(at, self.i64(0))
    }

    /// Purpose: This function says whether values of a type need releasing,
    /// for callers deciding whether to walk a buffer.
    pub fn needs_release(&self, t: &Ty) -> bool {
        is_heap(t)
    }
}
