//! `std::json`, and the `Json` value it returns.
//!
//! A `Json` value is the one built-in value that carries a tag, since a parsed
//! document can hold anything. JSON `null` is a null pointer, which is also how
//! the checker lets a `Json` variable start out as `nil`. Every other value is
//! `{ i64 rc, i64 kind, i64 payload }`: a bool or an int in the payload, a
//! float's bits, or a pointer to a string, an `[json]` array or a `{json}`
//! dict, so arrays and objects reuse the array and dict code, printing
//! included.
//!
//! `json.parse` follows serde_json, which v1 used: the same grammar, numbers
//! that fit an int become ints, and the same error messages with the same line
//! and column, under v1's `I/O error: json.parse: ` prefix. Parsed strings take
//! the trust of the text they came from.
//!
//! `json.stringify` writes any value whose type the compiler knows, with
//! serde's layout: compact, or two-space indented for `stringify_pretty`, dict
//! keys in sorted order, floats in Ryu's form. A class value is an object of
//! its fields. Unlike v1, the result keeps the taint of any string written into
//! it.

use inkwell::values::{BasicValueEnum, FloatValue, FunctionValue, IntValue, PointerValue};

use super::layout::{ARR_DATA, LEN};
use super::{Cx, F};
use crate::mir::{JsonFn, Ty};

/// The kinds a non-null `Json` value can be.
pub const KIND_BOOL: i64 = 1;
pub const KIND_INT: i64 = 2;
pub const KIND_FLOAT: i64 = 3;
pub const KIND_STR: i64 = 4;
pub const KIND_ARRAY: i64 = 5;
pub const KIND_OBJECT: i64 = 6;
/// Byte offsets in a `Json` value.
pub const KIND: i64 = 8;
pub const PAYLOAD: i64 = 16;

/// How deep `json.parse` nests before refusing, as serde_json does.
const PARSE_DEPTH: i64 = 128;
/// How deep `json.stringify` writes before refusing, as v1 did.
const WRITE_DEPTH: i64 = 64;

/// The parser's state: `{ ptr data, i64 len, i64 pos, i64 trust }`.
const ST_DATA: i64 = 0;
const ST_LEN: i64 = 8;
const ST_POS: i64 = 16;
const ST_TRUST: i64 = 24;

fn json_ty() -> Ty {
    Ty::Json
}

fn array_ty() -> Ty {
    Ty::Array(Box::new(Ty::Json))
}

