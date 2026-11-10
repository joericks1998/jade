//! Bytes: a fixed-length, mutable buffer of octets, shared by reference.
//!
//! A bytes value has the string layout, `{ i64 rc, i64 len, i64 trust, u8
//! data[len], u8 0 }`, so it releases as a string does and carries a trust word
//! the same way. Unlike a string it changes in place: `b[i] = v` writes one
//! octet, and every holder of the value sees it. An octet reads back as an int,
//! never a char, since a byte is not a Unicode scalar.
//!
//! Trust follows the data. `fs.read_bytes` makes a tainted value, `slice` and
//! `concat` keep the taint, and `decode` hands it to the string, so a file
//! cannot become a trusted command by passing through bytes.
//!
//! The messages match v1's `jade-runtime::bytesf`, since a program can catch
//! and print them.

use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use super::layout::{ARR_DATA, LEN};
use super::{Cx, F};

/// The longest bytes value, as in v1, whose header held a 32-bit length.
const MAX_LEN: i64 = u32::MAX as i64;

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns `jade.bytes.get(b, i) -> int`.
    pub fn bytes_get_fn(&self) -> FunctionValue<'ctx> {
        let i = self.i64t();
        self.support("jade.bytes.get", Some(i.into()), &[self.ptrt().into(), i.into()], |f| {
            let (b, i) = (f.param_ptr(0), f.param_int(1));
            let out = f.or(f.slt(i, f.i64(0)), f.sge(i, f.str_len(b)));
            f.fail_if(out, "bytes index out of bounds");
            let v = f.load_i8(f.at(f.str_data(b), i));
            f.ret(f.zext(v, f.cx.i64t()));
        })
    }

    /// Purpose: This function returns `jade.bytes.set(b, i, v)`, which writes
    /// one octet in place.
    pub fn bytes_set_fn(&self) -> FunctionValue<'ctx> {
        let i = self.i64t();
        self.support("jade.bytes.set", None, &[self.ptrt().into(), i.into(), i.into()], |f| {
            let (b, i, v) = (f.param_ptr(0), f.param_int(1), f.param_int(2));
            let len = f.str_len(b);
            f.if_(f.or(f.slt(i, f.i64(0)), f.sge(i, len)), |f| {
                let m = f.format("index %lld out of bounds (length %lld)", &[i, len]);
                f.fail_str(m);
            });
            f.if_(f.or(f.slt(v, f.i64(0)), f.sgt(v, f.i64(255))), |f| {
                let m = f.format("a bytes element is an octet in 0 to 255, got %lld", &[v]);
                f.fail_str(m);
            });
            f.store(f.at(f.str_data(b), i), f.trunc(v, f.cx.i8t()));
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.bytes.decode(b) -> str`: the
    /// octets as UTF-8 text, which must be valid, with the bytes' trust.
    pub fn bytes_decode_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support("jade.bytes.decode", Some(ptr.into()), &[ptr.into()], |f| {
            let b = f.param_ptr(0);
            let bad = f.call_int(f.cx.utf8_invalid_at_fn(), &[b.into()]);
            f.if_(f.sge(bad, f.i64(0)), |f| {
                let m = f.format(
                    &format!("{}bytes.decode(): not valid UTF-8 at byte %lld", super::fs::IO_ERROR),
                    &[bad],
                );
                f.fail_str(m);
            });
            f.ret(f.copy_bytes(b, f.i64(0), f.str_len(b)));
        })
    }

    /// Purpose: This function returns `jade.utf8.invalid_at(b) -> int`: where
    /// the first invalid UTF-8 sequence starts, or -1. It applies Rust's rules:
    /// no overlong forms, no surrogates, nothing past U+10FFFF.
    pub fn utf8_invalid_at_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.utf8.invalid_at", Some(self.i64t().into()), &[self.ptrt().into()], |f| {
            let b = f.param_ptr(0);
            let (data, n) = (f.str_data(b), f.str_len(b));
            let i = f.var(f.i64(0));
            let byte = |f: &F<'_, 'ctx>, at: IntValue<'ctx>| {
                f.zext(f.load_i8(f.at(data, at)), f.cx.i64t())
            };
            let within =
                |f: &F<'_, 'ctx>, v: IntValue<'ctx>, lo: IntValue<'ctx>, hi: IntValue<'ctx>| {
                    f.and(f.sge(v, lo), f.sle(v, hi))
                };
            f.while_(
                |f| f.slt(f.load_i64(i), n),
                |f, _| {
                    let at = f.load_i64(i);
                    let b0 = byte(f, at);
                    f.if_(f.slt(b0, f.i64(0x80)), |f| {
                        f.store(i, f.add(at, f.i64(1)));
                    });
                    f.if_(f.sge(b0, f.i64(0x80)), |f| {
                        let c = |lo: i64, hi: i64| within(f, b0, f.i64(lo), f.i64(hi));
                        // How many continuation bytes follow, and the range the
                        // first of them must fall in.
                        let need = f.select_int(c(0xc2, 0xdf), f.i64(1), f.i64(0));
                        let need = f.select_int(c(0xe0, 0xef), f.i64(2), need);
                        let need = f.select_int(c(0xf0, 0xf4), f.i64(3), need);
                        let lo = f.select_int(f.eq(b0, f.i64(0xe0)), f.i64(0xa0), f.i64(0x80));
                        let lo = f.select_int(f.eq(b0, f.i64(0xf0)), f.i64(0x90), lo);
                        let hi = f.select_int(f.eq(b0, f.i64(0xed)), f.i64(0x9f), f.i64(0xbf));
                        let hi = f.select_int(f.eq(b0, f.i64(0xf4)), f.i64(0x8f), hi);
                        let lead_bad = f.eq(need, f.i64(0));
                        let short = f.sge(f.add(at, need), n);
                        f.if_(f.or(lead_bad, short), |f| f.ret(at));
                        f.if_(f.not(within(f, byte(f, f.add(at, f.i64(1))), lo, hi)), |f| {
                            f.ret(at)
                        });
                        f.for_range(f.i64(2), f.add(need, f.i64(1)), |f, k, _| {
                            let v = byte(f, f.add(at, k));
                            f.if_(f.not(within(f, v, f.i64(0x80), f.i64(0xbf))), |f| f.ret(at));
                        });
                        f.store(i, f.add(at, f.add(need, f.i64(1))));
                    });
                },
            );
            f.ret(f.i64(-1));
        })
    }

    /// Purpose: This function returns `jade.utf8.lossy(b) -> str`: the octets
    /// as text, each invalid sequence replaced by U+FFFD, with Rust's rule for
    /// how much one replacement covers: a bad lead byte alone, or a good lead
    /// and the continuation bytes that fit it before the sequence broke.
    pub fn utf8_lossy_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.utf8.lossy", Some(p.into()), &[p.into()], |f| {
            let b = f.param_ptr(0);
            let (data, n) = (f.str_data(b), f.str_len(b));
            let sb = f.sb_new();
            f.sb_taint(sb, f.str_trust(b));
            let i = f.var(f.i64(0));
            let byte = |f: &F<'_, 'ctx>, at: IntValue<'ctx>| {
                f.zext(f.load_i8(f.at(data, at)), f.cx.i64t())
            };
            let within =
                |f: &F<'_, 'ctx>, v: IntValue<'ctx>, lo: IntValue<'ctx>, hi: IntValue<'ctx>| {
                    f.and(f.sge(v, lo), f.sle(v, hi))
                };
            f.while_(
                |f| f.slt(f.load_i64(i), n),
                |f, _| {
                    let at = f.load_i64(i);
                    let b0 = byte(f, at);
                    let c = |lo: i64, hi: i64| within(f, b0, f.i64(lo), f.i64(hi));
                    let need = f.select_int(c(0xc2, 0xdf), f.i64(1), f.i64(0));
                    let need = f.select_int(c(0xe0, 0xef), f.i64(2), need);
                    let need = f.select_int(c(0xf0, 0xf4), f.i64(3), need);
                    let need = f.select_int(f.slt(b0, f.i64(0x80)), f.i64(0), need);
                    let lo = f.select_int(f.eq(b0, f.i64(0xe0)), f.i64(0xa0), f.i64(0x80));
                    let lo = f.select_int(f.eq(b0, f.i64(0xf0)), f.i64(0x90), lo);
                    let hi = f.select_int(f.eq(b0, f.i64(0xed)), f.i64(0x9f), f.i64(0xbf));
                    let hi = f.select_int(f.eq(b0, f.i64(0xf4)), f.i64(0x8f), hi);
                    // `k` counts the bytes this step consumes.
                    let k = f.var(f.i64(1));
                    let has_next = f.slt(f.add(at, f.i64(1)), n);
                    f.if_(f.and(f.sgt(need, f.i64(0)), has_next), |f| {
                        f.if_(within(f, byte(f, f.add(at, f.i64(1))), lo, hi), |f| {
                            f.store(k, f.i64(2));
                            f.while_(
                                |f| {
                                    let kk = f.load_i64(k);
                                    let room = f.and(f.sle(kk, need), f.slt(f.add(at, kk), n));
                                    let ok = f.var(f.bool(false));
                                    f.if_(room, |f| {
                                        let v = byte(f, f.add(at, kk));
                                        f.store(ok, within(f, v, f.i64(0x80), f.i64(0xbf)));
                                    });
                                    f.load(f.cx.i1t(), ok).into_int_value()
                                },
                                |f, _| f.store(k, f.add(f.load_i64(k), f.i64(1))),
                            );
                        });
                    });
                    let kk = f.load_i64(k);
                    let ascii = f.slt(b0, f.i64(0x80));
                    let whole = f.eq(kk, f.add(need, f.i64(1)));
                    f.if_else(
                        f.or(ascii, f.and(f.sgt(need, f.i64(0)), whole)),
                        |f| f.sb_push(sb, f.at(data, at), kk),
                        |f| f.sb_push_lit(sb, "\u{fffd}"),
                    );
                    f.store(i, f.add(at, kk));
                },
            );
            f.ret(f.sb_finish(sb));
        })
    }

    /// Purpose: This function returns `jade.bytes.slice(b, start, end) ->
    /// bytes`: a copy of a range, both ends clamped, with the bytes' trust.
    pub fn bytes_slice_fn(&self) -> FunctionValue<'ctx> {
        let (p, i) = (self.ptrt(), self.i64t());
        self.support("jade.bytes.slice", Some(p.into()), &[p.into(), i.into(), i.into()], |f| {
            let (b, s, e) = (f.param_ptr(0), f.param_int(1), f.param_int(2));
            let len = f.str_len(b);
            let clamp = |v: IntValue<'ctx>, lo: IntValue<'ctx>| {
                let v = f.select_int(f.slt(v, lo), lo, v);
                f.select_int(f.sgt(v, len), len, v)
            };
            let start = clamp(s, f.i64(0));
            let end = clamp(e, start);
            f.ret(f.copy_bytes(b, start, f.sub(end, start)));
        })
    }

    /// Purpose: This function returns `jade.bytes.zeros(n) -> bytes`.
    pub fn bytes_zeros_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.bytes.zeros", Some(self.ptrt().into()), &[self.i64t().into()], |f| {
            let n = f.param_int(0);
            f.if_(f.slt(n, f.i64(0)), |f| {
                let m = f.format("bytes.zeros(): length cannot be negative, got %lld", &[n]);
                f.fail_str(m);
            });
            f.if_(f.sgt(n, f.i64(MAX_LEN)), |f| {
                let m = f.format(
                    &format!("bytes.zeros(): length %lld is past the {MAX_LEN} octet limit"),
                    &[n],
                );
                f.fail_str(m);
            });
            let b = f.str_new(n);
            f.b.build_memset(f.str_data(b), 1, f.i8(0), n).expect("positioned");
            f.ret(b);
        })
    }

    /// Purpose: This function returns `jade.bytes.from_ints(a) -> bytes`.
    pub fn bytes_from_ints_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.bytes.from_ints", Some(p.into()), &[p.into()], |f| {
            let a = f.param_ptr(0);
            let n = f.load_i64(f.at_c(a, LEN));
            let src = f.load_ptr(f.at_c(a, ARR_DATA));
            let b = f.str_new(n);
            let data = f.str_data(b);
            f.for_range(f.i64(0), n, |f, i, _| {
                let v = f.load_i64(f.elem(f.cx.i64t(), src, i));
                f.if_(f.or(f.slt(v, f.i64(0)), f.sgt(v, f.i64(255))), |f| {
                    f.release(&crate::mir::Ty::Bytes, b.into());
                    let m = f.format(
                        "bytes.from_ints(): element %lld is %lld, which is not an octet in 0 to 255",
                        &[i, v],
                    );
                    f.fail_str(m);
                });
                f.store(f.at(data, i), f.trunc(v, f.cx.i8t()));
            });
            f.ret(b);
        })
    }

    /// Purpose: This function returns `jade.bytes.concat(a, b) -> bytes`,
    /// tainted when either is.
    pub fn bytes_concat_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.bytes.concat", Some(p.into()), &[p.into(), p.into()], |f| {
            let (a, b) = (f.param_ptr(0), f.param_ptr(1));
            let total = f.add(f.str_len(a), f.str_len(b));
            f.if_(f.sgt(total, f.i64(MAX_LEN)), |f| {
                let m = f.format(
                    &format!(
                        "bytes.concat(): the result would be %lld octets, past the {MAX_LEN} octet limit"
                    ),
                    &[total],
                );
                f.fail_str(m);
            });
            f.ret(f.str_cat(&[a, b]));
        })
    }

    /// Purpose: This function returns `jade.bytes.render(b) -> str`: `b"…"`
    /// with every octet outside printable ASCII escaped, as v1 printed it.
    pub fn bytes_render_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.bytes.render", Some(p.into()), &[p.into()], |f| {
            let b = f.param_ptr(0);
            let sb = f.sb_new();
            f.sb_taint(sb, f.str_trust(b));
            f.sb_push_lit(sb, "b\"");
            let data = f.str_data(b);
            let hex = f.alloca(f.cx.i8t().array_type(8));
            f.for_range(f.i64(0), f.str_len(b), |f, i, _| {
                let c = f.zext(f.load_i8(f.at(data, i)), f.cx.i32t());
                let done = f.block();
                let other = f.block();
                let mut cases = Vec::new();
                let mut bodies = Vec::new();
                for (ch, text) in [(92, "\\\\"), (34, "\\\""), (10, "\\n"), (13, "\\r"), (9, "\\t")]
                {
                    let bb = f.block();
                    cases.push((f.i32(ch), bb));
                    bodies.push((bb, text));
                }
                f.switch(c, other, &cases);
                for (bb, text) in bodies {
                    f.position(bb);
                    f.sb_push_lit(sb, text);
                    f.br(done);
                }
                f.position(other);
                let printable = f.and(
                    f.icmp(inkwell::IntPredicate::UGE, c, f.i32(0x20)),
                    f.icmp(inkwell::IntPredicate::ULE, c, f.i32(0x7e)),
                );
                f.if_else(
                    printable,
                    |f| f.sb_push(sb, f.at(data, i), f.i64(1)),
                    |f| {
                        let n = f.call_int(
                            f.cx.libc("snprintf"),
                            &[hex.into(), f.i64(8).into(), f.cx.cstr("\\x%02x").into(), c.into()],
                        );
                        f.sb_push(sb, hex, f.sext(n, f.cx.i64t()));
                    },
                );
                f.br(done);
                f.position(done);
            });
            f.sb_push_lit(sb, "\"");
            f.ret(f.sb_finish(sb));
        })
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function copies a range of a string or bytes value into a
    /// new one with the same trust. Since the layouts match, a string's copy is
    /// bytes and a bytes value's copy is a string, as the caller needs.
    pub fn copy_bytes(
        &self,
        b: PointerValue<'ctx>,
        start: IntValue<'ctx>,
        n: IntValue<'ctx>,
    ) -> PointerValue<'ctx> {
        self.str_from_trust(self.at(self.str_data(b), start), n, self.str_trust(b))
    }

    /// Purpose: This function formats ints into a new string with `snprintf`.
    ///
    /// Inputs:
    /// - `fmt`: a C format whose conversions are all `%lld`.
    /// - `args`: the ints.
    ///
    /// Outputs: the text, owned.
    pub fn format(&self, fmt: &str, args: &[IntValue<'ctx>]) -> PointerValue<'ctx> {
        let buf = self.alloca(self.cx.i8t().array_type(256));
        let mut vals = vec![buf.into(), self.i64(256).into(), self.cx.cstr(fmt).into()];
        vals.extend(args.iter().map(|a| BasicValueEnum::from(*a)));
        let n = self.call_int(self.cx.libc("snprintf"), &vals);
        let n = self.sext(n, self.cx.i64t());
        let n = self.select_int(self.sgt(n, self.i64(255)), self.i64(255), n);
        self.call_ptr(self.cx.str_from_fn(), &[buf.into(), n.into()])
    }
}
