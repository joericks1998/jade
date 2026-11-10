//! `std::http` and `std::uhttp`: requests through `curl`.
//!
//! `std::http` ran `curl` in v1 too, which is how it gets HTTPS without a TLS
//! library in every binary; this is the same command line. `std::uhttp` speaks
//! HTTP over a Unix socket, `unix://<socket>:<path>`, which `curl` reaches with
//! `--unix-socket`. A request writes its body to curl's stdin, reads the reply
//! from curl's stdout, and splits off the status curl appends after it. Only a
//! transport failure raises; a 4xx or 5xx is an ordinary status. A response
//! body came off the network, so it is tainted.
//!
//! `uhttp.stream` keeps curl running and calls the program's handler once per
//! line until the stream ends or the handler returns `false`, which stops curl.

use inkwell::values::{FunctionValue, IntValue, PointerValue};

use super::fs::IO_ERROR;
use super::layout::{DICT_KEYS, DICT_VALS, LEN, TYPE_ID};
use super::sh::Stdio;
use super::{Cx, F};
use crate::mir::{HttpFn, Ty, is_heap};

/// What curl writes after the body, so the status can be told apart from it.
const STATUS_MARK: &str = "\nJADE_STATUS:";

/// `SIGPIPE`, `SIGTERM` and `SIG_IGN`, the same on macOS and Linux.
const SIGPIPE: i64 = 13;
const SIGTERM: i64 = 15;
const SIG_IGN: i64 = 1;

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns the support function behind an http or
    /// uhttp request: `(url, [body,] headers) -> HttpResponse`.
    pub fn http_fn(&self, m: HttpFn, unix: bool) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        let pkg = if unix { "uhttp" } else { "http" };
        let name = format!("jade.{pkg}.{m:?}").to_lowercase();
        let params: Vec<_> = if m.has_body() { vec![p.into(); 3] } else { vec![p.into(); 2] };
        self.support(&name, Some(p.into()), &params, |f| {
            let url = f.param_ptr(0);
            let (body, headers) = if m.has_body() {
                (Some(f.param_ptr(1)), f.param_ptr(2))
            } else {
                (None, f.param_ptr(1))
            };
            let method = m.method();
            let label = if unix { format!("uhttp {method}") } else { format!("http {method} '") };
            let target = f.http_target(url, unix, &label);
            // `--head` rather than v1's `-X HEAD`, which left curl waiting for
            // the body a HEAD response's length promised, and failing.
            let fixed: Vec<&str> = if m == HttpFn::Head {
                vec!["curl", "-sS", "--head", "-o", "/dev/null"]
            } else {
                vec!["curl", "-sS", "-X", method]
            };
            let (argv, keep) =
                f.curl_argv(&fixed, target.socket, headers, body.is_some(), true, target.url);
            let (in_r, in_w) = f.pipe();
            let (out_r, out_w) = f.pipe();
            let io =
                Stdio { stdin: body.map(|_| in_r), stdout: Some(out_w), stderr: None, quiet: true };
            let (pid, err) = f.spawn(f.cx.cstr("curl"), argv, true, io);
            f.close(in_r);
            f.close(out_w);
            f.call(f.cx.libc("free"), &[argv.into()]);
            f.release(&Ty::Array(Box::new(Ty::Str)), keep.into());
            f.release(&Ty::Str, target.url.into());
            f.release(&Ty::Opt(Box::new(Ty::Str)), target.socket.into());
            let fail = |f: &F<'_, 'ctx>, code: IntValue<'ctx>| f.http_fail(&label, url, unix, code);
            f.if_(f.ne(err, f.i32(0)), |f| {
                f.close(in_w);
                f.close(out_r);
                fail(f, f.i64(127));
            });
            if let Some(b) = body {
                // A curl that exits early must not take the program down with
                // a broken pipe.
                let old = f.call_ptr(
                    f.cx.libc("signal"),
                    &[f.i32(SIGPIPE).into(), f.int_ptr(SIG_IGN).into()],
                );
                f.call(
                    f.cx.libc("write"),
                    &[in_w.into(), f.str_data(b).into(), f.str_len(b).into()],
                );
                f.call(f.cx.libc("signal"), &[f.i32(SIGPIPE).into(), old.into()]);
            }
            f.close(in_w);
            let sb = f.sb_new();
            f.read_fd(out_r, sb);
            f.close(out_r);
            let code = f.wait_child(pid);
            f.if_(f.ne(code, f.i64(0)), |f| {
                f.call(f.cx.libc("free"), &[f.load_ptr(sb).into()]);
                fail(f, code);
            });
            let raw = f.sb_finish(sb);
            let (status, body_len) = f.split_status(raw);
            let body = f.copy_bytes(raw, f.i64(0), body_len);
            f.release(&Ty::Bytes, raw.into());
            let body = if m == HttpFn::GetBytes {
                body
            } else {
                // A text body stops at its first NUL, as v1's did.
                let nul = f.call_int(f.cx.libc("strlen"), &[f.str_data(body).into()]);
                let cut = f.copy_bytes(body, f.i64(0), nul);
                f.release(&Ty::Bytes, body.into());
                let text = f.call_ptr(f.cx.utf8_lossy_fn(), &[cut.into()]);
                f.release(&Ty::Bytes, cut.into());
                text
            };
            f.set_trust(body, f.i64(1));
            let class = if m == HttpFn::GetBytes { "HttpBytesResponse" } else { "HttpResponse" };
            let st = f.cx.struct_ty(class);
            let size = f.size_of(st.into());
            let o = f.call_ptr(f.cx.libc("malloc"), &[size.into()]);
            f.store(o, f.i64(1));
            f.store(f.at_c(o, TYPE_ID), f.i64(f.cx.struct_id(class) as i64));
            f.store(f.field(st, o, 2), status);
            f.store(f.field(st, o, 3), body);
            f.ret(o);
        })
    }

    /// Purpose: This function returns `jade.uhttp.stream.<T>(url, handler,
    /// headers)`, where the handler returns a `T`.
    pub fn uhttp_stream_fn(&self, ret: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.uhttp.stream.{}", super::rc::key(ret));
        let ret = ret.clone();
        let p = self.ptrt();
        self.support(&name, None, &[p.into(), p.into(), p.into()], |f| {
            let (url, handler, headers) = (f.param_ptr(0), f.param_ptr(1), f.param_ptr(2));
            let label = "uhttp stream";
            let target = f.http_target(url, true, label);
            let (argv, keep) = f.curl_argv(
                &["curl", "-sS", "-N"],
                target.socket,
                headers,
                false,
                false,
                target.url,
            );
            let (out_r, out_w) = f.pipe();
            let io = Stdio { stdin: None, stdout: Some(out_w), stderr: None, quiet: true };
            let (pid, err) = f.spawn(f.cx.cstr("curl"), argv, true, io);
            f.close(out_w);
            f.call(f.cx.libc("free"), &[argv.into()]);
            f.release(&Ty::Array(Box::new(Ty::Str)), keep.into());
            f.release(&Ty::Str, target.url.into());
            f.release(&Ty::Opt(Box::new(Ty::Str)), target.socket.into());
            f.if_(f.ne(err, f.i32(0)), |f| {
                f.close(out_r);
                f.http_fail(label, url, true, f.i64(127));
            });
            let fn_ty = inkwell::types::BasicType::fn_type(&f.cx.llty(&ret), &[p.into()], false);
            // Calls the handler on one line; true when it asks to stop or raised.
            let deliver = |f: &F<'_, 'ctx>, line: PointerValue<'ctx>| -> IntValue<'ctx> {
                let text = f.sb_lossy_bytes(line);
                let cs =
                    f.b.build_indirect_call(fn_ty, handler, &[text.into()], "")
                        .expect("positioned");
                f.release(&Ty::Str, text.into());
                let stop = f.var(f.err_pending());
                if let inkwell::values::ValueKind::Basic(r) = cs.try_as_basic_value() {
                    if ret == Ty::Bool {
                        let said_no = f.not(r.into_int_value());
                        f.store(stop, f.or(f.load(f.cx.i1t(), stop).into_int_value(), said_no));
                    } else if is_heap(&ret) {
                        f.release(&ret, r);
                    }
                }
                f.load(f.cx.i1t(), stop).into_int_value()
            };
            let pending = f.sb_new();
            let buf = f.alloca(f.cx.i8t().array_type(65536));
            let stopped = f.var(f.bool(false));
            f.while_(
                |f| f.not(f.load(f.cx.i1t(), stopped).into_int_value()),
                |f, exit| {
                    let n = f.call_int(
                        f.cx.libc("read"),
                        &[out_r.into(), buf.into(), f.i64(65536).into()],
                    );
                    f.if_(f.sle(n, f.i64(0)), |f| f.br(exit));
                    f.for_range(f.i64(0), n, |f, k, done| {
                        let c = f.load_i8(f.at(buf, k));
                        f.if_else(
                            f.eq(c, f.i8(10)),
                            |f| {
                                let line = f.sb_take(pending);
                                let stop = deliver(f, line);
                                f.release(&Ty::Bytes, line.into());
                                f.if_(stop, |f| {
                                    f.store(stopped, f.bool(true));
                                    f.br(done);
                                });
                            },
                            |f| f.sb_push(pending, f.at(buf, k), f.i64(1)),
                        );
                    });
                },
            );
            let stopped = f.load(f.cx.i1t(), stopped).into_int_value();
            // A last line without a newline still counts.
            f.if_(f.not(stopped), |f| {
                f.if_(f.sgt(f.load_i64(f.at_c(pending, 8)), f.i64(0)), |f| {
                    let line = f.sb_take(pending);
                    deliver(f, line);
                    f.release(&Ty::Bytes, line.into());
                });
            });
            f.call(f.cx.libc("free"), &[f.load_ptr(pending).into()]);
            f.if_(stopped, |f| {
                f.call(f.cx.libc("kill"), &[pid.into(), f.i32(SIGTERM).into()]);
            });
            f.close(out_r);
            let code = f.wait_child(pid);
            let failed = f.and(f.not(stopped), f.ne(code, f.i64(0)));
            f.if_(f.and(failed, f.not(f.err_pending())), |f| f.http_fail(label, url, true, code));
            f.ret_void();
        })
    }
}

