//! Strings: allocation, comparison, characters, and every string method.
//!
//! A string is `{ i64 rc, i64 len, i64 trust, u8 data[len], u8 0 }`, UTF-8,
//! with a NUL after the bytes so C functions such as `strtod` can read it in
//! place. Strings never change after they are made; every method returns a new
//! one.
//!
//! *Trust* is 1 for a string derived from outside the program: a file, a
//! command's output, stdin, the environment. Every function here that builds a
//! string from others gives it the most tainted of their trusts, and a char
//! taken from a tainted string keeps the taint in bit 31 (`CHAR_TAINT`), so
//! `"" + s[0] + s[1]` cannot launder it. The string builder collects the trust
//! of what is pushed into it. Code-execution sinks such as `sh.exec` refuse a
//! tainted string.
//! Character positions count Unicode scalars, not bytes, as v1 did:
//! `len("café")` is 4 and `"café"[3]` is `é`.
//!
//! The string builder (`sb_*`) is a growable byte buffer on the caller's stack.
//! Anything that produces a string of unknown length — printing an array,
//! joining, replacing — appends to one and turns it into a string at the end.

use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use super::layout::{ARR_DATA, CHAR_TAINT, LEN, STR_DATA, STR_TRUST};
use super::{Cx, F};

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns `jade.str.alloc(len) -> str`: a new string
    /// of `len` bytes, count 1, NUL-terminated, contents unset.
    pub fn str_alloc_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support("jade.str.alloc", Some(ptr.into()), &[self.i64t().into()], |f| {
            let len = f.param_int(0);
            let p = f.call_ptr(f.cx.libc("malloc"), &[f.add(len, f.i64(STR_DATA + 1)).into()]);
            f.store(p, f.i64(1));
            f.store(f.at_c(p, LEN), len);
            f.store(f.at_c(p, STR_TRUST), f.i64(0));
            f.store(f.at(f.at_c(p, STR_DATA), len), f.i8(0));
            f.ret(p);
        })
    }

    /// Purpose: This function returns `jade.str.from(bytes, len) -> str`.
    pub fn str_from_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support("jade.str.from", Some(ptr.into()), &[ptr.into(), self.i64t().into()], |f| {
            let (src, len) = (f.param_ptr(0), f.param_int(1));
            let s = f.str_new(len);
            f.memcpy(f.str_data(s), src, len);
            f.ret(s);
        })
    }

    /// Purpose: This function returns `jade.str.eq(a, b) -> bool`.
    pub fn str_eq_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support("jade.str.eq", Some(self.i1t().into()), &[ptr.into(), ptr.into()], |f| {
            let (a, b) = (f.param_ptr(0), f.param_ptr(1));
            let (la, lb) = (f.str_len(a), f.str_len(b));
            f.if_(f.ne(la, lb), |f| f.ret(f.bool(false)));
            let c = f.call_int(
                f.cx.libc("memcmp"),
                &[f.str_data(a).into(), f.str_data(b).into(), la.into()],
            );
            f.ret(f.eq(c, f.i32(0)));
        })
    }

    /// Purpose: This function returns `jade.str.cmp(a, b) -> i32`, comparing
    /// bytes, so `-1`, `0` or `1`.
    pub fn str_cmp_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support("jade.str.cmp", Some(self.i32t().into()), &[ptr.into(), ptr.into()], |f| {
            let (a, b) = (f.param_ptr(0), f.param_ptr(1));
            let (la, lb) = (f.str_len(a), f.str_len(b));
            let n = f.select_int(f.slt(la, lb), la, lb);
            let c = f.call_int(
                f.cx.libc("memcmp"),
                &[f.str_data(a).into(), f.str_data(b).into(), n.into()],
            );
            f.if_(f.slt(c, f.i32(0)), |f| f.ret(f.i32(-1)));
            f.if_(f.sgt(c, f.i32(0)), |f| f.ret(f.i32(1)));
            f.if_(f.slt(la, lb), |f| f.ret(f.i32(-1)));
            f.if_(f.sgt(la, lb), |f| f.ret(f.i32(1)));
            f.ret(f.i32(0));
        })
    }

    /// Purpose: This function returns `jade.str.concat(a, b) -> str`.
    pub fn str_concat_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support("jade.str.concat", Some(ptr.into()), &[ptr.into(), ptr.into()], |f| {
            let (a, b) = (f.param_ptr(0), f.param_ptr(1));
            let s = f.str_cat(&[a, b]);
            f.ret(s);
        })
    }

    /// Purpose: This function returns `jade.utf8.decode(p, step_out) -> i32`:
    /// the scalar starting at `p`, with its byte length written to `step_out`.
    pub fn utf8_decode_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support("jade.utf8.decode", Some(self.i32t().into()), &[ptr.into(), ptr.into()], |f| {
            let (p, out) = (f.param_ptr(0), f.param_ptr(1));
            let i32t = f.cx.i32t();
            let b0 = f.zext(f.load_i8(p), i32t);
            let byte = |k: i64| {
                let b = f.zext(f.load_i8(f.at_c(p, k)), i32t);
                f.and(b, f.i32(0x3f))
            };
            f.if_(f.icmp(inkwell::IntPredicate::ULT, b0, f.i32(0x80)), |f| {
                f.store(out, f.i64(1));
                f.ret(b0);
            });
            f.if_(f.icmp(inkwell::IntPredicate::ULT, b0, f.i32(0xe0)), |f| {
                f.store(out, f.i64(2));
                let v = f.or(f.shl(f.and(b0, f.i32(0x1f)), f.i32(6)), byte(1));
                f.ret(v);
            });
            f.if_(f.icmp(inkwell::IntPredicate::ULT, b0, f.i32(0xf0)), |f| {
                f.store(out, f.i64(3));
                let v = f.shl(f.and(b0, f.i32(0x0f)), f.i32(12));
                let v = f.or(v, f.shl(byte(1), f.i32(6)));
                f.ret(f.or(v, byte(2)));
            });
            f.store(out, f.i64(4));
            let v = f.shl(f.and(b0, f.i32(0x07)), f.i32(18));
            let v = f.or(v, f.shl(byte(1), f.i32(12)));
            let v = f.or(v, f.shl(byte(2), f.i32(6)));
            f.ret(f.or(v, byte(3)));
        })
    }

    /// Purpose: This function returns `jade.utf8.encode(cp, out) -> i64`: the
    /// scalar's UTF-8 bytes written to `out`, and how many there are.
    pub fn utf8_encode_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support(
            "jade.utf8.encode",
            Some(self.i64t().into()),
            &[self.i32t().into(), ptr.into()],
            |f| {
                let (c, out) = (f.param_int(0), f.param_ptr(1));
                let c = f.char_cp(c);
                let i8t = f.cx.i8t();
                let put = |k: i64, v: IntValue<'ctx>| f.store(f.at_c(out, k), f.trunc(v, i8t));
                let cont =
                    |shift: i64| f.or(f.and(f.lshr(c, f.i32(shift)), f.i32(0x3f)), f.i32(0x80));
                f.if_(f.icmp(inkwell::IntPredicate::ULT, c, f.i32(0x80)), |f| {
                    put(0, c);
                    f.ret(f.i64(1));
                });
                f.if_(f.icmp(inkwell::IntPredicate::ULT, c, f.i32(0x800)), |f| {
                    put(0, f.or(f.lshr(c, f.i32(6)), f.i32(0xc0)));
                    put(1, cont(0));
                    f.ret(f.i64(2));
                });
                f.if_(f.icmp(inkwell::IntPredicate::ULT, c, f.i32(0x10000)), |f| {
                    put(0, f.or(f.lshr(c, f.i32(12)), f.i32(0xe0)));
                    put(1, cont(6));
                    put(2, cont(0));
                    f.ret(f.i64(3));
                });
                put(0, f.or(f.lshr(c, f.i32(18)), f.i32(0xf0)));
                put(1, cont(12));
                put(2, cont(6));
                put(3, cont(0));
                f.ret(f.i64(4));
            },
        )
    }

    /// Purpose: This function returns `jade.char.tostr(c) -> str`.
    pub fn char_tostr_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.char.tostr", Some(self.ptrt().into()), &[self.i32t().into()], |f| {
            let buf = f.alloca(f.cx.i8t().array_type(4));
            let c = f.param_int(0);
            let n = f.call_int(f.cx.utf8_encode_fn(), &[c.into(), buf.into()]);
            let s = f.str_from_trust(buf, n, f.char_trust(c));
            f.ret(s);
        })
    }

    /// Purpose: This function returns `jade.str.charlen(s) -> i64`: how many
    /// scalars a string holds.
    pub fn str_charlen_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.str.charlen", Some(self.i64t().into()), &[self.ptrt().into()], |f| {
            let s = f.param_ptr(0);
            let data = f.str_data(s);
            let n = f.var(f.i64(0));
            f.for_range(f.i64(0), f.str_len(s), |f, i, _| {
                let b = f.load_i8(f.at(data, i));
                // Every byte but a continuation byte (10xxxxxx) starts a scalar.
                let cont = f.eq(f.and(b, f.i8(0xc0u8 as i64)), f.i8(0x80u8 as i64));
                f.if_(f.not(cont), |f| f.store(n, f.add(f.load_i64(n), f.i64(1))));
            });
            f.ret(f.load_i64(n));
        })
    }

    /// Purpose: This function returns `jade.str.byteat(s, i) -> i64`: the byte
    /// offset of scalar `i`, with `i <= 0` giving 0 and `i` past the end giving
    /// the length, as v1's slicing did.
    pub fn str_byteat_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support(
            "jade.str.byteat",
            Some(self.i64t().into()),
            &[ptr.into(), self.i64t().into()],
            |f| {
                let (s, want) = (f.param_ptr(0), f.param_int(1));
                f.if_(f.sle(want, f.i64(0)), |f| f.ret(f.i64(0)));
                let len = f.str_len(s);
                let data = f.str_data(s);
                let seen = f.var(f.i64(0));
                f.for_range(f.i64(0), len, |f, i, _| {
                    let b = f.load_i8(f.at(data, i));
                    let cont = f.eq(f.and(b, f.i8(0xc0u8 as i64)), f.i8(0x80u8 as i64));
                    f.if_(f.not(cont), |f| {
                        f.if_(f.eq(f.load_i64(seen), want), |f| f.ret(i));
                        f.store(seen, f.add(f.load_i64(seen), f.i64(1)));
                    });
                });
                f.ret(len);
            },
        )
    }

    /// Purpose: This function returns `jade.str.index(s, i) -> char`.
    pub fn str_index_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support(
            "jade.str.index",
            Some(self.i32t().into()),
            &[ptr.into(), self.i64t().into()],
            |f| {
                let (s, want) = (f.param_ptr(0), f.param_int(1));
                f.fail_if(f.slt(want, f.i64(0)), "string index out of bounds");
                let len = f.str_len(s);
                let data = f.str_data(s);
                let off = f.var(f.i64(0));
                let seen = f.var(f.i64(0));
                let step = f.alloca(f.cx.i64t());
                f.while_(
                    |f| f.slt(f.load_i64(off), len),
                    |f, _| {
                        let o = f.load_i64(off);
                        let cp =
                            f.call_int(f.cx.utf8_decode_fn(), &[f.at(data, o).into(), step.into()]);
                        f.if_(f.eq(f.load_i64(seen), want), |f| {
                            f.ret(f.char_with_trust(cp, f.str_trust(s)))
                        });
                        f.store(seen, f.add(f.load_i64(seen), f.i64(1)));
                        f.store(off, f.add(o, f.load_i64(step)));
                    },
                );
                f.fail("string index out of bounds");
            },
        )
    }

    /// Purpose: This function returns `jade.str.chars(s) -> [char]`.
    pub fn str_chars_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.str.chars", Some(self.ptrt().into()), &[self.ptrt().into()], |f| {
            let s = f.param_ptr(0);
            let n = f.call_int(f.cx.str_charlen_fn(), &[s.into()]);
            let arr = f.array_new(&crate::mir::Ty::Char, n);
            let buf = f.load_ptr(f.at_c(arr, ARR_DATA));
            let data = f.str_data(s);
            let trust = f.str_trust(s);
            let off = f.var(f.i64(0));
            let step = f.alloca(f.cx.i64t());
            f.for_range(f.i64(0), n, |f, i, _| {
                let o = f.load_i64(off);
                let cp = f.call_int(f.cx.utf8_decode_fn(), &[f.at(data, o).into(), step.into()]);
                f.store(f.elem(f.cx.i32t(), buf, i), f.char_with_trust(cp, trust));
                f.store(off, f.add(o, f.load_i64(step)));
            });
            f.store(f.at_c(arr, LEN), n);
            f.ret(arr);
        })
    }

    /// Purpose: This function returns `jade.str.find(s, sub, from) -> i64`: the
    /// byte offset of the first occurrence of `sub` at or after `from`, or -1.
    pub fn str_find_fn(&self) -> FunctionValue<'ctx> {
        let (ptr, i64t) = (self.ptrt(), self.i64t());
        self.support(
            "jade.str.find",
            Some(i64t.into()),
            &[ptr.into(), ptr.into(), i64t.into()],
            |f| {
                let (s, sub, from) = (f.param_ptr(0), f.param_ptr(1), f.param_int(2));
                let (ls, lsub) = (f.str_len(s), f.str_len(sub));
                let last = f.sub(ls, lsub);
                f.for_range(from, f.add(last, f.i64(1)), |f, i, _| {
                    let c = f.call_int(
                        f.cx.libc("memcmp"),
                        &[f.at(f.str_data(s), i).into(), f.str_data(sub).into(), lsub.into()],
                    );
                    f.if_(f.eq(c, f.i32(0)), |f| f.ret(i));
                });
                f.ret(f.i64(-1));
            },
        )
    }

    /// Purpose: This function returns `jade.str.rfind(s, sub) -> i64`: the byte
    /// offset of the last occurrence, or -1.
    pub fn str_rfind_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support("jade.str.rfind", Some(self.i64t().into()), &[ptr.into(), ptr.into()], |f| {
            let (s, sub) = (f.param_ptr(0), f.param_ptr(1));
            let (ls, lsub) = (f.str_len(s), f.str_len(sub));
            let i = f.var(f.sub(ls, lsub));
            f.while_(
                |f| f.sge(f.load_i64(i), f.i64(0)),
                |f, _| {
                    let at = f.load_i64(i);
                    let c = f.call_int(
                        f.cx.libc("memcmp"),
                        &[f.at(f.str_data(s), at).into(), f.str_data(sub).into(), lsub.into()],
                    );
                    f.if_(f.eq(c, f.i32(0)), |f| f.ret(at));
                    f.store(i, f.sub(at, f.i64(1)));
                },
            );
            f.ret(f.i64(-1));
        })
    }

    /// Purpose: This function returns `jade.str.charcount(s, nbytes) -> i64`:
    /// how many scalars start in the first `nbytes` bytes.
    fn str_prefix_chars_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support(
            "jade.str.prefixchars",
            Some(self.i64t().into()),
            &[ptr.into(), self.i64t().into()],
            |f| {
                let (s, nb) = (f.param_ptr(0), f.param_int(1));
                let data = f.str_data(s);
                let n = f.var(f.i64(0));
                f.for_range(f.i64(0), nb, |f, i, _| {
                    let b = f.load_i8(f.at(data, i));
                    let cont = f.eq(f.and(b, f.i8(0xc0u8 as i64)), f.i8(0x80u8 as i64));
                    f.if_(f.not(cont), |f| f.store(n, f.add(f.load_i64(n), f.i64(1))));
                });
                f.ret(f.load_i64(n));
            },
        )
    }

    /// Purpose: This function returns `jade.isspace.unicode(cp) -> bool`, the
    /// Unicode `White_Space` property Rust's `trim_start` and `trim_end` use.
    fn unicode_space_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.isspace.unicode", Some(self.i1t().into()), &[self.i32t().into()], |f| {
            let c = f.param_int(0);
            let r = |lo: i64, hi: i64| {
                f.and(
                    f.icmp(inkwell::IntPredicate::UGE, c, f.i32(lo)),
                    f.icmp(inkwell::IntPredicate::ULE, c, f.i32(hi)),
                )
            };
            let mut any = r(0x09, 0x0d);
            for (lo, hi) in [
                (0x20, 0x20),
                (0x85, 0x85),
                (0xa0, 0xa0),
                (0x1680, 0x1680),
                (0x2000, 0x200a),
                (0x2028, 0x2029),
                (0x202f, 0x202f),
                (0x205f, 0x205f),
                (0x3000, 0x3000),
            ] {
                any = f.or(any, r(lo, hi));
            }
            f.ret(any);
        })
    }

    /// Purpose: This function returns the function behind a string method that
    /// takes the string and returns a new one: case mapping and trimming.
    ///
    /// Inputs:
    /// - `which`: the method.
    ///
    /// Outputs: `jade.str.<method>(s) -> str`.
    pub fn str_unary_fn(&self, which: crate::mir::StrMethod) -> FunctionValue<'ctx> {
        use crate::mir::StrMethod as M;
        let name = format!("jade.str.{which:?}").to_lowercase();
        self.support(&name, Some(self.ptrt().into()), &[self.ptrt().into()], |f| {
            let s = f.param_ptr(0);
            match which {
                M::Upper | M::Lower | M::Capitalize => {
                    let sb = f.sb_new();
                    f.sb_taint(sb, f.str_trust(s));
                    let data = f.str_data(s);
                    let len = f.str_len(s);
                    let off = f.var(f.i64(0));
                    let step = f.alloca(f.cx.i64t());
                    let out = f.alloca(f.cx.i32t().array_type(3));
                    f.while_(
                        |f| f.slt(f.load_i64(off), len),
                        |f, _| {
                            let o = f.load_i64(off);
                            let cp = f.call_int(
                                f.cx.utf8_decode_fn(),
                                &[f.at(data, o).into(), step.into()],
                            );
                            let upper = match which {
                                M::Upper => f.bool(true),
                                M::Lower => f.bool(false),
                                _ => f.eq(o, f.i64(0)),
                            };
                            let n = f.var(f.i64(0));
                            f.if_else(
                                upper,
                                |f| {
                                    f.store(
                                        n,
                                        f.call_int(f.cx.case_fn(true), &[cp.into(), out.into()]),
                                    )
                                },
                                |f| {
                                    f.store(
                                        n,
                                        f.call_int(f.cx.case_fn(false), &[cp.into(), out.into()]),
                                    )
                                },
                            );
                            f.for_range(f.i64(0), f.load_i64(n), |f, k, _| {
                                let m = f.load_i32(f.elem(f.cx.i32t(), out, k));
                                f.sb_push_cp(sb, m);
                            });
                            f.store(off, f.add(o, f.load_i64(step)));
                        },
                    );
                    f.ret(f.sb_finish(sb));
                }
                M::Trim => {
                    // ASCII whitespace, as v1's `trim` used `isspace`.
                    let data = f.str_data(s);
                    let len = f.str_len(s);
                    let is_space = |f: &F<'_, 'ctx>, b: IntValue<'ctx>| {
                        let sp = f.eq(b, f.i8(32));
                        let ctl = f.and(
                            f.icmp(inkwell::IntPredicate::UGE, b, f.i8(9)),
                            f.icmp(inkwell::IntPredicate::ULE, b, f.i8(13)),
                        );
                        f.or(sp, ctl)
                    };
                    let a = f.var(f.i64(0));
                    f.while_(
                        |f| {
                            let i = f.load_i64(a);
                            let inside = f.slt(i, len);
                            let r = f.var(f.bool(false));
                            f.if_(inside, |f| {
                                let b = f.load_i8(f.at(data, i));
                                f.store(r, is_space(f, b));
                            });
                            f.load(f.cx.i1t(), r).into_int_value()
                        },
                        |f, _| f.store(a, f.add(f.load_i64(a), f.i64(1))),
                    );
                    let e = f.var(len);
                    f.while_(
                        |f| {
                            let i = f.load_i64(e);
                            let inside = f.sgt(i, f.load_i64(a));
                            let r = f.var(f.bool(false));
                            f.if_(inside, |f| {
                                let b = f.load_i8(f.at(data, f.sub(i, f.i64(1))));
                                f.store(r, is_space(f, b));
                            });
                            f.load(f.cx.i1t(), r).into_int_value()
                        },
                        |f, _| f.store(e, f.sub(f.load_i64(e), f.i64(1))),
                    );
                    let (a, e) = (f.load_i64(a), f.load_i64(e));
                    f.ret(f.str_from_trust(f.at(data, a), f.sub(e, a), f.str_trust(s)));
                }
                M::TrimStart | M::TrimEnd => {
                    let data = f.str_data(s);
                    let len = f.str_len(s);
                    let start = f.var(len);
                    let end = f.var(f.i64(0));
                    let off = f.var(f.i64(0));
                    let step = f.alloca(f.cx.i64t());
                    let found = f.var(f.bool(false));
                    f.while_(
                        |f| f.slt(f.load_i64(off), len),
                        |f, _| {
                            let o = f.load_i64(off);
                            let cp = f.call_int(
                                f.cx.utf8_decode_fn(),
                                &[f.at(data, o).into(), step.into()],
                            );
                            let next = f.add(o, f.load_i64(step));
                            let sp = f.call_int(f.cx.unicode_space_fn(), &[cp.into()]);
                            f.if_(f.not(sp), |f| {
                                f.if_(f.not(f.load(f.cx.i1t(), found).into_int_value()), |f| {
                                    f.store(start, o);
                                    f.store(found, f.bool(true));
                                });
                                f.store(end, next);
                            });
                            f.store(off, next);
                        },
                    );
                    let (a, e) = if which == M::TrimStart {
                        (f.load_i64(start), len)
                    } else {
                        (f.i64(0), f.load_i64(end))
                    };
                    let e = f.select_int(f.slt(e, a), a, e);
                    f.ret(f.str_from_trust(f.at(data, a), f.sub(e, a), f.str_trust(s)));
                }
                _ => f.unreachable(),
            }
        })
    }

    /// Purpose: This function returns `jade.str.split(s, sep) -> [str]`, with
    /// Rust's rules: an empty separator splits between every character and
    /// at both ends.
    pub fn str_split_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support("jade.str.split", Some(ptr.into()), &[ptr.into(), ptr.into()], |f| {
            let (s, sep) = (f.param_ptr(0), f.param_ptr(1));
            let st = crate::mir::Ty::Str;
            let arr = f.array_new(&st, f.i64(4));
            let data = f.str_data(s);
            let len = f.str_len(s);
            let lsep = f.str_len(sep);
            f.if_(f.eq(lsep, f.i64(0)), |f| {
                f.array_push_owned(&st, arr, f.cx.lit("").into());
                let chars = f.call_ptr(f.cx.str_chars_fn(), &[s.into()]);
                let n = f.load_i64(f.at_c(chars, LEN));
                let buf = f.load_ptr(f.at_c(chars, ARR_DATA));
                f.for_range(f.i64(0), n, |f, i, _| {
                    let cp = f.load_i32(f.elem(f.cx.i32t(), buf, i));
                    let one = f.call_ptr(f.cx.char_tostr_fn(), &[cp.into()]);
                    f.array_push_owned(&st, arr, one.into());
                });
                f.release(&crate::mir::Ty::Array(Box::new(crate::mir::Ty::Char)), chars.into());
                f.array_push_owned(&st, arr, f.cx.lit("").into());
                f.ret(arr);
            });
            let start = f.var(f.i64(0));
            f.while_(
                |_| f.bool(true),
                |f, exit| {
                    let from = f.load_i64(start);
                    let at = f.call_int(f.cx.str_find_fn(), &[s.into(), sep.into(), from.into()]);
                    f.if_(f.slt(at, f.i64(0)), |f| f.br(exit));
                    let part = f.str_from_trust(f.at(data, from), f.sub(at, from), f.str_trust(s));
                    f.array_push_owned(&st, arr, part.into());
                    f.store(start, f.add(at, lsep));
                },
            );
            let from = f.load_i64(start);
            let part = f.str_from_trust(f.at(data, from), f.sub(len, from), f.str_trust(s));
            f.array_push_owned(&st, arr, part.into());
            f.ret(arr);
        })
    }

    /// Purpose: This function returns `jade.str.lines(s) -> [str]`, with Rust's
    /// rules: `\n` or `\r\n` ends a line, and a final line ending adds no
    /// empty line.
    pub fn str_lines_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support("jade.str.lines", Some(ptr.into()), &[ptr.into()], |f| {
            let s = f.param_ptr(0);
            let st = crate::mir::Ty::Str;
            let arr = f.array_new(&st, f.i64(4));
            let data = f.str_data(s);
            let len = f.str_len(s);
            let start = f.var(f.i64(0));
            f.for_range(f.i64(0), len, |f, i, _| {
                let b = f.load_i8(f.at(data, i));
                f.if_(f.eq(b, f.i8(10)), |f| {
                    let from = f.load_i64(start);
                    let end = f.var(i);
                    f.if_(f.sgt(i, from), |f| {
                        let prev = f.load_i8(f.at(data, f.sub(i, f.i64(1))));
                        f.if_(f.eq(prev, f.i8(13)), |f| f.store(end, f.sub(i, f.i64(1))));
                    });
                    let e = f.load_i64(end);
                    let part = f.str_from_trust(f.at(data, from), f.sub(e, from), f.str_trust(s));
                    f.array_push_owned(&st, arr, part.into());
                    f.store(start, f.add(i, f.i64(1)));
                });
            });
            let from = f.load_i64(start);
            f.if_(f.slt(from, len), |f| {
                let part = f.str_from_trust(f.at(data, from), f.sub(len, from), f.str_trust(s));
                f.array_push_owned(&st, arr, part.into());
            });
            f.ret(arr);
        })
    }

    /// Purpose: This function returns the function behind a string method that
    /// takes the string and another string and answers a bool or an int.
    ///
    /// Inputs:
    /// - `which`: contains, starts_with, ends_with, index_of, last_index_of or count.
    ///
    /// Outputs: `jade.str.<method>(s, sub)`.
    pub fn str_search_fn(&self, which: crate::mir::StrMethod) -> FunctionValue<'ctx> {
        use crate::mir::StrMethod as M;
        let ptr = self.ptrt();
        let ret = match which {
            M::Contains | M::StartsWith | M::EndsWith => self.i1t().into(),
            _ => self.i64t().into(),
        };
        let name = format!("jade.str.{which:?}").to_lowercase();
        self.support(&name, Some(ret), &[ptr.into(), ptr.into()], |f| {
            let (s, sub) = (f.param_ptr(0), f.param_ptr(1));
            let (ls, lsub) = (f.str_len(s), f.str_len(sub));
            match which {
                M::Contains => {
                    let at =
                        f.call_int(f.cx.str_find_fn(), &[s.into(), sub.into(), f.i64(0).into()]);
                    f.ret(f.sge(at, f.i64(0)));
                }
                M::StartsWith | M::EndsWith => {
                    f.if_(f.sgt(lsub, ls), |f| f.ret(f.bool(false)));
                    let from = if which == M::StartsWith { f.i64(0) } else { f.sub(ls, lsub) };
                    let c = f.call_int(
                        f.cx.libc("memcmp"),
                        &[f.at(f.str_data(s), from).into(), f.str_data(sub).into(), lsub.into()],
                    );
                    f.ret(f.eq(c, f.i32(0)));
                }
                M::IndexOf | M::LastIndexOf => {
                    let at = if which == M::IndexOf {
                        f.call_int(f.cx.str_find_fn(), &[s.into(), sub.into(), f.i64(0).into()])
                    } else {
                        f.call_int(f.cx.str_rfind_fn(), &[s.into(), sub.into()])
                    };
                    f.if_(f.slt(at, f.i64(0)), |f| f.ret(f.i64(-1)));
                    f.ret(f.call_int(f.cx.str_prefix_chars_fn(), &[s.into(), at.into()]));
                }
                M::Count => {
                    f.if_(f.eq(lsub, f.i64(0)), |f| f.ret(f.i64(0)));
                    let n = f.var(f.i64(0));
                    let from = f.var(f.i64(0));
                    f.while_(
                        |_| f.bool(true),
                        |f, exit| {
                            let at = f.call_int(
                                f.cx.str_find_fn(),
                                &[s.into(), sub.into(), f.load_i64(from).into()],
                            );
                            f.if_(f.slt(at, f.i64(0)), |f| f.br(exit));
                            f.store(n, f.add(f.load_i64(n), f.i64(1)));
                            f.store(from, f.add(at, lsub));
                        },
                    );
                    f.ret(f.load_i64(n));
                }
                _ => f.unreachable(),
            }
        })
    }

    /// Purpose: This function returns `jade.str.replace(s, from, to) -> str`.
    /// An empty `from` gives back the string unchanged, as v1 did.
    pub fn str_replace_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support(
            "jade.str.replace",
            Some(ptr.into()),
            &[ptr.into(), ptr.into(), ptr.into()],
            |f| {
                let (s, from, to) = (f.param_ptr(0), f.param_ptr(1), f.param_ptr(2));
                let lfrom = f.str_len(from);
                f.if_(f.eq(lfrom, f.i64(0)), |f| {
                    f.retain(&crate::mir::Ty::Str, s.into());
                    f.ret(s);
                });
                let sb = f.sb_new();
                f.sb_taint(sb, f.str_trust(s));
                let data = f.str_data(s);
                let pos = f.var(f.i64(0));
                f.while_(
                    |_| f.bool(true),
                    |f, exit| {
                        let p = f.load_i64(pos);
                        let at = f.call_int(f.cx.str_find_fn(), &[s.into(), from.into(), p.into()]);
                        f.if_(f.slt(at, f.i64(0)), |f| f.br(exit));
                        f.sb_push(sb, f.at(data, p), f.sub(at, p));
                        f.sb_push_str(sb, to);
                        f.store(pos, f.add(at, lfrom));
                    },
                );
                let p = f.load_i64(pos);
                f.sb_push(sb, f.at(data, p), f.sub(f.str_len(s), p));
                f.ret(f.sb_finish(sb));
            },
        )
    }

    /// Purpose: This function returns `jade.str.repeat(s, n) -> str`.
    pub fn str_repeat_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support("jade.str.repeat", Some(ptr.into()), &[ptr.into(), self.i64t().into()], |f| {
            let (s, n) = (f.param_ptr(0), f.param_int(1));
            let sb = f.sb_new();
            f.for_range(f.i64(0), n, |f, _, _| f.sb_push_str(sb, s));
            f.ret(f.sb_finish(sb));
        })
    }

    /// Purpose: This function returns `jade.str.slice(s, start, end) -> str`,
    /// by character index, clamped as v1 clamped it.
    pub fn str_slice_fn(&self) -> FunctionValue<'ctx> {
        let (ptr, i64t) = (self.ptrt(), self.i64t());
        self.support(
            "jade.str.slice",
            Some(ptr.into()),
            &[ptr.into(), i64t.into(), i64t.into()],
            |f| {
                let (s, a, b) = (f.param_ptr(0), f.param_int(1), f.param_int(2));
                let ba = f.call_int(f.cx.str_byteat_fn(), &[s.into(), a.into()]);
                let bb = f.call_int(f.cx.str_byteat_fn(), &[s.into(), b.into()]);
                let len = f.select_int(f.sge(ba, bb), f.i64(0), f.sub(bb, ba));
                f.ret(f.str_from_trust(f.at(f.str_data(s), ba), len, f.str_trust(s)));
            },
        )
    }

    /// Purpose: This function returns `jade.str.pad(s, width, pad, at_start) -> str`.
    pub fn str_pad_fn(&self) -> FunctionValue<'ctx> {
        let (ptr, i64t) = (self.ptrt(), self.i64t());
        self.support(
            "jade.str.pad",
            Some(ptr.into()),
            &[ptr.into(), i64t.into(), ptr.into(), self.i1t().into()],
            |f| {
                let (s, width, pad, at_start) =
                    (f.param_ptr(0), f.param_int(1), f.param_ptr(2), f.param_int(3));
                let have = f.call_int(f.cx.str_charlen_fn(), &[s.into()]);
                let no_pad = f.or(f.eq(f.str_len(pad), f.i64(0)), f.sge(have, width));
                f.if_(no_pad, |f| {
                    f.retain(&crate::mir::Ty::Str, s.into());
                    f.ret(s);
                });
                let need = f.sub(width, have);
                let chars = f.call_ptr(f.cx.str_chars_fn(), &[pad.into()]);
                let n = f.load_i64(f.at_c(chars, LEN));
                let buf = f.load_ptr(f.at_c(chars, ARR_DATA));
                let sb = f.sb_new();
                let fill = |f: &F<'_, 'ctx>| {
                    f.for_range(f.i64(0), need, |f, i, _| {
                        let cp = f.load_i32(f.elem(f.cx.i32t(), buf, f.srem(i, n)));
                        f.sb_push_cp(sb, cp);
                    });
                };
                f.if_else(
                    at_start,
                    |f| {
                        fill(f);
                        f.sb_push_str(sb, s);
                    },
                    |f| {
                        f.sb_push_str(sb, s);
                        fill(f);
                    },
                );
                f.release(&crate::mir::Ty::Array(Box::new(crate::mir::Ty::Char)), chars.into());
                f.ret(f.sb_finish(sb));
            },
        )
    }

    /// Purpose: This function returns `jade.sb.push(sb, bytes, n)`, appending
    /// to a string builder.
    fn sb_push_fn(&self) -> FunctionValue<'ctx> {
        let (ptr, i64t) = (self.ptrt(), self.i64t());
        self.support("jade.sb.push", None, &[ptr.into(), ptr.into(), i64t.into()], |f| {
            let (sb, src, n) = (f.param_ptr(0), f.param_ptr(1), f.param_int(2));
            let buf_p = sb;
            let len_p = f.at_c(sb, 8);
            let cap_p = f.at_c(sb, 16);
            let len = f.load_i64(len_p);
            let need = f.add(len, n);
            f.if_(f.sgt(need, f.load_i64(cap_p)), |f| {
                let doubled = f.mul(f.load_i64(cap_p), f.i64(2));
                let cap = f.select_int(f.sgt(need, doubled), need, doubled);
                let cap = f.select_int(f.slt(cap, f.i64(32)), f.i64(32), cap);
                let nb = f.call_ptr(f.cx.libc("realloc"), &[f.load_ptr(buf_p).into(), cap.into()]);
                f.store(buf_p, nb);
                f.store(cap_p, cap);
            });
            f.memcpy(f.at(f.load_ptr(buf_p), len), src, n);
            f.store(len_p, need);
            f.ret_void();
        })
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function reads a string's byte length.
    pub fn str_len(&self, s: PointerValue<'ctx>) -> IntValue<'ctx> {
        self.load_i64(self.at_c(s, LEN))
    }

    /// Purpose: This function reads a string's trust: 0 trusted, 1 tainted.
    pub fn str_trust(&self, s: PointerValue<'ctx>) -> IntValue<'ctx> {
        self.load_i64(self.at_c(s, STR_TRUST))
    }

    /// Purpose: This function sets the trust of a string this code just made.
    /// It must never be called on a string anything else holds, or on a
    /// literal, which is read-only.
    pub fn set_trust(&self, s: PointerValue<'ctx>, trust: IntValue<'ctx>) {
        self.store(self.at_c(s, STR_TRUST), trust);
    }

    /// Purpose: This function makes a string from bytes with a given trust.
    pub fn str_from_trust(
        &self,
        src: PointerValue<'ctx>,
        len: IntValue<'ctx>,
        trust: IntValue<'ctx>,
    ) -> PointerValue<'ctx> {
        let s = self.call_ptr(self.cx.str_from_fn(), &[src.into(), len.into()]);
        self.set_trust(s, trust);
        s
    }

    /// Purpose: This function strips the taint bit from a char, leaving the
    /// Unicode scalar.
    pub fn char_cp(&self, c: IntValue<'ctx>) -> IntValue<'ctx> {
        self.and(c, self.i32(!CHAR_TAINT & 0xffff_ffff))
    }

    /// Purpose: This function reads a char's taint bit as a trust word.
    pub fn char_trust(&self, c: IntValue<'ctx>) -> IntValue<'ctx> {
        let bit = self.lshr(c, self.i32(31));
        self.zext(bit, self.cx.i64t())
    }

    /// Purpose: This function marks a scalar with a string's trust.
    pub fn char_with_trust(&self, cp: IntValue<'ctx>, trust: IntValue<'ctx>) -> IntValue<'ctx> {
        let bit = self.shl(self.trunc(trust, self.cx.i32t()), self.i32(31));
        self.or(cp, bit)
    }

    /// Purpose: This function returns a pointer to a string's first byte.
    pub fn str_data(&self, s: PointerValue<'ctx>) -> PointerValue<'ctx> {
        self.at_c(s, STR_DATA)
    }

    /// Purpose: This function makes a new string of `len` unset bytes.
    pub fn str_new(&self, len: IntValue<'ctx>) -> PointerValue<'ctx> {
        self.call_ptr(self.cx.str_alloc_fn(), &[len.into()])
    }

    /// Purpose: This function joins strings into a new one, in one allocation.
    ///
    /// Inputs:
    /// - `parts`: the strings.
    ///
    /// Outputs: the joined string, owned, tainted when any part is.
    pub fn str_cat(&self, parts: &[PointerValue<'ctx>]) -> PointerValue<'ctx> {
        let mut total = self.i64(0);
        let mut trust = self.i64(0);
        let lens: Vec<IntValue> = parts.iter().map(|p| self.str_len(*p)).collect();
        for (l, p) in lens.iter().zip(parts) {
            total = self.add(total, *l);
            trust = self.or(trust, self.str_trust(*p));
        }
        let s = self.str_new(total);
        self.set_trust(s, trust);
        let mut off = self.i64(0);
        for (p, l) in parts.iter().zip(&lens) {
            self.memcpy(self.at(self.str_data(s), off), self.str_data(*p), *l);
            off = self.add(off, *l);
        }
        s
    }

    /// Purpose: This function makes an empty string builder on the stack.
    ///
    /// Outputs: a pointer to its `{ ptr buf, i64 len, i64 cap, i64 trust }`.
    pub fn sb_new(&self) -> PointerValue<'ctx> {
        let i = self.cx.i64t();
        let t =
            self.cx.ctx.struct_type(&[self.cx.ptrt().into(), i.into(), i.into(), i.into()], false);
        let sb = self.alloca(t);
        self.store(sb, self.null());
        self.store(self.at_c(sb, 8), self.i64(0));
        self.store(self.at_c(sb, 16), self.i64(0));
        self.store(self.at_c(sb, 24), self.i64(0));
        sb
    }

    /// Purpose: This function marks what a string builder makes as derived
    /// from something with this trust, such as a string whose bytes are pushed
    /// a piece at a time.
    pub fn sb_taint(&self, sb: PointerValue<'ctx>, trust: IntValue<'ctx>) {
        let p = self.at_c(sb, 24);
        self.store(p, self.or(self.load_i64(p), trust));
    }

    /// Purpose: This function appends bytes to a string builder.
    pub fn sb_push(&self, sb: PointerValue<'ctx>, src: PointerValue<'ctx>, n: IntValue<'ctx>) {
        self.call(self.cx.sb_push_fn(), &[sb.into(), src.into(), n.into()]);
    }

    /// Purpose: This function appends a string to a string builder, with its trust.
    pub fn sb_push_str(&self, sb: PointerValue<'ctx>, s: PointerValue<'ctx>) {
        self.sb_taint(sb, self.str_trust(s));
        self.sb_push(sb, self.str_data(s), self.str_len(s));
    }

    /// Purpose: This function appends literal text to a string builder.
    pub fn sb_push_lit(&self, sb: PointerValue<'ctx>, text: &str) {
        if text.is_empty() {
            return;
        }
        let s = self.cx.lit(text);
        self.sb_push_str(sb, s);
    }

    /// Purpose: This function appends one scalar to a string builder.
    pub fn sb_push_cp(&self, sb: PointerValue<'ctx>, cp: IntValue<'ctx>) {
        self.sb_taint(sb, self.char_trust(cp));
        let buf = self.alloca(self.cx.i8t().array_type(4));
        let n = self.call_int(self.cx.utf8_encode_fn(), &[cp.into(), buf.into()]);
        self.sb_push(sb, buf, n);
    }

    /// Purpose: This function turns a string builder into a string and frees
    /// its buffer.
    ///
    /// Outputs: the string, owned.
    pub fn sb_finish(&self, sb: PointerValue<'ctx>) -> PointerValue<'ctx> {
        let buf = self.load_ptr(sb);
        let len = self.load_i64(self.at_c(sb, 8));
        let s = self.str_from_trust(buf, len, self.load_i64(self.at_c(sb, 24)));
        self.call(self.cx.libc("free"), &[buf.into()]);
        s
    }

    /// Purpose: This function calls a string method's support function.
    ///
    /// Inputs:
    /// - `m`: the method.
    /// - `args`: the receiver, then the method's arguments.
    ///
    /// Outputs: the result.
    pub fn str_method(
        &self,
        m: crate::mir::StrMethod,
        args: &[BasicValueEnum<'ctx>],
    ) -> BasicValueEnum<'ctx> {
        use crate::mir::StrMethod as M;
        let cx = self.cx;
        match m {
            M::Upper | M::Lower | M::Capitalize | M::Trim | M::TrimStart | M::TrimEnd => {
                self.callv(cx.str_unary_fn(m), args)
            }
            M::Split => self.callv(cx.str_split_fn(), args),
            M::Lines => self.callv(cx.str_lines_fn(), args),
            M::Contains | M::StartsWith | M::EndsWith | M::IndexOf | M::LastIndexOf | M::Count => {
                self.callv(cx.str_search_fn(m), args)
            }
            M::Replace => self.callv(cx.str_replace_fn(), args),
            M::Repeat => self.callv(cx.str_repeat_fn(), args),
            M::Slice => self.callv(cx.str_slice_fn(), args),
            M::PadStart | M::PadEnd => {
                let mut a = args.to_vec();
                a.push(self.bool(m == M::PadStart).into());
                self.callv(cx.str_pad_fn(), &a)
            }
            M::IsEmpty => {
                let s = args[0].into_pointer_value();
                self.eq(self.str_len(s), self.i64(0)).into()
            }
        }
    }
}