fn object_ty() -> Ty {
    Ty::Map(Box::new(Ty::Json))
}

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns `jade.json.new(kind, payload) -> json`.
    /// A pointer payload's reference moves into the value.
    pub fn json_new_fn(&self) -> FunctionValue<'ctx> {
        let i = self.i64t();
        self.support("jade.json.new", Some(self.ptrt().into()), &[i.into(), i.into()], |f| {
            let j = f.call_ptr(f.cx.libc("malloc"), &[f.i64(24).into()]);
            f.store(j, f.i64(1));
            f.store(f.at_c(j, KIND), f.param_int(0));
            f.store(f.at_c(j, PAYLOAD), f.param_int(1));
            f.ret(j);
        })
    }

    /// Purpose: This function returns the support function behind a `Json`
    /// operation that takes a `Json` value.
    pub fn json_fn(&self, m: JsonFn) -> FunctionValue<'ctx> {
        let (p, i) = (self.ptrt(), self.i64t());
        let name = format!("jade.json.{m:?}").to_lowercase();
        match m {
            JsonFn::Parse => self.json_parse_fn(),
            JsonFn::Get => self.support(&name, Some(p.into()), &[p.into(), p.into()], |f| {
                let (j, k) = (f.param_ptr(0), f.param_ptr(1));
                f.json_expect(j, KIND_OBJECT, "cannot index a json %s with a str");
                let d = f.load_ptr(f.at_c(j, PAYLOAD));
                let v = f.dict_get(&json_ty(), d, k);
                f.if_(f.err_pending(), |f| f.ret_zero());
                f.retain(&json_ty(), v);
                f.ret(v);
            }),
            JsonFn::At => self.support(&name, Some(p.into()), &[p.into(), i.into()], |f| {
                let (j, k) = (f.param_ptr(0), f.param_int(1));
                f.json_expect(j, KIND_ARRAY, "cannot index a json %s with an int");
                let a = f.load_ptr(f.at_c(j, PAYLOAD));
                let v = f.callv(f.cx.array_fn("get", &json_ty()), &[a.into(), k.into()]);
                f.if_(f.err_pending(), |f| f.ret_zero());
                f.retain(&json_ty(), v);
                f.ret(v);
            }),
            JsonFn::Len => self.support(&name, Some(i.into()), &[p.into()], |f| {
                let j = f.param_ptr(0);
                f.fail_if(f.is_null(j), "len() has no answer for json null");
                let kind = f.load_i64(f.at_c(j, KIND));
                let payload = f.load_ptr(f.at_c(j, PAYLOAD));
                f.if_(f.eq(kind, f.i64(KIND_STR)), |f| {
                    f.ret(f.call_int(f.cx.str_charlen_fn(), &[payload.into()]))
                });
                let sized = f.or(f.eq(kind, f.i64(KIND_ARRAY)), f.eq(kind, f.i64(KIND_OBJECT)));
                f.if_(sized, |f| f.ret(f.load_i64(f.at_c(payload, LEN))));
                f.json_kind_fail(j, "len() has no answer for json %s");
            }),
            JsonFn::Items => self.support(&name, Some(p.into()), &[p.into()], |f| {
                let j = f.param_ptr(0);
                f.fail_if(f.is_null(j), "cannot loop over json null");
                let kind = f.load_i64(f.at_c(j, KIND));
                let payload = f.load_ptr(f.at_c(j, PAYLOAD));
                f.if_(f.eq(kind, f.i64(KIND_ARRAY)), |f| {
                    f.retain(&array_ty(), payload.into());
                    f.ret(payload);
                });
                // An object gives its keys and a string its characters, each as
                // a json string, the way a dict and a string loop.
                f.if_(f.eq(kind, f.i64(KIND_OBJECT)), |f| {
                    let keys = f.call_ptr(f.cx.dict_keys_fn(&json_ty()), &[payload.into()]);
                    let n = f.load_i64(f.at_c(keys, LEN));
                    let src = f.load_ptr(f.at_c(keys, ARR_DATA));
                    let out = f.array_new(&json_ty(), n);
                    let dst = f.load_ptr(f.at_c(out, ARR_DATA));
                    f.for_range(f.i64(0), n, |f, k, _| {
                        let s = f.load_ptr(f.elem(f.cx.ptrt(), src, k));
                        f.retain(&Ty::Str, s.into());
                        f.store(f.elem(f.cx.ptrt(), dst, k), f.json_wrap(KIND_STR, f.ptr_word(s)));
                    });
                    f.store(f.at_c(out, LEN), n);
                    f.release(&Ty::Array(Box::new(Ty::Str)), keys.into());
                    f.ret(out);
                });
                f.if_(f.eq(kind, f.i64(KIND_STR)), |f| {
                    let chars = f.call_ptr(f.cx.str_chars_fn(), &[payload.into()]);
                    let n = f.load_i64(f.at_c(chars, LEN));
                    let src = f.load_ptr(f.at_c(chars, ARR_DATA));
                    let out = f.array_new(&json_ty(), n);
                    let dst = f.load_ptr(f.at_c(out, ARR_DATA));
                    f.for_range(f.i64(0), n, |f, k, _| {
                        let cp = f.load_i32(f.elem(f.cx.i32t(), src, k));
                        let s = f.call_ptr(f.cx.char_tostr_fn(), &[cp.into()]);
                        f.store(f.elem(f.cx.ptrt(), dst, k), f.json_wrap(KIND_STR, f.ptr_word(s)));
                    });
                    f.store(f.at_c(out, LEN), n);
                    f.release(&Ty::Array(Box::new(Ty::Char)), chars.into());
                    f.ret(out);
                });
                f.json_kind_fail(j, "cannot loop over json %s");
            }),
            JsonFn::ToInt => self.support(&name, Some(i.into()), &[p.into()], |f| {
                let j = f.param_ptr(0);
                f.fail_if(f.is_null(j), "int() cannot convert json null");
                let kind = f.load_i64(f.at_c(j, KIND));
                let w = f.load_i64(f.at_c(j, PAYLOAD));
                f.if_(f.or(f.eq(kind, f.i64(KIND_INT)), f.eq(kind, f.i64(KIND_BOOL))), |f| {
                    f.ret(w)
                });
                f.if_(f.eq(kind, f.i64(KIND_FLOAT)), |f| {
                    f.ret(f.float_to_int_sat(f.word_float(w)))
                });
                f.if_(f.eq(kind, f.i64(KIND_STR)), |f| {
                    f.ret(f.call_int(f.cx.toint_str_fn(), &[f.word_ptr(w).into()]))
                });
                f.json_kind_fail(j, "int() cannot convert json %s");
            }),
            JsonFn::ToFloat => self.support(&name, Some(self.f64t().into()), &[p.into()], |f| {
                let j = f.param_ptr(0);
                f.fail_if(f.is_null(j), "float() cannot convert json null");
                let kind = f.load_i64(f.at_c(j, KIND));
                let w = f.load_i64(f.at_c(j, PAYLOAD));
                f.if_(f.eq(kind, f.i64(KIND_INT)), |f| f.ret(f.itof(w)));
                f.if_(f.eq(kind, f.i64(KIND_FLOAT)), |f| f.ret(f.word_float(w)));
                f.if_(f.eq(kind, f.i64(KIND_STR)), |f| {
                    f.ret(f.callv(f.cx.tofloat_str_fn(), &[f.word_ptr(w).into()]))
                });
                f.json_kind_fail(j, "float() cannot convert json %s");
            }),
            JsonFn::ToBool => self.support(&name, Some(self.i1t().into()), &[p.into()], |f| {
                let j = f.param_ptr(0);
                f.fail_if(f.is_null(j), "bool() cannot convert json null");
                let kind = f.load_i64(f.at_c(j, KIND));
                let w = f.load_i64(f.at_c(j, PAYLOAD));
                f.if_(f.or(f.eq(kind, f.i64(KIND_INT)), f.eq(kind, f.i64(KIND_BOOL))), |f| {
                    f.ret(f.ne(w, f.i64(0)))
                });
                f.if_(f.eq(kind, f.i64(KIND_STR)), |f| {
                    f.ret(f.call_int(f.cx.tobool_str_fn(), &[f.word_ptr(w).into()]))
                });
                f.json_kind_fail(j, "bool() cannot convert json %s");
            }),
            JsonFn::Stringify | JsonFn::StringifyPretty => {
                unreachable!("stringify is generated per type by json_stringify_fn")
            }
        }
    }

    /// Purpose: This function returns `jade.json.kind_name(j) -> str`.
    fn json_kind_name_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.json.kind_name", Some(self.ptrt().into()), &[self.ptrt().into()], |f| {
            let j = f.param_ptr(0);
            f.if_(f.is_null(j), |f| f.ret(f.cx.lit("null")));
            let kind = f.load_i64(f.at_c(j, KIND));
            for (k, n) in [
                (KIND_BOOL, "bool"),
                (KIND_INT, "int"),
                (KIND_FLOAT, "float"),
                (KIND_STR, "string"),
                (KIND_ARRAY, "array"),
            ] {
                f.if_(f.eq(kind, f.i64(k)), |f| f.ret(f.cx.lit(n)));
            }
            f.ret(f.cx.lit("object"));
        })
    }

    /// Purpose: This function returns `jade.tostr.json(j) -> str`: the text
    /// `print` shows, which is what the same value parsed into plain arrays,
    /// dicts and scalars printed under v1.
    pub fn json_tostr_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.tostr.json", Some(self.ptrt().into()), &[self.ptrt().into()], |f| {
            let j = f.param_ptr(0);
            f.if_(f.is_null(j), |f| f.ret(f.cx.lit("nil")));
            let kind = f.load_i64(f.at_c(j, KIND));
            let w = f.load_i64(f.at_c(j, PAYLOAD));
            f.if_(f.eq(kind, f.i64(KIND_BOOL)), |f| {
                let v = f.trunc(w, f.cx.i1t());
                f.ret(f.callv(f.cx.tostr_fn(&Ty::Bool), &[v.into()]));
            });
            f.if_(f.eq(kind, f.i64(KIND_INT)), |f| {
                f.ret(f.callv(f.cx.tostr_fn(&Ty::Int), &[w.into()]))
            });
            f.if_(f.eq(kind, f.i64(KIND_FLOAT)), |f| {
                f.ret(f.callv(f.cx.tostr_fn(&Ty::Float), &[f.word_float(w).into()]))
            });
            f.if_(f.eq(kind, f.i64(KIND_STR)), |f| {
                let s = f.word_ptr(w);
                f.retain(&Ty::Str, s.into());
                f.ret(s);
            });
            f.if_(f.eq(kind, f.i64(KIND_ARRAY)), |f| {
                f.ret(f.callv(f.cx.tostr_fn(&array_ty()), &[f.word_ptr(w).into()]))
            });
            f.ret(f.callv(f.cx.tostr_fn(&object_ty()), &[f.word_ptr(w).into()]));
        })
    }

    /// Purpose: This function returns `jade.json.eq(a, b) -> bool`: the same
    /// kind and the same value, all the way down.
    pub fn json_eq_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.json.eq", Some(self.i1t().into()), &[p.into(), p.into()], |f| {
            let (a, b) = (f.param_ptr(0), f.param_ptr(1));
            let (na, nb) = (f.is_null(a), f.is_null(b));
            f.if_(f.or(na, nb), |f| f.ret(f.and(na, nb)));
            let kind = f.load_i64(f.at_c(a, KIND));
            f.if_(f.ne(kind, f.load_i64(f.at_c(b, KIND))), |f| f.ret(f.bool(false)));
            let (wa, wb) = (f.load_i64(f.at_c(a, PAYLOAD)), f.load_i64(f.at_c(b, PAYLOAD)));
            f.if_(f.eq(kind, f.i64(KIND_FLOAT)), |f| {
                f.ret(f.fcmp(inkwell::FloatPredicate::OEQ, f.word_float(wa), f.word_float(wb)))
            });
            f.if_(f.eq(kind, f.i64(KIND_STR)), |f| {
                f.ret(f.eq_values(&Ty::Str, f.word_ptr(wa).into(), f.word_ptr(wb).into()))
            });
            f.if_(f.eq(kind, f.i64(KIND_ARRAY)), |f| {
                f.ret(f.eq_values(&array_ty(), f.word_ptr(wa).into(), f.word_ptr(wb).into()))
            });
            f.if_(f.eq(kind, f.i64(KIND_OBJECT)), |f| {
                f.ret(f.eq_values(&object_ty(), f.word_ptr(wa).into(), f.word_ptr(wb).into()))
            });
            f.ret(f.eq(wa, wb));
        })
    }

    // ── parsing ──────────────────────────────────────────────────────────

    /// Purpose: This function returns `jade.json.parse(s) -> json`.
    fn json_parse_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.json.parse", Some(p.into()), &[p.into()], |f| {
            let s = f.param_ptr(0);
            let i = f.cx.i64t();
            let st =
                f.alloca(f.cx.ctx.struct_type(&[p.into(), i.into(), i.into(), i.into()], false));
            f.store(f.at_c(st, ST_DATA), f.str_data(s));
            f.store(f.at_c(st, ST_LEN), f.str_len(s));
            f.store(f.at_c(st, ST_POS), f.i64(0));
            f.store(f.at_c(st, ST_TRUST), f.str_trust(s));
            let v = f.call_ptr(f.cx.json_value_fn(), &[st.into(), f.i64(0).into()]);
            f.if_(f.err_pending(), |f| f.ret(f.null()));
            f.json_skip_ws(st);
            f.if_(f.slt(f.load_i64(f.at_c(st, ST_POS)), f.load_i64(f.at_c(st, ST_LEN))), |f| {
                f.release(&json_ty(), v.into());
                f.json_fail(st, "trailing characters", true);
            });
            f.ret(v);
        })
    }

    /// Purpose: This function returns `jade.json.fail(st, msg, peek)`, which
    /// raises `I/O error: json.parse: <msg> at line L column C`. serde names
    /// the position after what it consumed, or with `peek` one byte further.
    fn json_fail_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.json.fail", None, &[p.into(), p.into(), self.i1t().into()], |f| {
            let (st, msg, peek) = (f.param_ptr(0), f.param_ptr(1), f.param_int(2));
            let (data, len, pos) = f.json_state(st);
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
            let head = f.cx.lit(&format!("{}json.parse: ", super::fs::IO_ERROR));
            let m = f.str_cat(&[head, msg, tail]);
            f.release(&Ty::Str, tail.into());
            f.call(f.cx.raise_msg_fn(), &[m.into()]);
            f.release(&Ty::Str, m.into());
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.json.value(st, depth) -> json`:
    /// one value, starting after any whitespace.
    fn json_value_fn(&self) -> FunctionValue<'ctx> {
        let (p, i) = (self.ptrt(), self.i64t());
        self.support("jade.json.value", Some(p.into()), &[p.into(), i.into()], |f| {
            let (st, depth) = (f.param_ptr(0), f.param_int(1));
            f.json_skip_ws(st);
            let (data, len, pos) = f.json_state(st);
            f.if_(f.sge(pos, len), |f| f.json_fail(st, "EOF while parsing a value", true));
            let c = f.load_i8(f.at(data, pos));
            let ch = |b: u8| f.eq(c, f.i8(b as i64));
            f.if_(ch(b'n'), |f| {
                f.json_ident(st, "null");
                f.ret(f.null());
            });
            f.if_(ch(b't'), |f| {
                f.json_ident(st, "true");
                f.ret(f.json_wrap(KIND_BOOL, f.i64(1)));
            });
            f.if_(ch(b'f'), |f| {
                f.json_ident(st, "false");
                f.ret(f.json_wrap(KIND_BOOL, f.i64(0)));
            });
            f.if_(ch(b'"'), |f| {
                f.json_advance(st, 1);
                let s = f.call_ptr(f.cx.json_string_fn(), &[st.into()]);
                f.if_(f.err_pending(), |f| f.ret(f.null()));
                f.ret(f.json_wrap(KIND_STR, f.ptr_word(s)));
            });
            let digit = f.and(f.sge(c, f.i8(b'0' as i64)), f.sle(c, f.i8(b'9' as i64)));
            f.if_(f.or(ch(b'-'), digit), |f| {
                f.ret(f.call_ptr(f.cx.json_number_fn(), &[st.into()]))
            });
            let open = f.or(ch(b'['), ch(b'{'));
            f.if_(open, |f| {
                f.if_(f.sge(depth, f.i64(PARSE_DEPTH)), |f| {
                    f.json_fail(st, "recursion limit exceeded", false)
                });
            });
            f.if_(ch(b'['), |f| {
                f.ret(f.call_ptr(f.cx.json_array_fn(), &[st.into(), depth.into()]))
            });
            f.if_(ch(b'{'), |f| {
                f.ret(f.call_ptr(f.cx.json_object_fn(), &[st.into(), depth.into()]))
            });
            f.json_fail(st, "expected value", true);
        })
    }

    /// Purpose: This function returns `jade.json.array(st, depth) -> json`,
    /// starting at the `[`.
    fn json_array_fn(&self) -> FunctionValue<'ctx> {
        let (p, i) = (self.ptrt(), self.i64t());
        self.support("jade.json.array", Some(p.into()), &[p.into(), i.into()], |f| {
            let (st, depth) = (f.param_ptr(0), f.param_int(1));
            f.json_advance(st, 1);
            let arr = f.array_new(&json_ty(), f.i64(4));
            let fail = |f: &F<'_, 'ctx>, msg: &str| {
                f.release(&array_ty(), arr.into());
                f.json_fail(st, msg, true);
            };
            f.json_skip_ws(st);
            f.if_(f.json_at(st, b']'), |f| {
                f.json_advance(st, 1);
                f.ret(f.json_wrap(KIND_ARRAY, f.ptr_word(arr)));
            });
            f.while_(
                |f| f.bool(true),
                |f, _| {
                    let v = f.call_ptr(
                        f.cx.json_value_fn(),
                        &[st.into(), f.add(depth, f.i64(1)).into()],
                    );
                    f.if_(f.err_pending(), |f| {
                        f.release(&array_ty(), arr.into());
                        f.ret(f.null());
                    });
                    f.array_push_owned(&json_ty(), arr, v.into());
                    f.json_skip_ws(st);
                    f.if_(f.json_eof(st), |f| fail(f, "EOF while parsing a list"));
                    f.if_(f.json_at(st, b']'), |f| {
                        f.json_advance(st, 1);
                        f.ret(f.json_wrap(KIND_ARRAY, f.ptr_word(arr)));
                    });
                    f.if_(f.not(f.json_at(st, b',')), |f| fail(f, "expected `,` or `]`"));
                    f.json_advance(st, 1);
                    f.json_skip_ws(st);
                    f.if_(f.json_at(st, b']'), |f| fail(f, "trailing comma"));
                },
            );
            f.ret(f.null());
        })
    }

    /// Purpose: This function returns `jade.json.object(st, depth) -> json`,
    /// starting at the `{`. A repeated key keeps its last value.
    fn json_object_fn(&self) -> FunctionValue<'ctx> {
        let (p, i) = (self.ptrt(), self.i64t());
        self.support("jade.json.object", Some(p.into()), &[p.into(), i.into()], |f| {
            let (st, depth) = (f.param_ptr(0), f.param_int(1));
            f.json_advance(st, 1);
            let vsize = f.size_of(f.cx.ptrt().into());
            let d = f.call_ptr(f.cx.dict_new_fn(), &[f.i64(4).into(), vsize.into()]);
            let fail = |f: &F<'_, 'ctx>, msg: &str| {
                f.release(&object_ty(), d.into());
                f.json_fail(st, msg, true);
            };
            f.json_skip_ws(st);
            f.if_(f.json_at(st, b'}'), |f| {
                f.json_advance(st, 1);
                f.ret(f.json_wrap(KIND_OBJECT, f.ptr_word(d)));
            });
            f.while_(
                |f| f.bool(true),
                |f, _| {
                    f.json_skip_ws(st);
                    f.if_(f.json_eof(st), |f| fail(f, "EOF while parsing an object"));
                    f.if_(f.not(f.json_at(st, b'"')), |f| fail(f, "key must be a string"));
                    f.json_advance(st, 1);
                    let k = f.call_ptr(f.cx.json_string_fn(), &[st.into()]);
                    f.if_(f.err_pending(), |f| {
                        f.release(&object_ty(), d.into());
                        f.ret(f.null());
                    });
                    f.json_skip_ws(st);
                    let colon = f.json_at(st, b':');
                    f.if_(f.not(colon), |f| {
                        f.release(&Ty::Str, k.into());
                        f.if_(f.json_eof(st), |f| fail(f, "EOF while parsing an object"));
                        fail(f, "expected `:`");
                    });
                    f.json_advance(st, 1);
                    let v = f.call_ptr(
                        f.cx.json_value_fn(),
                        &[st.into(), f.add(depth, f.i64(1)).into()],
                    );
                    f.if_(f.err_pending(), |f| {
                        f.release(&Ty::Str, k.into());
                        f.release(&object_ty(), d.into());
                        f.ret(f.null());
                    });
                    f.call(f.cx.dict_fn("set", &json_ty()), &[d.into(), k.into(), v.into()]);
                    f.json_skip_ws(st);
                    f.if_(f.json_eof(st), |f| fail(f, "EOF while parsing an object"));
                    f.if_(f.json_at(st, b'}'), |f| {
                        f.json_advance(st, 1);
                        f.ret(f.json_wrap(KIND_OBJECT, f.ptr_word(d)));
                    });
                    f.if_(f.not(f.json_at(st, b',')), |f| fail(f, "expected `,` or `}`"));
                    f.json_advance(st, 1);
                    f.json_skip_ws(st);
                    f.if_(f.json_at(st, b'}'), |f| fail(f, "trailing comma"));
                },
            );
            f.ret(f.null());
        })
    }

    /// Purpose: This function returns `jade.json.string(st) -> str`, starting
    /// after the opening quote and ending after the closing one.
    fn json_string_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.json.string", Some(p.into()), &[p.into()], |f| {
            let st = f.param_ptr(0);
            let sb = f.sb_new();
            f.sb_taint(sb, f.load_i64(f.at_c(st, ST_TRUST)));
            let fail = |f: &F<'_, 'ctx>, msg: &str| {
                f.call(f.cx.libc("free"), &[f.load_ptr(sb).into()]);
                f.json_fail(st, msg, false);
            };
            f.while_(
                |f| f.bool(true),
                |f, _| {
                    let (data, len, pos) = f.json_state(st);
                    f.if_(f.sge(pos, len), |f| fail(f, "EOF while parsing a string"));
                    let c = f.load_i8(f.at(data, pos));
                    f.json_advance(st, 1);
                    f.if_(f.eq(c, f.i8(b'"' as i64)), |f| f.ret(f.sb_finish(sb)));
                    let ctl = f.icmp(inkwell::IntPredicate::ULT, c, f.i8(0x20));
                    f.if_(ctl, |f| {
                        fail(f, "control character (\\u0000-\\u001F) found while parsing a string")
                    });
                    f.if_else(
                        f.eq(c, f.i8(b'\\' as i64)),
                        |f| {
                            let (_, len, pos) = f.json_state(st);
                            f.if_(f.sge(pos, len), |f| fail(f, "EOF while parsing a string"));
                            let e = f.load_i8(f.at(data, pos));
                            f.json_advance(st, 1);
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
                            let cp = f.json_hex4(st, sb);
                            let lead = f.and(f.sge(cp, f.i64(0xd800)), f.sle(cp, f.i64(0xdbff)));
                            let trail = f.and(f.sge(cp, f.i64(0xdc00)), f.sle(cp, f.i64(0xdfff)));
                            f.if_(trail, |f| fail(f, "invalid unicode code point"));
                            let out = f.var(cp);
                            f.if_(lead, |f| {
                                let (data, len, pos) = f.json_state(st);
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
                                f.json_advance(st, 2);
                                let lo = f.json_hex4(st, sb);
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

    /// Purpose: This function returns `jade.json.number(st) -> json`, with
    /// serde's rules: an int when the text has no fraction or exponent and
    /// fits, `-0` as the float -0.0, and a float otherwise.
    fn json_number_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.json.number", Some(p.into()), &[p.into()], |f| {
            let st = f.param_ptr(0);
            let (data, len, start) = f.json_state(st);
            let byte = |f: &F<'_, 'ctx>, k: IntValue<'ctx>| f.load_i8(f.at(data, k));
            let is_digit = |f: &F<'_, 'ctx>| {
                let (_, len, pos) = f.json_state(st);
                let r = f.var(f.bool(false));
                f.if_(f.slt(pos, len), |f| {
                    let c = byte(f, pos);
                    f.store(r, f.and(f.sge(c, f.i8(b'0' as i64)), f.sle(c, f.i8(b'9' as i64))));
                });
                f.load(f.cx.i1t(), r).into_int_value()
            };
            let digits = |f: &F<'_, 'ctx>| {
                f.while_(|f| is_digit(f), |f, _| f.json_advance(st, 1));
            };
            let neg = f.eq(byte(f, start), f.i8(b'-' as i64));
            f.if_(neg, |f| f.json_advance(st, 1));
            // The int part: `0`, or a digit 1-9 and any more digits.
            let (_, _, pos) = f.json_state(st);
            f.if_(f.sge(pos, len), |f| f.json_fail(st, "EOF while parsing a value", false));
            let c = byte(f, pos);
            f.json_advance(st, 1);
            let zero = f.eq(c, f.i8(b'0' as i64));
            let one_nine = f.and(f.sge(c, f.i8(b'1' as i64)), f.sle(c, f.i8(b'9' as i64)));
            f.if_(f.not(f.or(zero, one_nine)), |f| f.json_fail(st, "invalid number", false));
            f.if_(zero, |f| f.if_(is_digit(f), |f| f.json_fail(st, "invalid number", true)));
            f.if_(one_nine, |f| digits(f));
            let float = f.var(f.bool(false));
            let need_digits = |f: &F<'_, 'ctx>| {
                f.if_(f.json_eof(st), |f| f.json_fail(st, "EOF while parsing a value", true));
                f.if_(f.not(is_digit(f)), |f| f.json_fail(st, "invalid number", true));
                digits(f);
                f.store(float, f.bool(true));
            };
            f.if_(f.json_at(st, b'.'), |f| {
                f.json_advance(st, 1);
                need_digits(f);
            });
            f.if_(f.or(f.json_at(st, b'e'), f.json_at(st, b'E')), |f| {
                f.json_advance(st, 1);
                f.if_(f.or(f.json_at(st, b'+'), f.json_at(st, b'-')), |f| f.json_advance(st, 1));
                need_digits(f);
            });
            let (_, _, end) = f.json_state(st);
            let float = f.load(f.cx.i1t(), float).into_int_value();
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
                    f.if_(f.and(f.not(neg), fits_pos), |f| f.ret(f.json_wrap(KIND_INT, m)));
                    // Negative: -0 is a float, and up to 2^63 fits.
                    let is_zero = f.eq(m, f.i64(0));
                    f.if_(f.and(neg, is_zero), |f| {
                        f.ret(f.json_wrap(KIND_FLOAT, f.float_word(f.f64(-0.0))))
                    });
                    let fits_neg = f.or(fits_pos, f.eq(m, f.i64(i64::MIN)));
                    f.if_(f.and(neg, fits_neg), |f| {
                        f.ret(f.json_wrap(KIND_INT, f.sub(f.i64(0), m)))
                    });
                    let x =
                        f.b.build_unsigned_int_to_float(m, f.cx.f64t(), "").expect("positioned");
                    let x = f.select(neg, f.fneg(x), x).into_float_value();
                    f.ret(f.json_wrap(KIND_FLOAT, f.float_word(x)));
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
            let huge = f.fcmp(
                inkwell::FloatPredicate::OEQ,
                f.float_intrinsic_fabs(x),
                f.f64(f64::INFINITY),
            );
            f.if_(huge, |f| f.json_fail(st, "number out of range", false));
            f.ret(f.json_wrap(KIND_FLOAT, f.float_word(x)));
        })
    }

    // ── writing ──────────────────────────────────────────────────────────

    /// Purpose: This function returns `jade.json.stringify.<T>(v, pretty) ->
    /// str`.
    pub fn json_stringify_fn(&self, t: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.json.stringify.{}", super::rc::key(t));
        let t = t.clone();
        let arg = self.llty(&t);
        self.support(&name, Some(self.ptrt().into()), &[arg, self.i1t().into()], |f| {
            let (v, pretty) = (f.param(0), f.param_int(1));
            let sb = f.sb_new();
            f.call(f.cx.json_write_fn(&t), &[sb.into(), v, f.i64(0).into(), pretty.into()]);
            f.if_(f.err_pending(), |f| {
                f.call(f.cx.libc("free"), &[f.load_ptr(sb).into()]);
                f.ret(f.null());
            });
            f.ret(f.sb_finish(sb));
        })
    }

    /// Purpose: This function returns `jade.json.write.<T>(sb, v, depth,
    /// pretty)`, which appends a value's JSON to a string builder.
    fn json_write_fn(&self, t: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.json.write.{}", super::rc::key(t));
        let t = t.clone();
        let (p, i) = (self.ptrt(), self.i64t());
        let params = [p.into(), self.llty(&t), i.into(), self.i1t().into()];
        self.support(&name, None, &params, |f| {
            let (sb, v, depth, pretty) =
                (f.param_ptr(0), f.param(1), f.param_int(2), f.param_int(3));
            f.if_(f.sgt(depth, f.i64(WRITE_DEPTH)), |f| {
                f.fail(&format!(
                    "{}json.stringify: value nests deeper than {WRITE_DEPTH} levels (a value that \
                     contains itself cannot be represented as JSON)",
                    super::fs::IO_ERROR
                ));
            });
            let next = f.add(depth, f.i64(1));
            match &t {
                Ty::Int => {
                    let s = f.int_text(v.into_int_value());
                    f.sb_push_str(sb, s);
                    f.release(&Ty::Str, s.into());
                }
                Ty::Float => f.json_write_float(sb, v.into_float_value()),
                Ty::Bool => {
                    let s = f.select(v.into_int_value(), f.cx.lit("true"), f.cx.lit("false"));
                    f.sb_push_str(sb, s.into_pointer_value());
                }
                Ty::Char => {
                    let s = f.call_ptr(f.cx.char_tostr_fn(), &[v]);
                    f.json_write_str(sb, s);
                    f.release(&Ty::Str, s.into());
                }
                Ty::Str => f.json_write_str(sb, v.into_pointer_value()),
                Ty::Opt(inner) => {
                    f.if_else(
                        f.is_nil(&t, v),
                        |f| f.sb_push_lit(sb, "null"),
                        |f| {
                            let x = f.unwrap(&t, v);
                            f.call(
                                f.cx.json_write_fn(inner),
                                &[sb.into(), x, depth.into(), pretty.into()],
                            );
                        },
                    );
                }
                Ty::Array(el) | Ty::Stream(el) => {
                    let a = v.into_pointer_value();
                    let n = f.load_i64(f.at_c(a, LEN));
                    let data = f.load_ptr(f.at_c(a, ARR_DATA));
                    let et = f.cx.llty(el);
                    f.json_write_seq(sb, n, depth, pretty, b'[', b']', |f, k| {
                        let x = f.load(et, f.elem(et, data, k));
                        f.call(f.cx.json_write_fn(el), &[sb.into(), x, next.into(), pretty.into()]);
                    });
                }
                Ty::Bytes => {
                    let b = v.into_pointer_value();
                    let data = f.str_data(b);
                    f.json_write_seq(sb, f.str_len(b), depth, pretty, b'[', b']', |f, k| {
                        let x = f.zext(f.load_i8(f.at(data, k)), f.cx.i64t());
                        let s = f.int_text(x);
                        f.sb_push_str(sb, s);
                        f.release(&Ty::Str, s.into());
                    });
                }
                Ty::Map(vt) => {
                    let d = v.into_pointer_value();
                    let keys = f.call_ptr(f.cx.dict_keys_fn(vt), &[d.into()]);
                    let kd = f.load_ptr(f.at_c(keys, ARR_DATA));
                    let n = f.load_i64(f.at_c(keys, LEN));
                    f.json_write_seq(sb, n, depth, pretty, b'{', b'}', |f, k| {
                        let key = f.load_ptr(f.elem(f.cx.ptrt(), kd, k));
                        f.json_write_key(sb, key, pretty);
                        let x = f.dict_get(vt, d, key);
                        f.call(f.cx.json_write_fn(vt), &[sb.into(), x, next.into(), pretty.into()]);
                    });
                    f.release(&Ty::Array(Box::new(Ty::Str)), keys.into());
                }
                Ty::Struct(name) => {
                    let o = v.into_pointer_value();
                    let st = f.cx.struct_ty(name);
                    let fields = f.cx.struct_fields(name);
                    let names = f.cx.struct_field_names(name);
                    let n = f.i64(fields.len() as i64);
                    let names_lits: Vec<PointerValue> = names.iter().map(|s| f.cx.lit(s)).collect();
                    // Each field is written by its own case, since each has its own type.
                    f.json_write_seq(sb, n, depth, pretty, b'{', b'}', |f, k| {
                        let done = f.block();
                        let mut cases = Vec::new();
                        let mut bodies = Vec::new();
                        for idx in 0..fields.len() {
                            let bb = f.block();
                            cases.push((f.i64(idx as i64), bb));
                            bodies.push((bb, idx));
                        }
                        f.switch(k, done, &cases);
                        for (bb, idx) in bodies {
                            f.position(bb);
                            f.json_write_key(sb, names_lits[idx], pretty);
                            let ft = &fields[idx];
                            let x = f.load(f.cx.llty(ft), f.field(st, o, idx as u32 + 2));
                            f.call(
                                f.cx.json_write_fn(ft),
                                &[sb.into(), x, next.into(), pretty.into()],
                            );
                            f.br(done);
                        }
                        f.position(done);
                    });
                }
                Ty::Json => {
                    let j = v.into_pointer_value();
                    f.if_(f.is_null(j), |f| {
                        f.sb_push_lit(sb, "null");
                        f.ret_void();
                    });
                    let kind = f.load_i64(f.at_c(j, KIND));
                    let w = f.load_i64(f.at_c(j, PAYLOAD));
                    let write = |f: &F<'_, 'ctx>, ty: Ty, x: BasicValueEnum<'ctx>| {
                        f.call(
                            f.cx.json_write_fn(&ty),
                            &[sb.into(), x, depth.into(), pretty.into()],
                        );
                        f.ret_void();
                    };
                    f.if_(f.eq(kind, f.i64(KIND_BOOL)), |f| {
                        write(f, Ty::Bool, f.trunc(w, f.cx.i1t()).into())
                    });
                    f.if_(f.eq(kind, f.i64(KIND_INT)), |f| write(f, Ty::Int, w.into()));
                    f.if_(f.eq(kind, f.i64(KIND_FLOAT)), |f| {
                        write(f, Ty::Float, f.word_float(w).into())
                    });
                    f.if_(f.eq(kind, f.i64(KIND_STR)), |f| write(f, Ty::Str, f.word_ptr(w).into()));
                    f.if_(f.eq(kind, f.i64(KIND_ARRAY)), |f| {
                        write(f, array_ty(), f.word_ptr(w).into())
                    });
                    write(f, object_ty(), f.word_ptr(w).into());
                }
                Ty::Nil => f.sb_push_lit(sb, "null"),
                // Errors, futures and functions have no JSON form.
                _ => f.sb_push_lit(sb, "null"),
            }
            f.ret_void();
        })
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function makes a `Json` value.
    pub fn json_wrap(&self, kind: i64, payload: IntValue<'ctx>) -> PointerValue<'ctx> {
        self.call_ptr(self.cx.json_new_fn(), &[self.i64(kind).into(), payload.into()])
    }

    fn ptr_word(&self, p: PointerValue<'ctx>) -> IntValue<'ctx> {
        self.b.build_ptr_to_int(p, self.cx.i64t(), "").expect("positioned")
    }

    fn word_ptr(&self, w: IntValue<'ctx>) -> PointerValue<'ctx> {
        self.b.build_int_to_ptr(w, self.cx.ptrt(), "").expect("positioned")
    }

    fn float_word(&self, x: FloatValue<'ctx>) -> IntValue<'ctx> {
        self.b.build_bit_cast(x, self.cx.i64t(), "").expect("positioned").into_int_value()
    }

    fn word_float(&self, w: IntValue<'ctx>) -> FloatValue<'ctx> {
        self.b.build_bit_cast(w, self.cx.f64t(), "").expect("positioned").into_float_value()
    }

    fn float_intrinsic_fabs(&self, x: FloatValue<'ctx>) -> FloatValue<'ctx> {
        let intr = inkwell::intrinsics::Intrinsic::find("llvm.fabs").expect("intrinsic");
        let decl =
            intr.get_declaration(&self.cx.module, &[self.cx.f64t().into()]).expect("declaration");
        self.callv(decl, &[x.into()]).into_float_value()
    }

    /// Purpose: This function releases what a `Json` value holds, when its
    /// count reached zero, and frees it.
    pub fn json_destroy(&self, j: PointerValue<'ctx>) {
        let kind = self.load_i64(self.at_c(j, KIND));
        let w = self.load_i64(self.at_c(j, PAYLOAD));
        for (k, t) in [(KIND_STR, Ty::Str), (KIND_ARRAY, array_ty()), (KIND_OBJECT, object_ty())] {
            self.if_(self.eq(kind, self.i64(k)), |f| f.release(&t, f.word_ptr(w).into()));
        }
        self.call(self.cx.libc("free"), &[j.into()]);
    }

    /// Purpose: This function fails from support code unless a `Json` value
    /// has a kind, naming the kind it has in the message.
    fn json_expect(&self, j: PointerValue<'ctx>, kind: i64, msg: &str) {
        let ok = self.var(self.bool(false));
        self.if_(self.not(self.is_null(j)), |f| {
            f.store(ok, f.eq(f.load_i64(f.at_c(j, KIND)), f.i64(kind)));
        });
        let ok = self.load(self.cx.i1t(), ok).into_int_value();
        self.if_(self.not(ok), |f| f.json_kind_fail(j, msg));
    }

    /// Purpose: This function fails from support code with a message whose
    /// `%s` is the value's kind.
    fn json_kind_fail(&self, j: PointerValue<'ctx>, msg: &str) {
        let (before, after) = msg.split_once("%s").unwrap_or((msg, ""));
        let kind = self.call_ptr(self.cx.json_kind_name_fn(), &[j.into()]);
        let m = self.str_cat(&[self.cx.lit(before), kind, self.cx.lit(after)]);
        self.fail_str(m);
    }

    // ── the parser's state ───────────────────────────────────────────────

    fn json_state(
        &self,
        st: PointerValue<'ctx>,
    ) -> (PointerValue<'ctx>, IntValue<'ctx>, IntValue<'ctx>) {
        (
            self.load_ptr(self.at_c(st, ST_DATA)),
            self.load_i64(self.at_c(st, ST_LEN)),
            self.load_i64(self.at_c(st, ST_POS)),
        )
    }

    fn json_advance(&self, st: PointerValue<'ctx>, n: i64) {
        let p = self.at_c(st, ST_POS);
        self.store(p, self.add(self.load_i64(p), self.i64(n)));
    }

    fn json_eof(&self, st: PointerValue<'ctx>) -> IntValue<'ctx> {
        let (_, len, pos) = self.json_state(st);
        self.sge(pos, len)
    }

    /// Purpose: This function says whether the next byte is `b`.
    fn json_at(&self, st: PointerValue<'ctx>, b: u8) -> IntValue<'ctx> {
        let (data, len, pos) = self.json_state(st);
        let r = self.var(self.bool(false));
        self.if_(self.slt(pos, len), |f| {
            f.store(r, f.eq(f.load_i8(f.at(data, pos)), f.i8(b as i64)));
        });
        self.load(self.cx.i1t(), r).into_int_value()
    }

    fn json_skip_ws(&self, st: PointerValue<'ctx>) {
        self.while_(
            |f| {
                let sp = f.or(f.json_at(st, b' '), f.json_at(st, b'\n'));
                f.or(sp, f.or(f.json_at(st, b'\t'), f.json_at(st, b'\r')))
            },
            |f, _| f.json_advance(st, 1),
        );
    }

    /// Purpose: This function raises a parse error from support code and
    /// returns.
    fn json_fail(&self, st: PointerValue<'ctx>, msg: &str, peek: bool) {
        self.call(
            self.cx.json_fail_fn(),
            &[st.into(), self.cx.lit(msg).into(), self.bool(peek).into()],
        );
        self.ret_zero();
    }

    /// Purpose: This function reads the rest of `null`, `true` or `false`.
    fn json_ident(&self, st: PointerValue<'ctx>, word: &str) {
        self.json_advance(st, 1);
        for b in word.bytes().skip(1) {
            self.if_(self.json_eof(st), |f| f.json_fail(st, "EOF while parsing a value", false));
            let ok = self.json_at(st, b);
            self.json_advance(st, 1);
            self.if_(self.not(ok), |f| f.json_fail(st, "expected ident", false));
        }
    }

    /// Purpose: This function reads four hex digits after `\u`.
    fn json_hex4(&self, st: PointerValue<'ctx>, sb: PointerValue<'ctx>) -> IntValue<'ctx> {
        let v = self.var(self.i64(0));
        for _ in 0..4 {
            self.if_(self.json_eof(st), |f| {
                f.call(f.cx.libc("free"), &[f.load_ptr(sb).into()]);
                f.json_fail(st, "EOF while parsing a string", false);
            });
            let (data, _, pos) = self.json_state(st);
            let c = self.zext(self.load_i8(self.at(data, pos)), self.cx.i64t());
            self.json_advance(st, 1);
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
                f.json_fail(st, "invalid escape", false);
            });
            self.store(v, self.or(self.shl(self.load_i64(v), self.i64(4)), d));
        }
        self.load_i64(v)
    }

    // ── writing ──────────────────────────────────────────────────────────

    /// Purpose: This function writes a bracketed sequence with serde's layout:
    /// `[a,b]` compact, or one item a line, indented two spaces a level.
    #[allow(clippy::too_many_arguments)]
    fn json_write_seq(
        &self,
        sb: PointerValue<'ctx>,
        n: IntValue<'ctx>,
        depth: IntValue<'ctx>,
        pretty: IntValue<'ctx>,
        open: u8,
        close: u8,
        item: impl Fn(&Self, IntValue<'ctx>),
    ) {
        let (o, c) = ((open as char).to_string(), (close as char).to_string());
        self.sb_push_lit(sb, &o);
        self.for_range(self.i64(0), n, |f, k, _| {
            f.if_(f.sgt(k, f.i64(0)), |f| f.sb_push_lit(sb, ","));
            f.if_(pretty, |f| {
                f.sb_push_lit(sb, "\n");
                f.json_indent(sb, f.add(depth, f.i64(1)));
            });
            item(f, k);
            f.if_(f.err_pending(), |f| f.ret_void());
        });
        self.if_(self.and(pretty, self.sgt(n, self.i64(0))), |f| {
            f.sb_push_lit(sb, "\n");
            f.json_indent(sb, depth);
        });
        self.sb_push_lit(sb, &c);
    }

    fn json_indent(&self, sb: PointerValue<'ctx>, level: IntValue<'ctx>) {
        self.for_range(self.i64(0), level, |f, _, _| f.sb_push_lit(sb, "  "));
    }

    fn json_write_key(
        &self,
        sb: PointerValue<'ctx>,
        key: PointerValue<'ctx>,
        pretty: IntValue<'ctx>,
    ) {
        self.json_write_str(sb, key);
        let sep = self.select(pretty, self.cx.lit(": "), self.cx.lit(":"));
        self.sb_push_str(sb, sep.into_pointer_value());
    }

    /// Purpose: This function writes a JSON string with serde's escapes.
    fn json_write_str(&self, sb: PointerValue<'ctx>, s: PointerValue<'ctx>) {
        self.call(self.cx.json_write_str_fn(), &[sb.into(), s.into()]);
    }

    /// Purpose: This function writes a float the way serde did through Ryu:
    /// `1.0`, `0.001`, `1e16`, `1.5e-7`, and `null` for NaN and infinity.
    fn json_write_float(&self, sb: PointerValue<'ctx>, x: FloatValue<'ctx>) {
        let s = self.call_ptr(self.cx.json_float_fn(), &[x.into()]);
        self.sb_push_str(sb, s);
        self.release(&Ty::Str, s.into());
    }
}

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns `jade.json.write_str(sb, s)`.
    fn json_write_str_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.json.write_str", None, &[p.into(), p.into()], |f| {
            let (sb, s) = (f.param_ptr(0), f.param_ptr(1));
            f.sb_taint(sb, f.str_trust(s));
            f.sb_push_lit(sb, "\"");
            let data = f.str_data(s);
            let hex = f.alloca(f.cx.i8t().array_type(8));
            f.for_range(f.i64(0), f.str_len(s), |f, k, _| {
                let c = f.zext(f.load_i8(f.at(data, k)), f.cx.i32t());
                let done = f.block();
                let other = f.block();
                let mut cases = Vec::new();
                let mut bodies = Vec::new();
                for (ch, text) in [
                    (34, "\\\""),
                    (92, "\\\\"),
                    (10, "\\n"),
                    (13, "\\r"),
                    (9, "\\t"),
                    (8, "\\b"),
                    (12, "\\f"),
                ] {
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
                f.if_else(
                    f.icmp(inkwell::IntPredicate::ULT, c, f.i32(0x20)),
                    |f| {
                        let n = f.call_int(
                            f.cx.libc("snprintf"),
                            &[hex.into(), f.i64(8).into(), f.cx.cstr("\\u%04x").into(), c.into()],
                        );
                        f.sb_push(sb, hex, f.sext(n, f.cx.i64t()));
                    },
                    |f| f.sb_push(sb, f.at(data, k), f.i64(1)),
                );
                f.br(done);
                f.position(done);
            });
            f.sb_push_lit(sb, "\"");
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.json.float(x) -> str`, in Ryu's
    /// layout from the same shortest digits `print` uses.
    fn json_float_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.json.float", Some(self.ptrt().into()), &[self.f64t().into()], |f| {
            let x = f.param(0).into_float_value();
            let finite = f.fcmp(
                inkwell::FloatPredicate::OLT,
                f.float_intrinsic_fabs(x),
                f.f64(f64::INFINITY),
            );
            f.if_(f.not(finite), |f| f.ret(f.cx.lit("null")));
            let (neg, digits, len, exp) = f.float_digits(x);
            let sb = f.sb_new();
            f.if_(neg, |f| f.sb_push_lit(sb, "-"));
            // `kk` is where the point falls after the first `kk` digits.
            let kk = f.add(exp, f.i64(1));
            let k = f.sub(kk, len);
            let zeros = |f: &F<'_, 'ctx>, n: IntValue<'ctx>| {
                f.for_range(f.i64(0), n, |f, _, _| f.sb_push_lit(sb, "0"));
            };
            let small = f.sle(kk, f.i64(16));
            let whole = f.and(f.sge(k, f.i64(0)), small);
            let mid = f.and(f.sgt(kk, f.i64(0)), small);
            let frac = f.and(f.sgt(kk, f.i64(-5)), f.sle(kk, f.i64(0)));
            f.if_else(
                whole,
                |f| {
                    f.sb_push(sb, digits, len);
                    zeros(f, k);
                    f.sb_push_lit(sb, ".0");
                },
                |f| {
                    f.if_else(
                        mid,
                        |f| {
                            f.sb_push(sb, digits, kk);
                            f.sb_push_lit(sb, ".");
                            f.sb_push(sb, f.at(digits, kk), f.sub(len, kk));
                        },
                        |f| {
                            f.if_else(
                                frac,
                                |f| {
                                    f.sb_push_lit(sb, "0.");
                                    zeros(f, f.sub(f.i64(0), kk));
                                    f.sb_push(sb, digits, len);
                                },
                                |f| {
                                    f.sb_push(sb, digits, f.i64(1));
                                    f.if_(f.sgt(len, f.i64(1)), |f| {
                                        f.sb_push_lit(sb, ".");
                                        f.sb_push(sb, f.at_c(digits, 1), f.sub(len, f.i64(1)));
                                    });
                                    f.sb_push_lit(sb, "e");
                                    let e = f.int_text(f.sub(kk, f.i64(1)));
                                    f.sb_push_str(sb, e);
                                    f.release(&Ty::Str, e.into());
                                },
                            );
                        },
                    );
                },
            );
            f.ret(f.sb_finish(sb));
        })
    }

    /// Purpose: This function returns a class's field names, in layout order.
    pub fn struct_field_names(&self, name: &str) -> Vec<String> {
        self.structs
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.fields.iter().map(|(n, _)| n.clone()).collect())
            .unwrap_or_default()
    }
}
