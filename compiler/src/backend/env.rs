//! `std::env` and `std::path`: the process environment, and paths as text.
//!
//! An environment variable is set by whoever started the program, so its value
//! is tainted. The arguments and the working directory are the program's own,
//! as in v1, so they are trusted.
//!
//! Path functions follow Rust's `std::path::Path` on Unix, which v1 used. A
//! path's components are what lies between slashes, where repeated slashes
//! count as one, a trailing slash adds nothing, and a `.` component is dropped
//! everywhere but at the start. Every result carries the trust of the paths it
//! came from.

use inkwell::values::{FunctionValue, IntValue, PointerValue};

use super::layout::{ARR_DATA, LEN};
use super::{Cx, F};
use crate::mir::{EnvFn, PathFn, Ty};

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns the globals the C `main` stores its
    /// `argc` and `argv` in, for `env.args()`.
    pub fn args_globals(&self) -> (PointerValue<'ctx>, PointerValue<'ctx>) {
        let get = |name: &str, t: inkwell::types::BasicTypeEnum<'ctx>| {
            if let Some(g) = self.module.get_global(name) {
                return g.as_pointer_value();
            }
            let g = self.module.add_global(t, None, name);
            g.set_initializer(&super::layout::zero(t));
            g.set_linkage(inkwell::module::Linkage::Internal);
            g.as_pointer_value()
        };
        (get("jade.argc", self.i64t().into()), get("jade.argv", self.ptrt().into()))
    }

    /// Purpose: This function returns the support function behind a
    /// `std::env` function.
    pub fn env_fn(&self, m: EnvFn) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        let name = format!("jade.env.{m:?}").to_lowercase();
        match m {
            EnvFn::Get => self.support(&name, Some(p.into()), &[p.into()], |f| {
                let v = f.call_ptr(f.cx.libc("getenv"), &[f.str_data(f.param_ptr(0)).into()]);
                f.if_(f.is_null(v), |f| f.ret(f.null()));
                let n = f.call_int(f.cx.libc("strlen"), &[v.into()]);
                let s = f.str_from_trust(v, n, f.i64(1));
                // Rust's `env::var` gives nothing for a value that is not UTF-8.
                let bad = f.call_int(f.cx.utf8_invalid_at_fn(), &[s.into()]);
                f.if_(f.sge(bad, f.i64(0)), |f| {
                    f.release(&Ty::Str, s.into());
                    f.ret(f.null());
                });
                f.ret(s);
            }),
            EnvFn::Set => self.support(&name, None, &[p.into(), p.into()], |f| {
                let (k, v) = (f.param_ptr(0), f.param_ptr(1));
                f.call(
                    f.cx.libc("setenv"),
                    &[f.str_data(k).into(), f.str_data(v).into(), f.i32(1).into()],
                );
                f.ret_void();
            }),
            EnvFn::Args => self.support(&name, Some(p.into()), &[], |f| {
                let (argc, argv) = f.cx.args_globals();
                let n = f.load_i64(argc);
                let list = f.load_ptr(argv);
                let arr = f.array_new(&Ty::Str, n);
                let data = f.load_ptr(f.at_c(arr, ARR_DATA));
                f.for_range(f.i64(0), n, |f, i, _| {
                    let a = f.load_ptr(f.elem(f.cx.ptrt(), list, i));
                    let len = f.call_int(f.cx.libc("strlen"), &[a.into()]);
                    let s = f.call_ptr(f.cx.str_from_fn(), &[a.into(), len.into()]);
                    f.store(f.elem(f.cx.ptrt(), data, i), s);
                });
                f.store(f.at_c(arr, LEN), n);
                f.ret(arr);
            }),
            EnvFn::Cwd => self.support(&name, Some(p.into()), &[], |f| {
                let buf = f.alloca(f.cx.i8t().array_type(4096));
                let r = f.call_ptr(f.cx.libc("getcwd"), &[buf.into(), f.i64(4096).into()]);
                f.if_(f.is_null(r), |f| f.ret(f.cx.lit("")));
                let n = f.call_int(f.cx.libc("strlen"), &[buf.into()]);
                f.ret(f.call_ptr(f.cx.str_from_fn(), &[buf.into(), n.into()]));
            }),
        }
    }

    /// Purpose: This function returns the support function behind a
    /// `std::path` function.
    pub fn path_fn(&self, m: PathFn) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        let name = format!("jade.path.{m:?}").to_lowercase();
        match m {
            PathFn::Join => self.support(&name, Some(p.into()), &[p.into(), p.into()], |f| {
                let (a, b) = (f.param_ptr(0), f.param_ptr(1));
                let trust = f.or(f.str_trust(a), f.str_trust(b));
                let blen = f.str_len(b);
                let abs =
                    f.and(f.sgt(blen, f.i64(0)), f.eq(f.load_i8(f.str_data(b)), f.i8(b'/' as i64)));
                f.if_(abs, |f| f.ret(f.copy_with_trust(b, f.i64(0), blen, trust)));
                let alen = f.str_len(a);
                let sep = f.var(f.bool(false));
                f.if_(f.sgt(alen, f.i64(0)), |f| {
                    let last = f.load_i8(f.at(f.str_data(a), f.sub(alen, f.i64(1))));
                    f.store(sep, f.ne(last, f.i8(b'/' as i64)));
                });
                let sep = f.load(f.cx.i1t(), sep).into_int_value();
                let mid = f.select(sep, f.cx.lit("/"), f.cx.lit("")).into_pointer_value();
                f.ret(f.str_cat(&[a, mid, b]));
            }),
            PathFn::Basename | PathFn::Stem | PathFn::Ext => {
                self.support(&name, Some(p.into()), &[p.into()], |f| {
                    let s = f.param_ptr(0);
                    let trust = f.str_trust(s);
                    let (start, end, kind) = f.path_last(s);
                    let normal = f.eq(kind, f.i64(KIND_NORMAL));
                    if m == PathFn::Basename {
                        f.if_(f.not(normal), |f| f.ret(f.cx.lit("")));
                        f.ret(f.copy_with_trust(s, start, f.sub(end, start), trust));
                        return;
                    }
                    let none = if m == PathFn::Ext { f.null() } else { f.cx.lit("") };
                    f.if_(f.not(normal), |f| f.ret(none));
                    // The last dot, unless it is the name's first byte.
                    let data = f.str_data(s);
                    let dot = f.var(f.i64(-1));
                    f.for_range(f.add(start, f.i64(1)), end, |f, i, _| {
                        f.if_(f.eq(f.load_i8(f.at(data, i)), f.i8(b'.' as i64)), |f| {
                            f.store(dot, i)
                        });
                    });
                    let dot = f.load_i64(dot);
                    let found = f.sge(dot, f.i64(0));
                    if m == PathFn::Ext {
                        f.if_(f.not(found), |f| f.ret(f.null()));
                        f.ret(f.copy_with_trust(s, dot, f.sub(end, dot), trust));
                    } else {
                        let stop = f.select_int(found, dot, end);
                        f.ret(f.copy_with_trust(s, start, f.sub(stop, start), trust));
                    }
                })
            }
            PathFn::Dirname => self.support(&name, Some(p.into()), &[p.into()], |f| {
                let s = f.param_ptr(0);
                let (start, _, kind) = f.path_last(s);
                let has_parent = f.and(f.ne(kind, f.i64(KIND_NONE)), f.ne(kind, f.i64(KIND_ROOT)));
                f.if_(f.not(has_parent), |f| f.ret(f.cx.lit(".")));
                let cut = f.call_int(f.cx.path_trim_fn(), &[s.into(), start.into()]);
                f.if_(f.eq(cut, f.i64(0)), |f| f.ret(f.cx.lit(".")));
                f.ret(f.copy_with_trust(s, f.i64(0), cut, f.str_trust(s)));
            }),
            PathFn::Abs => self.support(&name, Some(p.into()), &[p.into()], |f| {
                let s = f.param_ptr(0);
                let n = f.str_len(s);
                f.if_(f.eq(n, f.i64(0)), |f| {
                    f.retain(&Ty::Str, s.into());
                    f.ret(s);
                });
                let data = f.str_data(s);
                let sb = f.sb_new();
                f.sb_taint(sb, f.str_trust(s));
                let rooted = f.eq(f.load_i8(data), f.i8(b'/' as i64));
                f.if_(f.not(rooted), |f| {
                    let cwd = f.call_ptr(f.cx.env_fn(EnvFn::Cwd), &[]);
                    f.sb_push_str(sb, cwd);
                    f.release(&Ty::Str, cwd.into());
                });
                // Each component that is not empty and not `.`, after a slash.
                let begin = f.var(f.i64(0));
                f.for_range(f.i64(0), f.add(n, f.i64(1)), |f, i, _| {
                    let at_end = f.eq(i, n);
                    let slash = f.var(at_end);
                    f.if_(f.not(at_end), |f| {
                        f.store(slash, f.eq(f.load_i8(f.at(data, i)), f.i8(b'/' as i64)));
                    });
                    f.if_(f.load(f.cx.i1t(), slash).into_int_value(), |f| {
                        let b0 = f.load_i64(begin);
                        let len = f.sub(i, b0);
                        let dot = f.and(
                            f.eq(len, f.i64(1)),
                            f.eq(f.load_i8(f.at(data, b0)), f.i8(b'.' as i64)),
                        );
                        f.if_(f.and(f.sgt(len, f.i64(0)), f.not(dot)), |f| {
                            f.sb_push_lit(sb, "/");
                            f.sb_push(sb, f.at(data, b0), len);
                        });
                        f.store(begin, f.add(i, f.i64(1)));
                    });
                });
                let out_len = f.load_i64(f.at_c(sb, 8));
                let trailing = f.eq(f.load_i8(f.at(data, f.sub(n, f.i64(1)))), f.i8(b'/' as i64));
                f.if_else(
                    f.eq(out_len, f.i64(0)),
                    |f| f.sb_push_lit(sb, "/"),
                    |f| f.if_(trailing, |f| f.sb_push_lit(sb, "/")),
                );
                f.ret(f.sb_finish(sb));
            }),
            PathFn::IsAbs => self.support(&name, Some(self.i1t().into()), &[p.into()], |f| {
                let s = f.param_ptr(0);
                let some = f.sgt(f.str_len(s), f.i64(0));
                f.if_(f.not(some), |f| f.ret(f.bool(false)));
                f.ret(f.eq(f.load_i8(f.str_data(s)), f.i8(b'/' as i64)));
            }),
        }
    }

    /// Purpose: This function returns `jade.path.trim(p, len) -> int`: the
    /// length of `p[..len]` once trailing slashes and trailing `.` components
    /// are dropped, keeping a lone leading slash and a leading `.`.
    fn path_trim_fn(&self) -> FunctionValue<'ctx> {
        let i = self.i64t();
        self.support("jade.path.trim", Some(i.into()), &[self.ptrt().into(), i.into()], |f| {
            let (s, len0) = (f.param_ptr(0), f.param_int(1));
            let data = f.str_data(s);
            let len = f.var(len0);
            let byte = |f: &F<'_, 'ctx>, k: IntValue<'ctx>| f.load_i8(f.at(data, k));
            f.while_(
                |f| {
                    let n = f.load_i64(len);
                    let go = f.var(f.bool(false));
                    // A slash at the end, other than the root itself.
                    f.if_(f.sgt(n, f.i64(1)), |f| {
                        let last = byte(f, f.sub(n, f.i64(1)));
                        f.if_(f.eq(last, f.i8(b'/' as i64)), |f| f.store(go, f.bool(true)));
                        // Or a `.` component after a slash.
                        let prev = byte(f, f.sub(n, f.i64(2)));
                        let dot =
                            f.and(f.eq(last, f.i8(b'.' as i64)), f.eq(prev, f.i8(b'/' as i64)));
                        f.if_(dot, |f| f.store(go, f.bool(true)));
                    });
                    f.load(f.cx.i1t(), go).into_int_value()
                },
                |f, _| f.store(len, f.sub(f.load_i64(len), f.i64(1))),
            );
            f.ret(f.load_i64(len));
        })
    }
}

