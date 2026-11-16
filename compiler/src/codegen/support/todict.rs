//! `toDict(text)`: JSON text read into a dict, with a reader generated for
//! each dict type a program reads.
//!
//! The checker picks the dict's value type from how the program uses it, and
//! MIR's `Op::ToDict` carries the dict type here, so a program gets one
//! function per type it reads, `jade.todict.<type>(text)`. Below that sits one
//! function that reads a value of each type the dict holds, nested to any
//! depth, and one that reads an object or an array of each.
//!
//! A dict of strings takes every value. A JSON string is unescaped; anything
//! else is the exact text the input wrote, so a number, `true` or `null` is its
//! own spelling and a nested object or array is its slice of the input, which
//! `toDict` reads again. Every other value type raises on a value that does not
//! fit it, `null` included, with a message that names the fix.
//!
//! The scanner is the one the JSON package had until 2.0.7, which followed
//! serde_json: the same grammar, numbers, escapes and depth limit, and errors
//! that name the line and column. A key written twice keeps its last value.
//! Keys and values take the trust of the text they came from (see `text.rs`).

use inkwell::values::{FloatValue, FunctionValue, IntValue, PointerValue};

use crate::codegen::{Cx, F};
use crate::mir::Ty;

/// How deep objects and arrays nest before the reader refuses, as serde_json.
const PARSE_DEPTH: i64 = 128;

/// The reader's state: `{ ptr data, i64 len, i64 pos, i64 trust, i64 kind }`.
/// `kind` is where the number reader leaves what it read.
const ST_DATA: i64 = 0;
const ST_LEN: i64 = 8;
const ST_POS: i64 = 16;
const ST_TRUST: i64 = 24;
const ST_KIND: i64 = 32;

/// What `jade.todict.number` read: an int, a float, or an int too long for 64
/// bits, which it returns as the nearest float.
const NUM_INT: i64 = 0;
const NUM_FLOAT: i64 = 1;
const NUM_BIG: i64 = 2;

/// The fix a message names when a value does not fit, by the type expected.
const NULL_HINT: &str = "; read the dict as strings and check for \"null\"";
const OBJECT_HINT: &str =
    "; to read another value, put it under a key, such as {\"items\": [1, 2]}";