/// Where a request goes: the URL curl is given, and for uhttp the socket.
struct Target<'ctx> {
    url: PointerValue<'ctx>,
    socket: PointerValue<'ctx>,
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function works out what curl is pointed at. For uhttp it
    /// splits `unix://<socket>:<path>` at the first colon after the scheme,
    /// raising v1's message when the URL is malformed.
    fn http_target(&self, url: PointerValue<'ctx>, unix: bool, label: &str) -> Target<'ctx> {
        if !unix {
            self.retain(&Ty::Str, url.into());
            return Target { url, socket: self.null() };
        }
        let label = label.trim_end_matches(" '");
        let data = self.str_data(url);
        let len = self.str_len(url);
        let scheme = self.cx.lit("unix://");
        let ok = self.call_int(
            self.cx.str_search_fn(crate::mir::StrMethod::StartsWith),
            &[url.into(), scheme.into()],
        );
        self.fail_if(self.not(ok), &format!("{IO_ERROR}{label}: url must start with unix://"));
        let colon = self.var(len);
        self.for_range(self.i64(7), len, |f, k, done| {
            f.if_(f.eq(f.load_i8(f.at(data, k)), f.i8(b':' as i64)), |f| {
                f.store(colon, k);
                f.br(done);
            });
        });
        let colon = self.load_i64(colon);
        self.fail_if(self.eq(colon, self.i64(7)), &format!("{IO_ERROR}{label}: empty socket path"));
        let socket = self.copy_bytes(url, self.i64(7), self.sub(colon, self.i64(7)));
        let rest = self.sub(len, colon);
        let path_len =
            self.select_int(self.sgt(rest, self.i64(1)), self.sub(rest, self.i64(1)), self.i64(0));
        let path = self.copy_bytes(url, self.add(colon, self.i64(1)), path_len);
        let slash = self.select(self.eq(path_len, self.i64(0)), self.cx.lit("/"), self.cx.lit(""));
        let full =
            self.str_cat(&[self.cx.lit("http://localhost"), slash.into_pointer_value(), path]);
        self.release(&Ty::Str, path.into());
        Target { url: full, socket }
    }

    /// Purpose: This function builds curl's arguments: the fixed ones,
    /// `--unix-socket`, each header, the body and status flags, then `--` and
    /// the URL.
    ///
    /// Outputs: a malloc'd array the caller frees, and a `[str]` holding the
    /// header lines it points into, which the caller releases once curl has
    /// started.
    fn curl_argv(
        &self,
        fixed: &[&str],
        socket: PointerValue<'ctx>,
        headers: PointerValue<'ctx>,
        body: bool,
        status: bool,
        url: PointerValue<'ctx>,
    ) -> (PointerValue<'ctx>, PointerValue<'ctx>) {
        let p = self.cx.ptrt();
        let nh = self.var(self.i64(0));
        self.if_(self.not(self.is_null(headers)), |f| {
            f.store(nh, f.load_i64(f.at_c(headers, LEN)))
        });
        let nh = self.load_i64(nh);
        let total = self.add(self.i64(fixed.len() as i64 + 10), self.mul(nh, self.i64(2)));
        let argv = self.call_ptr(self.cx.libc("malloc"), &[self.mul(total, self.i64(8)).into()]);
        let at = self.var(self.i64(0));
        let push = |f: &F<'_, 'ctx>, v: PointerValue<'ctx>| {
            let i = f.load_i64(at);
            f.store(f.elem(p, argv, i), v);
            f.store(at, f.add(i, f.i64(1)));
        };
        for a in fixed {
            push(self, self.cx.cstr(a));
        }
        self.if_(self.not(self.is_null(socket)), |f| {
            push(f, f.cx.cstr("--unix-socket"));
            push(f, f.str_data(socket));
        });
        let keep = self.array_new(&Ty::Str, nh);
        self.for_range(self.i64(0), nh, |f, k, _| {
            let key = f.load_ptr(f.elem(p, f.load_ptr(f.at_c(headers, DICT_KEYS)), k));
            let val = f.load_ptr(f.elem(p, f.load_ptr(f.at_c(headers, DICT_VALS)), k));
            let line = f.str_cat(&[key, f.cx.lit(": "), val]);
            push(f, f.cx.cstr("-H"));
            push(f, f.str_data(line));
            f.array_push_owned(&Ty::Str, keep, line.into());
        });
        if body {
            push(self, self.cx.cstr("--data-binary"));
            push(self, self.cx.cstr("@-"));
        }
        if status {
            push(self, self.cx.cstr("-w"));
            push(self, self.cx.cstr(&format!("{STATUS_MARK}%{{http_code}}")));
        }
        push(self, self.cx.cstr("--"));
        push(self, self.str_data(url));
        push(self, self.null());
        (argv, keep)
    }

    /// Purpose: This function reads a descriptor to its end into a string
    /// builder.
    fn read_fd(&self, fd: IntValue<'ctx>, sb: PointerValue<'ctx>) {
        let buf = self.alloca(self.cx.i8t().array_type(65536));
        self.while_(
            |f| f.bool(true),
            |f, exit| {
                let n =
                    f.call_int(f.cx.libc("read"), &[fd.into(), buf.into(), f.i64(65536).into()]);
                f.if_(f.sle(n, f.i64(0)), |f| f.br(exit));
                f.sb_push(sb, buf, n);
            },
        );
    }

    /// Purpose: This function finds the status curl wrote after the body.
    ///
    /// Outputs: the status, 0 when it is missing, and the body's length.
    fn split_status(&self, raw: PointerValue<'ctx>) -> (IntValue<'ctx>, IntValue<'ctx>) {
        let mark = self.cx.lit(STATUS_MARK);
        let at = self.call_int(self.cx.str_rfind_fn(), &[raw.into(), mark.into()]);
        let found = self.sge(at, self.i64(0));
        let status = self.var(self.i64(0));
        self.if_(found, |f| {
            let data = f.str_data(raw);
            let from = f.add(at, f.i64(STATUS_MARK.len() as i64));
            f.for_range(from, f.str_len(raw), |f, k, done| {
                let c = f.zext(f.load_i8(f.at(data, k)), f.cx.i64t());
                let digit = f.and(f.sge(c, f.i64(b'0' as i64)), f.sle(c, f.i64(b'9' as i64)));
                f.if_(f.not(digit), |f| f.br(done));
                f.store(
                    status,
                    f.add(f.mul(f.load_i64(status), f.i64(10)), f.sub(c, f.i64(b'0' as i64))),
                );
            });
        });
        let len = self.select_int(found, at, self.str_len(raw));
        (self.load_i64(status), len)
    }

    /// Purpose: This function raises a transport failure from support code,
    /// worded as v1 worded it, and returns.
    fn http_fail(&self, label: &str, url: PointerValue<'ctx>, unix: bool, code: IntValue<'ctx>) {
        let reason = |f: &F<'_, 'ctx>| {
            let out = f.var(f.cx.lit("request failed"));
            for (c, text) in [
                (6, "could not resolve host"),
                (
                    7,
                    if unix {
                        "could not connect to the socket"
                    } else {
                        "could not connect to host"
                    },
                ),
                (28, "request timed out"),
                (35, "TLS handshake failed"),
                (127, "curl executable not found"),
            ] {
                f.if_(f.eq(code, f.i64(c)), |f| f.store(out, f.cx.lit(text)));
            }
            f.load_ptr(out)
        };
        let r = reason(self);
        let msg = if unix {
            self.str_cat(&[self.cx.lit(&format!("{IO_ERROR}{label}: ")), r])
        } else {
            let tail = self.format(" (curl exit %lld)", &[code]);
            let m = self.str_cat(&[
                self.cx.lit(&format!("{IO_ERROR}{label}")),
                url,
                self.cx.lit("': "),
                r,
                tail,
            ]);
            self.release(&Ty::Str, tail.into());
            m
        };
        self.fail_str(msg);
    }

    /// Purpose: This function turns a string builder's bytes into a new bytes
    /// value and empties the builder for reuse.
    fn sb_take(&self, sb: PointerValue<'ctx>) -> PointerValue<'ctx> {
        let len_p = self.at_c(sb, 8);
        let b = self.call_ptr(
            self.cx.str_from_fn(),
            &[self.load_ptr(sb).into(), self.load_i64(len_p).into()],
        );
        self.store(len_p, self.i64(0));
        b
    }

    /// Purpose: This function decodes a line of network bytes as tainted text.
    fn sb_lossy_bytes(&self, b: PointerValue<'ctx>) -> PointerValue<'ctx> {
        let s = self.call_ptr(self.cx.utf8_lossy_fn(), &[b.into()]);
        self.set_trust(s, self.i64(1));
        s
    }

    /// Purpose: This function makes a pointer from an integer constant, for
    /// `SIG_IGN`.
    fn int_ptr(&self, v: i64) -> PointerValue<'ctx> {
        self.b.build_int_to_ptr(self.i64(v), self.cx.ptrt(), "").expect("positioned")
    }
}