/// What `path_last` found at the end of a path.
const KIND_NONE: i64 = 0;
const KIND_NORMAL: i64 = 1;
const KIND_ROOT: i64 = 2;
const KIND_OTHER: i64 = 3;

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function finds a path's last component.
    ///
    /// Outputs: its byte range and its kind: none for an empty path, root for
    /// `/`, other for `.` or `..`, and normal for a name.
    fn path_last(&self, s: PointerValue<'ctx>) -> (IntValue<'ctx>, IntValue<'ctx>, IntValue<'ctx>) {
        let data = self.str_data(s);
        let end = self.call_int(self.cx.path_trim_fn(), &[s.into(), self.str_len(s).into()]);
        let start = self.var(self.i64(0));
        self.for_range(self.i64(0), end, |f, i, _| {
            f.if_(f.eq(f.load_i8(f.at(data, i)), f.i8(b'/' as i64)), |f| {
                f.store(start, f.add(i, f.i64(1)))
            });
        });
        let start = self.load_i64(start);
        let len = self.sub(end, start);
        let byte = |k: i64| self.load_i8(self.at(data, self.add(start, self.i64(k))));
        let kind = self.var(self.i64(KIND_NORMAL));
        self.if_(self.eq(end, self.i64(0)), |f| f.store(kind, f.i64(KIND_NONE)));
        self.if_(self.eq(len, self.i64(0)), |f| {
            // Only the root is left.
            f.if_(f.sgt(end, f.i64(0)), |f| f.store(kind, f.i64(KIND_ROOT)));
        });
        self.if_(self.eq(len, self.i64(1)), |f| {
            f.if_(f.eq(byte(0), f.i8(b'.' as i64)), |f| f.store(kind, f.i64(KIND_OTHER)));
        });
        self.if_(self.eq(len, self.i64(2)), |f| {
            let dots = f.and(f.eq(byte(0), f.i8(b'.' as i64)), f.eq(byte(1), f.i8(b'.' as i64)));
            f.if_(dots, |f| f.store(kind, f.i64(KIND_OTHER)));
        });
        (start, end, self.load_i64(kind))
    }

    /// Purpose: This function copies a range of a string with a given trust.
    fn copy_with_trust(
        &self,
        s: PointerValue<'ctx>,
        start: IntValue<'ctx>,
        n: IntValue<'ctx>,
        trust: IntValue<'ctx>,
    ) -> PointerValue<'ctx> {
        self.str_from_trust(self.at(self.str_data(s), start), n, trust)
    }
}