/// Purpose: This function names a value type the way a message does.
///
/// Inputs:
/// - `t`: the value type a reader expects.
///
/// Outputs: the name with its article, and the fix for a value that is not
/// one, which tells the program to read mixed values as strings.
fn expected(t: &Ty) -> (&'static str, &'static str) {
    match t {
        Ty::Int => (
            "an int",
            "; to read mixed values, use the dict as strings and convert each with toInt()",
        ),
        Ty::Float => (
            "a float",
            "; to read mixed values, use the dict as strings and convert each with toFloat()",
        ),
        Ty::Bool => (
            "a bool",
            "; to read mixed values, use the dict as strings and convert each with toBool()",
        ),
        Ty::Map(_) => (
            "an object",
            "; to read mixed values, use the dict as strings and read each nested object with toDict()",
        ),
        _ => ("an array", ""),
    }
}

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns `jade.todict.<T>(text) -> T`, which reads
    /// JSON text holding an object into the dict type `T`.
    ///
    /// Inputs:
    /// - `t`: the dict type, `{str: V}`.
    ///
    /// Outputs: the function. Its result is owned.
    pub fn todict_fn(&self, t: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.todict.{}", crate::codegen::memory::rc::key(t));
        let p = self.ptrt();
        let t = t.clone();
        self.support(&name, Some(p.into()), &[p.into()], |f| {
            let s = f.param_ptr(0);
            let i = f.cx.i64t();
            let st = f.alloca(
                f.cx.ctx.struct_type(&[p.into(), i.into(), i.into(), i.into(), i.into()], false),
            );
            f.store(f.at_c(st, ST_DATA), f.str_data(s));
            f.store(f.at_c(st, ST_LEN), f.str_len(s));
            f.store(f.at_c(st, ST_POS), f.i64(0));
            f.store(f.at_c(st, ST_TRUST), f.str_trust(s));
            f.store(f.at_c(st, ST_KIND), f.i64(NUM_INT));
            f.todict_skip_ws(st);
            f.if_(f.todict_eof(st), |f| {
                f.todict_fail(st, "the text must hold a JSON object, found nothing", true, "")
            });
            f.if_(f.not(f.todict_at(st, b'{')), |f| {
                let found = f.call_ptr(f.cx.todict_found_fn(), &[st.into()]);
                f.if_(f.is_null(found), |f| f.todict_fail(st, "expected value", true, ""));
                f.todict_fail_found(
                    st,
                    "the text must hold a JSON object, found ",
                    found,
                    OBJECT_HINT,
                );
            });
            let vt = match &t {
                Ty::Map(v) => (**v).clone(),
                _ => Ty::Str,
            };
            let d = f.call_ptr(f.cx.todict_object_fn(&vt), &[st.into(), f.i64(0).into()]);
            f.if_(f.err_pending(), |f| f.ret(f.null()));
            f.todict_skip_ws(st);
            f.if_(f.not(f.todict_eof(st)), |f| {
                f.release(&t, d.into());
                f.todict_fail(st, "trailing characters", true, "");
            });
            f.ret(d);
        })
    }

    /// Purpose: This function returns `jade.todict.read.<V>(st, depth) -> V`:
    /// one value of type `V`, starting after any whitespace.
    fn todict_read_fn(&self, t: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.todict.read.{}", crate::codegen::memory::rc::key(t));
        let (p, i) = (self.ptrt(), self.i64t());
        let t = t.clone();
        self.support(&name, Some(self.llty(&t)), &[p.into(), i.into()], |f| {
            let (st, depth) = (f.param_ptr(0), f.param_int(1));
            f.todict_skip_ws(st);
            let (data, len, start) = f.todict_state(st);
            f.if_(f.sge(start, len), |f| f.todict_fail(st, "EOF while parsing a value", true, ""));
            let c = f.load_i8(f.at(data, start));
            let ch = |b: u8| f.eq(c, f.i8(b as i64));
            let digit = f.and(f.sge(c, f.i8(b'0' as i64)), f.sle(c, f.i8(b'9' as i64)));
            let number = f.or(ch(b'-'), digit);
            if t == Ty::Str {
                // A string is unescaped; anything else is the text it was written as.
                f.if_(ch(b'"'), |f| {
                    f.todict_advance(st, 1);
                    let s = f.call_ptr(f.cx.todict_string_fn(), &[st.into()]);
                    f.if_(f.err_pending(), |f| f.ret(f.null()));
                    f.ret(s);
                });
                f.call(f.cx.todict_skip_fn(), &[st.into(), depth.into()]);
                f.if_(f.err_pending(), |f| f.ret(f.null()));
                let (_, _, end) = f.todict_state(st);
                let trust = f.load_i64(f.at_c(st, ST_TRUST));
                f.ret(f.str_from_trust(f.at(data, start), f.sub(end, start), trust));
                return;
            }
            let (want, hint) = expected(&t);
            // null fits only a dict of strings.
            f.if_(ch(b'n'), |f| f.todict_mismatch(st, depth, want, NULL_HINT));
            match &t {
                Ty::Int => {
                    f.if_(number, |f| {
                        let w = f.call_int(f.cx.todict_number_fn(), &[st.into()]);
                        f.if_(f.err_pending(), |f| f.ret_zero());
                        let kind = f.load_i64(f.at_c(st, ST_KIND));
                        f.if_(f.eq(kind, f.i64(NUM_INT)), |f| f.ret(w));
                        f.store(f.at_c(st, ST_POS), start);
                        f.if_(f.eq(kind, f.i64(NUM_BIG)), |f| {
                            f.todict_fail(
                                st,
                                "expected an int, found a number too long for 64 bits",
                                true,
                                "; read the dict as strings to keep its digits",
                            )
                        });
                        f.todict_fail(st, "expected an int, found a float", true, hint);
                    });
                }
                Ty::Float => {
                    f.if_(number, |f| {
                        let w = f.call_int(f.cx.todict_number_fn(), &[st.into()]);
                        f.if_(f.err_pending(), |f| f.ret_zero());
                        let kind = f.load_i64(f.at_c(st, ST_KIND));
                        f.if_(f.eq(kind, f.i64(NUM_INT)), |f| f.ret(f.itof(w)));
                        f.ret(f.todict_word_float(w));
                    });
                }
                Ty::Bool => {
                    f.if_(ch(b't'), |f| {
                        f.todict_ident(st, "true");
                        f.ret(f.bool(true));
                    });
                    f.if_(ch(b'f'), |f| {
                        f.todict_ident(st, "false");
                        f.ret(f.bool(false));
                    });
                }
                Ty::Map(vt) => {
                    f.if_(ch(b'{'), |f| {
                        f.ret(f.call_ptr(f.cx.todict_object_fn(vt), &[st.into(), depth.into()]))
                    });
                }
                Ty::Array(et) => {
                    f.if_(ch(b'['), |f| {
                        f.ret(f.call_ptr(f.cx.todict_array_fn(et), &[st.into(), depth.into()]))
                    });
                }
                // The checker lets `toDict` read nothing else.
                _ => {}
            }
            f.todict_mismatch(st, depth, want, hint);
        })
    }

    /// Purpose: This function returns `jade.todict.object.<V>(st, depth) ->
    /// {str: V}`, starting at the `{`. A key written twice keeps its last value.
    fn todict_object_fn(&self, vt: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.todict.object.{}", crate::codegen::memory::rc::key(vt));
        let (p, i) = (self.ptrt(), self.i64t());
        let vt = vt.clone();
        let dict_ty = Ty::Map(Box::new(vt.clone()));
        self.support(&name, Some(p.into()), &[p.into(), i.into()], |f| {
            let (st, depth) = (f.param_ptr(0), f.param_int(1));
            f.if_(f.sge(depth, f.i64(PARSE_DEPTH)), |f| {
                f.todict_fail(st, "recursion limit exceeded", false, "")
            });
            f.todict_advance(st, 1);
            let vsize = f.size_of(f.cx.llty(&vt));
            let d = f.call_ptr(f.cx.dict_new_fn(), &[f.i64(4).into(), vsize.into()]);
            let fail = |f: &F<'_, 'ctx>, msg: &str| {
                f.release(&dict_ty, d.into());
                f.todict_fail(st, msg, true, "");
            };
            f.todict_skip_ws(st);
            f.if_(f.todict_at(st, b'}'), |f| {
                f.todict_advance(st, 1);
                f.ret(d);
            });
            f.while_(
                |f| f.bool(true),
                |f, _| {
                    f.todict_skip_ws(st);
                    f.if_(f.todict_eof(st), |f| fail(f, "EOF while parsing an object"));
                    f.if_(f.not(f.todict_at(st, b'"')), |f| fail(f, "key must be a string"));
                    f.todict_advance(st, 1);
                    let k = f.call_ptr(f.cx.todict_string_fn(), &[st.into()]);
                    f.if_(f.err_pending(), |f| {
                        f.release(&dict_ty, d.into());
                        f.ret(f.null());
                    });
                    f.todict_skip_ws(st);
                    f.if_(f.not(f.todict_at(st, b':')), |f| {
                        f.release(&Ty::Str, k.into());
                        f.if_(f.todict_eof(st), |f| fail(f, "EOF while parsing an object"));
                        fail(f, "expected `:`");
                    });
                    f.todict_advance(st, 1);
                    let v = f.callv(
                        f.cx.todict_read_fn(&vt),
                        &[st.into(), f.add(depth, f.i64(1)).into()],
                    );
                    f.if_(f.err_pending(), |f| {
                        f.release(&Ty::Str, k.into());
                        f.release(&dict_ty, d.into());
                        f.ret(f.null());
                    });
                    // `set` takes the key and the value, and releases an old one.
                    f.call(f.cx.dict_fn("set", &vt), &[d.into(), k.into(), v]);
                    f.todict_skip_ws(st);
                    f.if_(f.todict_eof(st), |f| fail(f, "EOF while parsing an object"));
                    f.if_(f.todict_at(st, b'}'), |f| {
                        f.todict_advance(st, 1);
                        f.ret(d);
                    });
                    f.if_(f.not(f.todict_at(st, b',')), |f| fail(f, "expected `,` or `}`"));
                    f.todict_advance(st, 1);
                    f.todict_skip_ws(st);
                    f.if_(f.todict_at(st, b'}'), |f| fail(f, "trailing comma"));
                },
            );
            f.ret(f.null());
        })
    }

    /// Purpose: This function returns `jade.todict.array.<E>(st, depth) ->
    /// [E]`, starting at the `[`.
    fn todict_array_fn(&self, et: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.todict.array.{}", crate::codegen::memory::rc::key(et));
        let (p, i) = (self.ptrt(), self.i64t());
        let et = et.clone();
        let array_ty = Ty::Array(Box::new(et.clone()));
        self.support(&name, Some(p.into()), &[p.into(), i.into()], |f| {
            let (st, depth) = (f.param_ptr(0), f.param_int(1));
            f.if_(f.sge(depth, f.i64(PARSE_DEPTH)), |f| {
                f.todict_fail(st, "recursion limit exceeded", false, "")
            });
            f.todict_advance(st, 1);
            let arr = f.array_new(&et, f.i64(4));
            let fail = |f: &F<'_, 'ctx>, msg: &str| {
                f.release(&array_ty, arr.into());
                f.todict_fail(st, msg, true, "");
            };
            f.todict_skip_ws(st);
            f.if_(f.todict_at(st, b']'), |f| {
                f.todict_advance(st, 1);
                f.ret(arr);
            });
            f.while_(
                |f| f.bool(true),
                |f, _| {
                    let v = f.callv(
                        f.cx.todict_read_fn(&et),
                        &[st.into(), f.add(depth, f.i64(1)).into()],
                    );
                    f.if_(f.err_pending(), |f| {
                        f.release(&array_ty, arr.into());
                        f.ret(f.null());
                    });
                    f.array_push_owned(&et, arr, v);
                    f.todict_skip_ws(st);
                    f.if_(f.todict_eof(st), |f| fail(f, "EOF while parsing a list"));
                    f.if_(f.todict_at(st, b']'), |f| {
                        f.todict_advance(st, 1);
                        f.ret(arr);
                    });
                    f.if_(f.not(f.todict_at(st, b',')), |f| fail(f, "expected `,` or `]`"));
                    f.todict_advance(st, 1);
                    f.todict_skip_ws(st);
                    f.if_(f.todict_at(st, b']'), |f| fail(f, "trailing comma"));
                },
            );
            f.ret(f.null());
        })
    }

    /// Purpose: This function returns `jade.todict.skip(st, depth)`, which
    /// reads past one value of any kind, checking its grammar and building
    /// nothing. A dict of strings keeps the text it read past.
    fn todict_skip_fn(&self) -> FunctionValue<'ctx> {
        let (p, i) = (self.ptrt(), self.i64t());
        self.support("jade.todict.skip", None, &[p.into(), i.into()], |f| {
            let (st, depth) = (f.param_ptr(0), f.param_int(1));
            f.todict_skip_ws(st);
            let (data, len, pos) = f.todict_state(st);
            f.if_(f.sge(pos, len), |f| f.todict_fail(st, "EOF while parsing a value", true, ""));
            let c = f.load_i8(f.at(data, pos));
            let ch = |b: u8| f.eq(c, f.i8(b as i64));
            for (b, word) in [(b'n', "null"), (b't', "true"), (b'f', "false")] {
                f.if_(ch(b), |f| {
                    f.todict_ident(st, word);
                    f.ret_void();
                });
            }
            f.if_(ch(b'"'), |f| {
                f.todict_advance(st, 1);
                let s = f.call_ptr(f.cx.todict_string_fn(), &[st.into()]);
                f.if_(f.err_pending(), |f| f.ret_void());
                f.release(&Ty::Str, s.into());
                f.ret_void();
            });
            let digit = f.and(f.sge(c, f.i8(b'0' as i64)), f.sle(c, f.i8(b'9' as i64)));
            f.if_(f.or(ch(b'-'), digit), |f| {
                f.call(f.cx.todict_scan_number_fn(), &[st.into()]);
                f.ret_void();
            });
            f.if_(f.or(ch(b'['), ch(b'{')), |f| {
                f.if_(f.sge(depth, f.i64(PARSE_DEPTH)), |f| {
                    f.todict_fail(st, "recursion limit exceeded", false, "")
                });
            });
            let next = f.add(depth, f.i64(1));
            f.if_(ch(b'['), |f| {
                f.todict_advance(st, 1);
                f.todict_skip_ws(st);
                f.if_(f.todict_at(st, b']'), |f| {
                    f.todict_advance(st, 1);
                    f.ret_void();
                });
                f.while_(
                    |f| f.bool(true),
                    |f, _| {
                        f.call(f.cx.todict_skip_fn(), &[st.into(), next.into()]);
                        f.if_(f.err_pending(), |f| f.ret_void());
                        f.todict_skip_ws(st);
                        f.if_(f.todict_eof(st), |f| {
                            f.todict_fail(st, "EOF while parsing a list", true, "")
                        });
                        f.if_(f.todict_at(st, b']'), |f| {
                            f.todict_advance(st, 1);
                            f.ret_void();
                        });
                        f.if_(f.not(f.todict_at(st, b',')), |f| {
                            f.todict_fail(st, "expected `,` or `]`", true, "")
                        });
                        f.todict_advance(st, 1);
                        f.todict_skip_ws(st);
                        f.if_(f.todict_at(st, b']'), |f| {
                            f.todict_fail(st, "trailing comma", true, "")
                        });
                    },
                );
                f.ret_void();
            });
            f.if_(ch(b'{'), |f| {
                f.todict_advance(st, 1);
                f.todict_skip_ws(st);
                f.if_(f.todict_at(st, b'}'), |f| {
                    f.todict_advance(st, 1);
                    f.ret_void();
                });
                f.while_(
                    |f| f.bool(true),
                    |f, _| {
                        f.todict_skip_ws(st);
                        let eof = "EOF while parsing an object";
                        f.if_(f.todict_eof(st), |f| f.todict_fail(st, eof, true, ""));
                        f.if_(f.not(f.todict_at(st, b'"')), |f| {
                            f.todict_fail(st, "key must be a string", true, "")
                        });
                        f.todict_advance(st, 1);
                        let k = f.call_ptr(f.cx.todict_string_fn(), &[st.into()]);
                        f.if_(f.err_pending(), |f| f.ret_void());
                        f.release(&Ty::Str, k.into());
                        f.todict_skip_ws(st);
                        f.if_(f.not(f.todict_at(st, b':')), |f| {
                            f.if_(f.todict_eof(st), |f| f.todict_fail(st, eof, true, ""));
                            f.todict_fail(st, "expected `:`", true, "");
                        });
                        f.todict_advance(st, 1);
                        f.call(f.cx.todict_skip_fn(), &[st.into(), next.into()]);
                        f.if_(f.err_pending(), |f| f.ret_void());
                        f.todict_skip_ws(st);
                        f.if_(f.todict_eof(st), |f| f.todict_fail(st, eof, true, ""));
                        f.if_(f.todict_at(st, b'}'), |f| {
                            f.todict_advance(st, 1);
                            f.ret_void();
                        });
                        f.if_(f.not(f.todict_at(st, b',')), |f| {
                            f.todict_fail(st, "expected `,` or `}`", true, "")
                        });
                        f.todict_advance(st, 1);
                        f.todict_skip_ws(st);
                        f.if_(f.todict_at(st, b'}'), |f| {
                            f.todict_fail(st, "trailing comma", true, "")
                        });
                    },
                );
                f.ret_void();
            });
            f.todict_fail(st, "expected value", true, "");
        })
    }

    /// Purpose: This function returns `jade.todict.found(st) -> str`, naming
    /// the kind of value that starts at the reader's position for a message:
    /// `a string`, `null`, `nothing` at the end, and so on, or a null pointer
    /// when no JSON value starts there.
    fn todict_found_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.todict.found", Some(p.into()), &[p.into()], |f| {
            let st = f.param_ptr(0);
            let (data, len, pos) = f.todict_state(st);
            f.if_(f.sge(pos, len), |f| f.ret(f.cx.lit("nothing")));
            let c = f.load_i8(f.at(data, pos));
            let ch = |b: u8| f.eq(c, f.i8(b as i64));
            let digit = f.and(f.sge(c, f.i8(b'0' as i64)), f.sle(c, f.i8(b'9' as i64)));
            f.if_(f.or(ch(b'-'), digit), |f| f.ret(f.cx.lit("a number")));
            for (b, kind) in [
                (b'"', "a string"),
                (b'{', "an object"),
                (b'[', "an array"),
                (b't', "a bool"),
                (b'f', "a bool"),
                (b'n', "null"),
            ] {
                f.if_(ch(b), |f| f.ret(f.cx.lit(kind)));
            }
            f.ret(f.null());
        })
    }

    /// Purpose: This function returns `jade.todict.fail(st, msg, peek, hint)`,
    /// which raises `toDict: <msg> at line L column C<hint>`. The position is
    /// after what the reader consumed, or with `peek` one byte further, which
    /// is the column of the byte at the reader's position.
    fn todict_fail_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        let params = [p.into(), p.into(), self.i1t().into(), p.into()];
        self.support("jade.todict.fail", None, &params, |f| {
            let (st, msg, peek, hint) =
                (f.param_ptr(0), f.param_ptr(1), f.param_int(2), f.param_ptr(3));
            let (data, len, pos) = f.todict_state(st);
            let next = f.add(pos, f.i64(1));
            let next = f.select_int(f.sgt(next, len), len, next);
            let at = f.select_int(peek, next, pos);
            let line = f.var(f.i64(1));
            let col = f.var(f.i64(0));
            f.for_range(f.i64(0), at, |f, k, _| {
                let nl = f.eq(f.load_i8(f.at(data, k)), f.i8(10));
                f.if_else(
                    nl,
                    |f| {
                        f.store(line, f.add(f.load_i64(line), f.i64(1)));
                        f.store(col, f.i64(0));
                    },
                    |f| f.store(col, f.add(f.load_i64(col), f.i64(1))),
                );
            });
            let tail = f.format(" at line %lld column %lld", &[f.load_i64(line), f.load_i64(col)]);
            let m = f.str_cat(&[f.cx.lit("toDict: "), msg, tail, hint]);
            f.release(&Ty::Str, tail.into());
            f.call(f.cx.raise_msg_fn(), &[m.into()]);
            f.release(&Ty::Str, m.into());
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.todict.string(st) -> str`, starting
    /// after the opening quote and ending after the closing one, with the
    /// text's trust.
    fn todict_string_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.todict.string", Some(p.into()), &[p.into()], |f| {
            let st = f.param_ptr(0);
            let sb = f.sb_new();
            f.sb_taint(sb, f.load_i64(f.at_c(st, ST_TRUST)));
            let fail = |f: &F<'_, 'ctx>, msg: &str| {
                f.call(f.cx.libc("free"), &[f.load_ptr(sb).into()]);
                f.todict_fail(st, msg, false, "");
            };
            f.while_(
                |f| f.bool(true),
                |f, _| {
                    let (data, len, pos) = f.todict_state(st);
                    f.if_(f.sge(pos, len), |f| fail(f, "EOF while parsing a string"));
                    let c = f.load_i8(f.at(data, pos));
                    f.todict_advance(st, 1);
                    f.if_(f.eq(c, f.i8(b'"' as i64)), |f| f.ret(f.sb_finish(sb)));
                    let ctl = f.icmp(inkwell::IntPredicate::ULT, c, f.i8(0x20));
                    f.if_(ctl, |f| {
                        fail(f, "control character (\\u0000-\\u001F) found while parsing a string")
                    });
                    f.if_else(
                        f.eq(c, f.i8(b'\\' as i64)),
                        |f| {
                            let (_, len, pos) = f.todict_state(st);
                            f.if_(f.sge(pos, len), |f| fail(f, "EOF while parsing a string"));
                            let e = f.load_i8(f.at(data, pos));
                            f.todict_advance(st, 1);
                            let done = f.block();
                            let unicode = f.block();
                            let bad = f.block();
                            let mut cases = vec![(f.i8(b'u' as i64), unicode)];
                            let mut simple = Vec::new();
                            for (from, to) in [
                                (b'"', b'"'),
                                (b'\\', b'\\'),
                                (b'/', b'/'),
                                (b'b', 8u8),
                                (b'f', 12),
                                (b'n', b'\n'),
                                (b'r', b'\r'),
                                (b't', b'\t'),
                            ] {
                                let bb = f.block();
                                cases.push((f.i8(from as i64), bb));
                                simple.push((bb, to));
                            }
                            f.switch(e, bad, &cases);
                            for (bb, to) in simple {
                                f.position(bb);
                                f.sb_push_cp(sb, f.i32(to as i64));
                                f.br(done);
                            }
                            f.position(bad);
                            fail(f, "invalid escape");
                            f.position(unicode);
                            let cp = f.todict_hex4(st, sb);
                            let lead = f.and(f.sge(cp, f.i64(0xd800)), f.sle(cp, f.i64(0xdbff)));
                            let trail = f.and(f.sge(cp, f.i64(0xdc00)), f.sle(cp, f.i64(0xdfff)));
                            f.if_(trail, |f| fail(f, "invalid unicode code point"));
                            let out = f.var(cp);
                            f.if_(lead, |f| {
                                let (data, len, pos) = f.todict_state(st);
                                let room = f.sle(f.add(pos, f.i64(2)), len);
                                let more = f.var(f.bool(false));
                                f.if_(room, |f| {
                                    let a = f.eq(f.load_i8(f.at(data, pos)), f.i8(b'\\' as i64));
                                    let b = f.eq(
                                        f.load_i8(f.at(data, f.add(pos, f.i64(1)))),
                                        f.i8(b'u' as i64),
                                    );
                                    f.store(more, f.and(a, b));
                                });
                                let more = f.load(f.cx.i1t(), more).into_int_value();
                                f.if_(f.not(more), |f| {
                                    fail(f, "lone leading surrogate in hex escape")
                                });
                                f.todict_advance(st, 2);
                                let lo = f.todict_hex4(st, sb);
                                let ok = f.and(f.sge(lo, f.i64(0xdc00)), f.sle(lo, f.i64(0xdfff)));
                                f.if_(f.not(ok), |f| {
                                    fail(f, "lone leading surrogate in hex escape")
                                });
                                let hi_bits = f.shl(f.sub(cp, f.i64(0xd800)), f.i64(10));
                                let v =
                                    f.add(f.add(hi_bits, f.sub(lo, f.i64(0xdc00))), f.i64(0x10000));
                                f.store(out, v);
                            });
                            f.sb_push_cp(sb, f.trunc(f.load_i64(out), f.cx.i32t()));
                            f.br(done);
                            f.position(done);
                        },
                        |f| f.sb_push(sb, f.at(data, pos), f.i64(1)),
                    );
                },
            );
            f.ret(f.null());
        })
    }

    /// Purpose: This function returns `jade.todict.scan_number(st) -> bool`,
    /// which reads past a number with serde's grammar, building nothing.
    ///
    /// Outputs: whether the number has a fraction or an exponent.
    fn todict_scan_number_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.todict.scan_number", Some(self.i1t().into()), &[p.into()], |f| {
            let st = f.param_ptr(0);
            let (data, len, start) = f.todict_state(st);
            let byte = |f: &F<'_, 'ctx>, k: IntValue<'ctx>| f.load_i8(f.at(data, k));
            let is_digit = |f: &F<'_, 'ctx>| {
                let (_, len, pos) = f.todict_state(st);
                let r = f.var(f.bool(false));
                f.if_(f.slt(pos, len), |f| {
                    let c = byte(f, pos);
                    f.store(r, f.and(f.sge(c, f.i8(b'0' as i64)), f.sle(c, f.i8(b'9' as i64))));
                });
                f.load(f.cx.i1t(), r).into_int_value()
            };
            let digits = |f: &F<'_, 'ctx>| {
                f.while_(|f| is_digit(f), |f, _| f.todict_advance(st, 1));
            };
            let neg = f.eq(byte(f, start), f.i8(b'-' as i64));
            f.if_(neg, |f| f.todict_advance(st, 1));
            // The int part: `0`, or a digit 1-9 and any more digits.
            let (_, _, pos) = f.todict_state(st);
            f.if_(f.sge(pos, len), |f| f.todict_fail(st, "EOF while parsing a value", false, ""));
            let c = byte(f, pos);
            f.todict_advance(st, 1);
            let zero = f.eq(c, f.i8(b'0' as i64));
            let one_nine = f.and(f.sge(c, f.i8(b'1' as i64)), f.sle(c, f.i8(b'9' as i64)));
            f.if_(f.not(f.or(zero, one_nine)), |f| f.todict_fail(st, "invalid number", false, ""));
            f.if_(zero, |f| f.if_(is_digit(f), |f| f.todict_fail(st, "invalid number", true, "")));
            f.if_(one_nine, |f| digits(f));
            let float = f.var(f.bool(false));
            let need_digits = |f: &F<'_, 'ctx>| {
                f.if_(f.todict_eof(st), |f| {
                    f.todict_fail(st, "EOF while parsing a value", true, "")
                });
                f.if_(f.not(is_digit(f)), |f| f.todict_fail(st, "invalid number", true, ""));
                digits(f);
                f.store(float, f.bool(true));
            };
            f.if_(f.todict_at(st, b'.'), |f| {
                f.todict_advance(st, 1);
                need_digits(f);
            });
            f.if_(f.or(f.todict_at(st, b'e'), f.todict_at(st, b'E')), |f| {
                f.todict_advance(st, 1);
                f.if_(f.or(f.todict_at(st, b'+'), f.todict_at(st, b'-')), |f| {
                    f.todict_advance(st, 1)
                });
                need_digits(f);
            });
            f.ret(f.load(f.cx.i1t(), float).into_int_value());
        })
    }

    /// Purpose: This function returns `jade.todict.number(st) -> i64`, with
    /// serde's rules: an int when the text has no fraction or exponent and
    /// fits in 64 bits, and a float otherwise, returned as its bits. It leaves
    /// `NUM_INT`, `NUM_FLOAT` or `NUM_BIG` in the state's `kind`. `-0` is the
    /// int 0, since a dict of ints reads it.
    fn todict_number_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.todict.number", Some(self.i64t().into()), &[p.into()], |f| {
            let st = f.param_ptr(0);
            let (data, _, start) = f.todict_state(st);
            let byte = |f: &F<'_, 'ctx>, k: IntValue<'ctx>| f.load_i8(f.at(data, k));
            let float = f.call_int(f.cx.todict_scan_number_fn(), &[st.into()]);
            f.if_(f.err_pending(), |f| f.ret_zero());
            let (_, _, end) = f.todict_state(st);
            let kind = f.at_c(st, ST_KIND);
            let neg = f.eq(byte(f, start), f.i8(b'-' as i64));
            f.if_(f.not(float), |f| {
                // The magnitude as an unsigned 64-bit number, or overflow.
                let mag = f.var(f.i64(0));
                let over = f.var(f.bool(false));
                let from = f.select_int(neg, f.add(start, f.i64(1)), start);
                f.for_range(from, end, |f, k, _| {
                    let d = f.zext(f.sub(byte(f, k), f.i8(b'0' as i64)), f.cx.i64t());
                    let (m10, o1) = f.checked("umul", f.load_i64(mag), f.i64(10));
                    let (sum, o2) = f.checked("uadd", m10, d);
                    f.store(over, f.or(f.load(f.cx.i1t(), over).into_int_value(), f.or(o1, o2)));
                    f.store(mag, sum);
                });
                let over = f.load(f.cx.i1t(), over).into_int_value();
                let m = f.load_i64(mag);
                f.if_(f.not(over), |f| {
                    let fits_pos = f.sge(m, f.i64(0));
                    f.store(kind, f.i64(NUM_INT));
                    f.if_(f.not(neg), |f| f.if_(fits_pos, |f| f.ret(m)));
                    // Negative: up to 2^63 fits.
                    let fits_neg = f.or(fits_pos, f.eq(m, f.i64(i64::MIN)));
                    f.if_(f.and(neg, fits_neg), |f| f.ret(f.sub(f.i64(0), m)));
                    let x =
                        f.b.build_unsigned_int_to_float(m, f.cx.f64t(), "").expect("positioned");
                    let x = f.select(neg, f.fneg(x), x).into_float_value();
                    f.store(kind, f.i64(NUM_BIG));
                    f.ret(f.todict_float_word(x));
                });
            });
            // A float, or an int too long for 64 bits: let strtod round it.
            let text = f.call_ptr(
                f.cx.str_from_fn(),
                &[f.at(data, start).into(), f.sub(end, start).into()],
            );
            let x = f.callv(f.cx.libc("strtod"), &[f.str_data(text).into(), f.null().into()]);
            f.release(&Ty::Str, text.into());
            let x = x.into_float_value();
            let huge = f.fcmp(inkwell::FloatPredicate::OEQ, f.todict_fabs(x), f.f64(f64::INFINITY));
            f.if_(huge, |f| {
                f.store(f.at_c(st, ST_POS), end);
                f.todict_fail(st, "number out of range", false, "");
            });
            f.store(kind, f.select_int(float, f.i64(NUM_FLOAT), f.i64(NUM_BIG)));
            f.ret(f.todict_float_word(x));
        })
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
    fn todict_state(
        &self,
        st: PointerValue<'ctx>,
    ) -> (PointerValue<'ctx>, IntValue<'ctx>, IntValue<'ctx>) {
        (
            self.load_ptr(self.at_c(st, ST_DATA)),
            self.load_i64(self.at_c(st, ST_LEN)),
            self.load_i64(self.at_c(st, ST_POS)),
        )
    }

    fn todict_advance(&self, st: PointerValue<'ctx>, n: i64) {
        let p = self.at_c(st, ST_POS);
        self.store(p, self.add(self.load_i64(p), self.i64(n)));
    }

    fn todict_eof(&self, st: PointerValue<'ctx>) -> IntValue<'ctx> {
        let (_, len, pos) = self.todict_state(st);
        self.sge(pos, len)
    }

    /// Purpose: This function says whether the next byte is `b`.
    fn todict_at(&self, st: PointerValue<'ctx>, b: u8) -> IntValue<'ctx> {
        let (data, len, pos) = self.todict_state(st);
        let r = self.var(self.bool(false));
        self.if_(self.slt(pos, len), |f| {
            f.store(r, f.eq(f.load_i8(f.at(data, pos)), f.i8(b as i64)));
        });
        self.load(self.cx.i1t(), r).into_int_value()
    }

    fn todict_skip_ws(&self, st: PointerValue<'ctx>) {
        self.while_(
            |f| {
                let sp = f.or(f.todict_at(st, b' '), f.todict_at(st, b'\n'));
                f.or(sp, f.or(f.todict_at(st, b'\t'), f.todict_at(st, b'\r')))
            },
            |f, _| f.todict_advance(st, 1),
        );
    }

    /// Purpose: This function raises a read error from support code and
    /// returns.
    ///
    /// Inputs:
    /// - `st`: the reader's state.
    /// - `msg`: what went wrong.
    /// - `peek`: whether the position names the byte at the reader's position.
    /// - `hint`: the fix, starting with `; `, or empty.
    fn todict_fail(&self, st: PointerValue<'ctx>, msg: &str, peek: bool, hint: &str) {
        self.call(
            self.cx.todict_fail_fn(),
            &[st.into(), self.cx.lit(msg).into(), self.bool(peek).into(), self.cx.lit(hint).into()],
        );
        self.ret_zero();
    }

    /// Purpose: This function raises a read error whose message ends with
    /// what was found, from `jade.todict.found`, and returns.
    fn todict_fail_found(
        &self,
        st: PointerValue<'ctx>,
        before: &str,
        found: PointerValue<'ctx>,
        hint: &str,
    ) {
        let m = self.str_cat(&[self.cx.lit(before), found]);
        self.call(
            self.cx.todict_fail_fn(),
            &[st.into(), m.into(), self.bool(true).into(), self.cx.lit(hint).into()],
        );
        self.release(&Ty::Str, m.into());
        self.ret_zero();
    }

    /// Purpose: This function raises the error for a value that does not fit
    /// the type being read, `expected an int, found a string at …`, at the
    /// value's first byte, and returns. The value is read past first, so text
    /// that is not JSON at all, such as `tru`, raises its syntax error instead.
    ///
    /// Inputs:
    /// - `st`: the reader's state, at the value.
    /// - `depth`: how deep the value is nested.
    /// - `want`: the type expected, with its article.
    /// - `hint`: the fix, starting with `; `, or empty.
    fn todict_mismatch(
        &self,
        st: PointerValue<'ctx>,
        depth: IntValue<'ctx>,
        want: &str,
        hint: &str,
    ) {
        let start = self.load_i64(self.at_c(st, ST_POS));
        self.call(self.cx.todict_skip_fn(), &[st.into(), depth.into()]);
        self.if_(self.err_pending(), |f| f.ret_zero());
        self.store(self.at_c(st, ST_POS), start);
        let found = self.call_ptr(self.cx.todict_found_fn(), &[st.into()]);
        self.todict_fail_found(st, &format!("expected {want}, found "), found, hint);
    }

    /// Purpose: This function reads the rest of `null`, `true` or `false`.
    fn todict_ident(&self, st: PointerValue<'ctx>, word: &str) {
        self.todict_advance(st, 1);
        for b in word.bytes().skip(1) {
            self.if_(self.todict_eof(st), |f| {
                f.todict_fail(st, "EOF while parsing a value", false, "")
            });
            let ok = self.todict_at(st, b);
            self.todict_advance(st, 1);
            self.if_(self.not(ok), |f| f.todict_fail(st, "expected ident", false, ""));
        }
    }

    /// Purpose: This function reads four hex digits after `\u`.
    fn todict_hex4(&self, st: PointerValue<'ctx>, sb: PointerValue<'ctx>) -> IntValue<'ctx> {
        let v = self.var(self.i64(0));
        for _ in 0..4 {
            self.if_(self.todict_eof(st), |f| {
                f.call(f.cx.libc("free"), &[f.load_ptr(sb).into()]);
                f.todict_fail(st, "EOF while parsing a string", false, "");
            });
            let (data, _, pos) = self.todict_state(st);
            let c = self.zext(self.load_i8(self.at(data, pos)), self.cx.i64t());
            self.todict_advance(st, 1);
            let within = |lo: u8, hi: u8| {
                self.and(self.sge(c, self.i64(lo as i64)), self.sle(c, self.i64(hi as i64)))
            };
            let d = self.select_int(
                within(b'0', b'9'),
                self.sub(c, self.i64(b'0' as i64)),
                self.i64(-1),
            );
            let d = self.select_int(within(b'a', b'f'), self.sub(c, self.i64(b'a' as i64 - 10)), d);
            let d = self.select_int(within(b'A', b'F'), self.sub(c, self.i64(b'A' as i64 - 10)), d);
            self.if_(self.slt(d, self.i64(0)), |f| {
                f.call(f.cx.libc("free"), &[f.load_ptr(sb).into()]);
                f.todict_fail(st, "invalid escape", false, "");
            });
            self.store(v, self.or(self.shl(self.load_i64(v), self.i64(4)), d));
        }
        self.load_i64(v)
    }

    fn todict_float_word(&self, x: FloatValue<'ctx>) -> IntValue<'ctx> {
        self.b.build_bit_cast(x, self.cx.i64t(), "").expect("positioned").into_int_value()
    }

    fn todict_word_float(&self, w: IntValue<'ctx>) -> FloatValue<'ctx> {
        self.b.build_bit_cast(w, self.cx.f64t(), "").expect("positioned").into_float_value()
    }

    fn todict_fabs(&self, x: FloatValue<'ctx>) -> FloatValue<'ctx> {
        let intr = inkwell::intrinsics::Intrinsic::find("llvm.fabs").expect("intrinsic");
        let decl =
            intr.get_declaration(&self.cx.module, &[self.cx.f64t().into()]).expect("declaration");
        self.callv(decl, &[x.into()]).into_float_value()
    }
}
