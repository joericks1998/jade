//! Text for values, output, input, and conversions between kinds.
//!
//! `print`, `str()` and f-strings all show a value the same way, and they all
//! come through `jade.tostr.<type>`, one function per type a program shows.
//! The rules are v1's (`src/runtime/src/render.rs`): strings and chars as their
//! text, even inside a container; floats as the shortest digits that read back
//! to the same number, never in exponent form, with `.0` on a whole number;
//! dicts with their keys sorted and quoted; a struct as `<struct>`.
//!
//! Floats are the one subtle case. The C library finds the shortest digits:
//! `snprintf("%.*e")` at increasing precision until `strtod` reads the same
//! value back. Those digits are then laid out in positional form here, so
//! `48000.0` prints as `48000.0` and not `4.8e+04`.

use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use super::layout::{ARR_DATA, LEN};
use super::{Cx, F};
use crate::mir::Ty;

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns `jade.write(fd, bytes, n)`, which writes
    /// every byte, retrying after a short write.
    pub fn write_fn(&self) -> FunctionValue<'ctx> {
        let (ptr, i64t) = (self.ptrt(), self.i64t());
        self.support("jade.write", None, &[self.i32t().into(), ptr.into(), i64t.into()], |f| {
            let (fd, p, n) = (f.param_int(0), f.param_ptr(1), f.param_int(2));
            let off = f.var(f.i64(0));
            f.while_(
                |f| f.slt(f.load_i64(off), n),
                |f, exit| {
                    let o = f.load_i64(off);
                    let r = f.call_int(
                        f.cx.libc("write"),
                        &[fd.into(), f.at(p, o).into(), f.sub(n, o).into()],
                    );
                    f.if_(f.sle(r, f.i64(0)), |f| f.br(exit));
                    f.store(off, f.add(o, r));
                },
            );
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.print(text, end)`.
    pub fn print_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support("jade.print", None, &[ptr.into(), ptr.into()], |f| {
            // One write when the ending is a newline, so a line is never split.
            let (s, end) = (f.param_ptr(0), f.param_ptr(1));
            let whole = f.str_cat(&[s, end]);
            f.write_str(1, whole);
            f.call(f.cx.libc("free"), &[whole.into()]);
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.input(prompt) -> str`: the prompt
    /// written, then one line read, without its line ending.
    pub fn input_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support("jade.input", Some(ptr.into()), &[ptr.into()], |f| {
            f.write_str(1, f.param_ptr(0));
            let sb = f.sb_new();
            let byte = f.alloca(f.cx.i8t());
            f.while_(
                |_| f.bool(true),
                |f, exit| {
                    let n = f.call_int(
                        f.cx.libc("read"),
                        &[f.i32(0).into(), byte.into(), f.i64(1).into()],
                    );
                    f.if_(f.sle(n, f.i64(0)), |f| f.br(exit));
                    let b = f.load_i8(byte);
                    f.if_(f.eq(b, f.i8(10)), |f| f.br(exit));
                    f.sb_push(sb, byte, f.i64(1));
                },
            );
            // Drop a carriage return left by a \r\n line ending.
            let len_p = f.at_c(sb, 8);
            let len = f.load_i64(len_p);
            f.if_(f.sgt(len, f.i64(0)), |f| {
                let last = f.load_i8(f.at(f.load_ptr(sb), f.sub(len, f.i64(1))));
                f.if_(f.eq(last, f.i8(13)), |f| f.store(len_p, f.sub(len, f.i64(1))));
            });
            f.ret(f.sb_finish(sb));
        })
    }

    /// Purpose: This function returns `jade.tostr.<type>(v) -> str`, the text
    /// `print` shows for a value of that type.
    ///
    /// Inputs:
    /// - `t`: the type.
    ///
    /// Outputs: the function. Its result is owned.
    pub fn tostr_fn(&self, t: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.tostr.{}", super::rc::key(t));
        let arg = self.llty(t);
        let t = t.clone();
        self.support(&name, Some(self.ptrt().into()), &[arg], |f| {
            let v = f.param(0);
            let s = match &t {
                Ty::Int => f.int_text(v.into_int_value()),
                Ty::Float => f.call_ptr(f.cx.float_tostr_fn(), &[v]),
                Ty::Bool => f
                    .select(v.into_int_value(), f.cx.lit("true"), f.cx.lit("false"))
                    .into_pointer_value(),
                Ty::Nil => f.cx.lit("nil"),
                Ty::Char => f.call_ptr(f.cx.char_tostr_fn(), &[v]),
                Ty::Str => {
                    f.retain(&Ty::Str, v);
                    v.into_pointer_value()
                }
                Ty::Opt(inner) => {
                    let out = f.var(f.cx.lit("nil"));
                    let absent = f.is_nil(&t, v);
                    f.if_(f.not(absent), |f| {
                        let x = f.unwrap(&t, v);
                        f.store(out, f.call_ptr(f.cx.tostr_fn(inner), &[x]));
                    });
                    f.load_ptr(out)
                }
                Ty::Array(el) => {
                    let a = v.into_pointer_value();
                    let sb = f.sb_new();
                    f.sb_push_lit(sb, "[");
                    let len = f.load_i64(f.at_c(a, LEN));
                    let data = f.load_ptr(f.at_c(a, ARR_DATA));
                    let et = f.cx.llty(el);
                    f.for_range(f.i64(0), len, |f, i, _| {
                        f.if_(f.sgt(i, f.i64(0)), |f| f.sb_push_lit(sb, ", "));
                        let x = f.load(et, f.elem(et, data, i));
                        f.sb_push_value(sb, el, x);
                    });
                    f.sb_push_lit(sb, "]");
                    f.sb_finish(sb)
                }
                Ty::Map(vt) => {
                    let d = v.into_pointer_value();
                    let keys = f.call_ptr(f.cx.dict_keys_fn(vt), &[d.into()]);
                    let sb = f.sb_new();
                    f.sb_push_lit(sb, "{");
                    let len = f.load_i64(f.at_c(keys, LEN));
                    let kdata = f.load_ptr(f.at_c(keys, ARR_DATA));
                    f.for_range(f.i64(0), len, |f, i, _| {
                        f.if_(f.sgt(i, f.i64(0)), |f| f.sb_push_lit(sb, ", "));
                        let k = f.load_ptr(f.elem(f.cx.ptrt(), kdata, i));
                        let q = f.call_ptr(f.cx.str_debug_fn(), &[k.into()]);
                        f.sb_push_str(sb, q);
                        f.release(&Ty::Str, q.into());
                        f.sb_push_lit(sb, ": ");
                        let x = f.dict_get(vt, d, k);
                        f.sb_push_value(sb, vt, x);
                    });
                    f.release(&Ty::Array(Box::new(Ty::Str)), keys.into());
                    f.sb_push_lit(sb, "}");
                    f.sb_finish(sb)
                }
                Ty::Struct(_) => f.cx.lit("<struct>"),
                // Functions print the way v1 named them.
                Ty::Func(crate::compiler::infer::FuncRef::Builtin(n)) => match n.as_str() {
                    "str" | "int" | "float" | "bool" | "char" => f.cx.lit(&format!("<type {n}>")),
                    "print" | "write" | "input" => f.cx.lit("<native fn>"),
                    other => f.cx.lit(&format!("<builtin {other}>")),
                },
                Ty::Func(_) | Ty::Fn(_) => f.cx.lit("<fn>"),
                // A caught error prints its message when it is a RuntimeError,
                // which is what a raised string becomes; a class prints as one.
                Ty::Error => {
                    let e = v.into_pointer_value();
                    f.if_(f.is_null(e), |f| f.ret(f.cx.lit("nil")));
                    let rt = f.eq(f.type_id(e), f.i64(f.cx.struct_id("RuntimeError") as i64));
                    let out = f.var(f.cx.lit("<struct>"));
                    f.if_(rt, |f| {
                        let st = f.cx.struct_ty("RuntimeError");
                        let m = f.load_ptr(f.field(st, e, 2));
                        f.retain(&Ty::Str, m.into());
                        f.store(out, m);
                    });
                    f.load_ptr(out)
                }
                Ty::Future(_) => f.cx.lit("<future>"),
                _ => f.cx.lit("<value>"),
            };
            f.ret(s);
        })
    }

    /// Purpose: This function returns `jade.str.debug(s) -> str`: a string
    /// quoted and escaped the way Rust's `{:?}` writes it, for dict keys.
    pub fn str_debug_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support("jade.str.debug", Some(ptr.into()), &[ptr.into()], |f| {
            let s = f.param_ptr(0);
            let sb = f.sb_new();
            f.sb_push_lit(sb, "\"");
            let data = f.str_data(s);
            let len = f.str_len(s);
            let off = f.var(f.i64(0));
            let step = f.alloca(f.cx.i64t());
            f.while_(
                |f| f.slt(f.load_i64(off), len),
                |f, _| {
                    let o = f.load_i64(off);
                    let cp =
                        f.call_int(f.cx.utf8_decode_fn(), &[f.at(data, o).into(), step.into()]);
                    let done = f.block();
                    let other = f.block();
                    let escapes = [
                        (34, "\\\""),
                        (92, "\\\\"),
                        (10, "\\n"),
                        (13, "\\r"),
                        (9, "\\t"),
                        (0, "\\0"),
                    ];
                    let mut cases = Vec::new();
                    let mut bodies = Vec::new();
                    for (c, text) in escapes {
                        let b = f.block();
                        cases.push((f.i32(c), b));
                        bodies.push((b, text));
                    }
                    f.switch(cp, other, &cases);
                    for (b, text) in bodies {
                        f.position(b);
                        f.sb_push_lit(sb, text);
                        f.br(done);
                    }
                    f.position(other);
                    let control = f.or(
                        f.icmp(inkwell::IntPredicate::ULT, cp, f.i32(0x20)),
                        f.eq(cp, f.i32(0x7f)),
                    );
                    f.if_else(
                        control,
                        |f| {
                            let buf = f.alloca(f.cx.i8t().array_type(16));
                            let n = f.call_int(
                                f.cx.libc("snprintf"),
                                &[
                                    buf.into(),
                                    f.i64(16).into(),
                                    f.cx.cstr("\\u{%x}").into(),
                                    cp.into(),
                                ],
                            );
                            f.sb_push(sb, buf, f.sext(n, f.cx.i64t()));
                        },
                        |f| f.sb_push(sb, f.at(data, o), f.load_i64(step)),
                    );
                    f.br(done);
                    f.position(done);
                    f.store(off, f.add(o, f.load_i64(step)));
                },
            );
            f.sb_push_lit(sb, "\"");
            f.ret(f.sb_finish(sb));
        })
    }

    /// Purpose: This function returns `jade.tostr.float(x) -> str`.
    pub fn float_tostr_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.tostr.float", Some(self.ptrt().into()), &[self.f64t().into()], |f| {
            let x = f.param(0).into_float_value();
            use inkwell::FloatPredicate as P;
            f.if_(f.fcmp(P::UNO, x, x), |f| f.ret(f.cx.lit("NaN")));
            f.if_(f.fcmp(P::OEQ, x, f.f64(f64::INFINITY)), |f| f.ret(f.cx.lit("inf")));
            f.if_(f.fcmp(P::OEQ, x, f.f64(f64::NEG_INFINITY)), |f| f.ret(f.cx.lit("-inf")));

            // The fewest significant digits that read back as `x`.
            let buf = f.alloca(f.cx.i8t().array_type(64));
            let prec = f.var(f.i32(0));
            f.while_(
                |f| {
                    let p = f.load_i32(prec);
                    f.call(
                        f.cx.libc("snprintf"),
                        &[
                            buf.into(),
                            f.i64(64).into(),
                            f.cx.cstr("%.*e").into(),
                            p.into(),
                            x.into(),
                        ],
                    );
                    let back = f
                        .callv(f.cx.libc("strtod"), &[buf.into(), f.null().into()])
                        .into_float_value();
                    let same = f.fcmp(P::OEQ, back, x);
                    f.and(f.not(same), f.slt(p, f.i32(17)))
                },
                |f, _| f.store(prec, f.add(f.load_i32(prec), f.i32(1))),
            );

            // `buf` is now `[-]d[.ddd]e±xx`. Gather the digits and the exponent.
            let digits = f.alloca(f.cx.i8t().array_type(32));
            let nd = f.var(f.i64(0));
            let i = f.var(f.i64(0));
            let neg = f.eq(f.load_i8(buf), f.i8(45));
            f.if_(neg, |f| f.store(i, f.i64(1)));
            f.while_(
                |f| {
                    let b = f.load_i8(f.at(buf, f.load_i64(i)));
                    f.ne(b, f.i8(101))
                },
                |f, _| {
                    let at = f.load_i64(i);
                    let b = f.load_i8(f.at(buf, at));
                    f.if_(f.ne(b, f.i8(46)), |f| {
                        let n = f.load_i64(nd);
                        f.store(f.at(digits, n), b);
                        f.store(nd, f.add(n, f.i64(1)));
                    });
                    f.store(i, f.add(at, f.i64(1)));
                },
            );
            let after_e = f.add(f.load_i64(i), f.i64(1));
            let exp_neg = f.eq(f.load_i8(f.at(buf, after_e)), f.i8(45));
            let e = f.var(f.i64(0));
            let j = f.var(f.add(after_e, f.i64(1)));
            f.while_(
                |f| f.ne(f.load_i8(f.at(buf, f.load_i64(j))), f.i8(0)),
                |f, _| {
                    let at = f.load_i64(j);
                    let d = f.sext(f.sub(f.load_i8(f.at(buf, at)), f.i8(48)), f.cx.i64t());
                    f.store(e, f.add(f.mul(f.load_i64(e), f.i64(10)), d));
                    f.store(j, f.add(at, f.i64(1)));
                },
            );
            let ev = f.load_i64(e);
            let exp = f.select_int(exp_neg, f.sub(f.i64(0), ev), ev);
            let k = f.load_i64(nd);

            let sb = f.sb_new();
            f.if_(neg, |f| f.sb_push_lit(sb, "-"));
            let zeros = |f: &F<'_, 'ctx>, n: IntValue<'ctx>| {
                f.for_range(f.i64(0), n, |f, _, _| f.sb_push_lit(sb, "0"));
            };
            let last = f.sub(k, f.i64(1));
            f.if_else(
                f.sge(exp, last),
                |f| {
                    // A whole number: every digit, zeros up to the point, then `.0`.
                    f.sb_push(sb, digits, k);
                    zeros(f, f.sub(exp, last));
                    f.sb_push_lit(sb, ".0");
                },
                |f| {
                    f.if_else(
                        f.sge(exp, f.i64(0)),
                        |f| {
                            let int_part = f.add(exp, f.i64(1));
                            f.sb_push(sb, digits, int_part);
                            f.sb_push_lit(sb, ".");
                            f.sb_push(sb, f.at(digits, int_part), f.sub(k, int_part));
                        },
                        |f| {
                            f.sb_push_lit(sb, "0.");
                            zeros(f, f.sub(f.sub(f.i64(0), exp), f.i64(1)));
                            f.sb_push(sb, digits, k);
                        },
                    );
                },
            );
            f.ret(f.sb_finish(sb));
        })
    }

    /// Purpose: This function returns `jade.toint.str(s) -> int`, parsing a
    /// decimal int with surrounding ASCII whitespace allowed, as v1 did.
    pub fn toint_str_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.toint.str", Some(self.i64t().into()), &[self.ptrt().into()], |f| {
            let s = f.param_ptr(0);
            let (a, b) = f.ascii_trim_span(s);
            let data = f.str_data(s);
            let bad = |f: &F<'_, 'ctx>| f.fail_quoted("int(): cannot convert \"", s, "\" to int");
            f.if_(f.sge(a, b), bad);
            let first = f.load_i8(f.at(data, a));
            let neg = f.eq(first, f.i8(45));
            let signed = f.or(neg, f.eq(first, f.i8(43)));
            let i = f.var(f.select_int(signed, f.add(a, f.i64(1)), a));
            f.if_(f.sge(f.load_i64(i), b), bad);
            // Accumulate as a negative number, which reaches i64::MIN.
            let acc = f.var(f.i64(0));
            f.while_(
                |f| f.slt(f.load_i64(i), b),
                |f, _| {
                    let at = f.load_i64(i);
                    let c = f.load_i8(f.at(data, at));
                    let digit = f.and(
                        f.icmp(inkwell::IntPredicate::UGE, c, f.i8(48)),
                        f.icmp(inkwell::IntPredicate::ULE, c, f.i8(57)),
                    );
                    f.if_(f.not(digit), bad);
                    let d = f.sext(f.sub(c, f.i8(48)), f.cx.i64t());
                    let cur = f.load_i64(acc);
                    let (m, o1) = f.checked("smul", cur, f.i64(10));
                    let (r, o2) = f.checked("ssub", m, d);
                    f.if_(f.or(o1, o2), bad);
                    f.store(acc, r);
                    f.store(i, f.add(at, f.i64(1)));
                },
            );
            let v = f.load_i64(acc);
            f.if_(neg, |f| f.ret(v));
            f.if_(f.eq(v, f.i64(i64::MIN)), bad);
            f.ret(f.sub(f.i64(0), v));
        })
    }

    /// Purpose: This function returns `jade.tofloat.str(s) -> float`.
    pub fn tofloat_str_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.tofloat.str", Some(self.f64t().into()), &[self.ptrt().into()], |f| {
            let s = f.param_ptr(0);
            let (a, b) = f.ascii_trim_span(s);
            let bad =
                |f: &F<'_, 'ctx>| f.fail_quoted("float(): cannot convert \"", s, "\" to float");
            f.if_(f.sge(a, b), bad);
            let part = f
                .call_ptr(f.cx.str_from_fn(), &[f.at(f.str_data(s), a).into(), f.sub(b, a).into()]);
            let end = f.alloca(f.cx.ptrt());
            let v = f.callv(f.cx.libc("strtod"), &[f.str_data(part).into(), end.into()]);
            let stop = f.load_ptr(end);
            let want = f.at(f.str_data(part), f.str_len(part));
            let ok = f.eq(
                f.b.build_ptr_to_int(stop, f.cx.i64t(), "").expect("positioned"),
                f.b.build_ptr_to_int(want, f.cx.i64t(), "").expect("positioned"),
            );
            f.release(&Ty::Str, part.into());
            f.if_(f.not(ok), bad);
            f.ret(v);
        })
    }

    /// Purpose: This function returns `jade.tochar.str(s) -> char`.
    pub fn tochar_str_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.tochar.str", Some(self.i32t().into()), &[self.ptrt().into()], |f| {
            let s = f.param_ptr(0);
            let len = f.str_len(s);
            let bad = |f: &F<'_, 'ctx>| {
                f.fail_quoted("char(): expected a string of exactly one character, got \"", s, "\"")
            };
            f.if_(f.eq(len, f.i64(0)), bad);
            let step = f.alloca(f.cx.i64t());
            let cp = f.call_int(f.cx.utf8_decode_fn(), &[f.str_data(s).into(), step.into()]);
            f.if_(f.ne(f.load_i64(step), len), bad);
            f.ret(cp);
        })
    }

    /// Purpose: This function returns `jade.tochar.int(n) -> char`.
    pub fn tochar_int_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.tochar.int", Some(self.i32t().into()), &[self.i64t().into()], |f| {
            let n = f.param_int(0);
            let out = f.or(f.slt(n, f.i64(0)), f.sgt(n, f.i64(0x10FFFF)));
            let surrogate = f.and(f.sge(n, f.i64(0xD800)), f.sle(n, f.i64(0xDFFF)));
            f.if_(f.or(out, surrogate), |f| {
                let text = f.int_text(n);
                let sb = f.sb_new();
                f.sb_push_lit(sb, "char(): ");
                f.sb_push_str(sb, text);
                f.sb_push_lit(sb, " is not a Unicode scalar");
                let m = f.sb_finish(sb);
                f.fail_str(m);
            });
            f.ret(f.trunc(n, f.cx.i32t()));
        })
    }

    /// Purpose: This function returns `jade.tobool.str(s) -> bool`: false for
    /// the empty string and for `false` in any case, true otherwise.
    pub fn tobool_str_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.tobool.str", Some(self.i1t().into()), &[self.ptrt().into()], |f| {
            let s = f.param_ptr(0);
            let len = f.str_len(s);
            f.if_(f.eq(len, f.i64(0)), |f| f.ret(f.bool(false)));
            f.if_(f.ne(len, f.i64(5)), |f| f.ret(f.bool(true)));
            let data = f.str_data(s);
            for (k, c) in "false".bytes().enumerate() {
                let b = f.load_i8(f.at_c(data, k as i64));
                // ASCII lowercase: set bit 5 on letters.
                let lower = f.or(b, f.i8(0x20));
                f.if_(f.ne(lower, f.i8(c as i64)), |f| f.ret(f.bool(true)));
            }
            f.ret(f.bool(false));
        })
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function writes a string to a file descriptor.
    pub fn write_str(&self, fd: i64, s: PointerValue<'ctx>) {
        self.call(
            self.cx.write_fn(),
            &[self.i32(fd).into(), self.str_data(s).into(), self.str_len(s).into()],
        );
    }

    /// Purpose: This function formats an int as decimal text.
    pub fn int_text(&self, v: IntValue<'ctx>) -> PointerValue<'ctx> {
        let buf = self.alloca(self.cx.i8t().array_type(32));
        let n = self.call_int(
            self.cx.libc("snprintf"),
            &[buf.into(), self.i64(32).into(), self.cx.cstr("%lld").into(), v.into()],
        );
        self.call_ptr(self.cx.str_from_fn(), &[buf.into(), self.sext(n, self.cx.i64t()).into()])
    }

    /// Purpose: This function appends a value's text to a string builder.
    pub fn sb_push_value(&self, sb: PointerValue<'ctx>, t: &Ty, v: BasicValueEnum<'ctx>) {
        if *t == Ty::Str {
            self.sb_push_str(sb, v.into_pointer_value());
            return;
        }
        let s = self.call_ptr(self.cx.tostr_fn(t), &[v]);
        self.sb_push_str(sb, s);
        self.release(&Ty::Str, s.into());
    }

    /// Purpose: This function finds a string's span without ASCII whitespace
    /// at either end.
    ///
    /// Outputs: the start and end byte offsets.
    pub fn ascii_trim_span(&self, s: PointerValue<'ctx>) -> (IntValue<'ctx>, IntValue<'ctx>) {
        let data = self.str_data(s);
        let len = self.str_len(s);
        let space = |f: &F<'_, 'ctx>, at: IntValue<'ctx>| {
            let b = f.load_i8(f.at(data, at));
            let ctl = f.and(
                f.icmp(inkwell::IntPredicate::UGE, b, f.i8(9)),
                f.icmp(inkwell::IntPredicate::ULE, b, f.i8(13)),
            );
            f.or(f.eq(b, f.i8(32)), ctl)
        };
        let a = self.var(self.i64(0));
        self.while_(
            |f| {
                let i = f.load_i64(a);
                let r = f.var(f.bool(false));
                f.if_(f.slt(i, len), |f| f.store(r, space(f, i)));
                f.load(f.cx.i1t(), r).into_int_value()
            },
            |f, _| f.store(a, f.add(f.load_i64(a), f.i64(1))),
        );
        let e = self.var(len);
        self.while_(
            |f| {
                let i = f.load_i64(e);
                let r = f.var(f.bool(false));
                f.if_(f.sgt(i, f.load_i64(a)), |f| f.store(r, space(f, f.sub(i, f.i64(1)))));
                f.load(f.cx.i1t(), r).into_int_value()
            },
            |f, _| f.store(e, f.sub(f.load_i64(e), f.i64(1))),
        );
        (self.load_i64(a), self.load_i64(e))
    }

    /// Purpose: This function fails with a message that quotes a string.
    pub fn fail_quoted(&self, before: &str, s: PointerValue<'ctx>, after: &str) {
        let sb = self.sb_new();
        self.sb_push_lit(sb, before);
        self.sb_push_str(sb, s);
        self.sb_push_lit(sb, after);
        let m = self.sb_finish(sb);
        self.fail_str(m);
    }

    /// Purpose: This function runs an LLVM overflow-checking intrinsic.
    ///
    /// Inputs:
    /// - `op`: `sadd`, `ssub` or `smul`.
    /// - `a`, `b`: the operands.
    ///
    /// Outputs: the wrapped result and whether it overflowed.
    pub fn checked(
        &self,
        op: &str,
        a: IntValue<'ctx>,
        b: IntValue<'ctx>,
    ) -> (IntValue<'ctx>, IntValue<'ctx>) {
        let name = format!("llvm.{op}.with.overflow");
        let intr = inkwell::intrinsics::Intrinsic::find(&name).expect("overflow intrinsic exists");
        let decl =
            intr.get_declaration(&self.cx.module, &[self.cx.i64t().into()]).expect("declaration");
        let pair = self.callv(decl, &[a.into(), b.into()]).into_struct_value();
        let v = self.b.build_extract_value(pair, 0, "").expect("positioned").into_int_value();
        let o = self.b.build_extract_value(pair, 1, "").expect("positioned").into_int_value();
        (v, o)
    }
}
