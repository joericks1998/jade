//! `std::fs`: files and directories through the C library.
//!
//! Reading and writing go through `fopen`, which is the same on every Unix.
//! The questions `exists`, `is_file`, `is_dir` and `size` need `stat`, and
//! listing a directory needs `readdir`; their structs differ by platform, so
//! `stat_layout` and `DIRENT_NAME` hold the offsets for the targets Jade
//! builds on. Failures raise `I/O error: <op> '<path>': <reason> (os error
//! <n>)`, the text Rust's `io::Error` gave v1.
//!
//! *Trust.* What `read` and `read_bytes` return came from outside the program,
//! so it is tainted, unless the call says `trust = true`. Reading a path that
//! is itself tainted is refused unless the call says the same, because a path
//! a model or a file chose can name anything on the machine.

use inkwell::values::{FunctionValue, IntValue, PointerValue};

use super::{Cx, F};
use crate::mir::{FsFn, Ty};

/// `S_IFMT`, `S_IFDIR` and `S_IFREG`, the same on every Unix.
const S_IFMT: i64 = 0o170000;
const S_IFDIR: i64 = 0o040000;
const S_IFREG: i64 = 0o100000;

/// The prefix v1 gave every failure of the outside world: a file, a command,
/// or bytes that were not text.
pub const IO_ERROR: &str = "I/O error: ";

/// The size of the stack buffer files are read through.
const CHUNK: i64 = 64 * 1024;

/// The byte offset of `d_name` in a `struct dirent`.
const DIRENT_NAME: i64 = if cfg!(target_os = "macos") { 21 } else { 19 };

/// Purpose: This function returns where `struct stat` keeps the mode and the
/// size on this target, and whether the mode is 16 bits wide.
fn stat_layout() -> (i64, bool, i64) {
    if cfg!(target_os = "macos") {
        (4, true, 96)
    } else if cfg!(target_arch = "x86_64") {
        (24, false, 48)
    } else {
        // The generic layout aarch64 and riscv64 Linux share.
        (16, false, 48)
    }
}

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns the support function behind a `std::fs`
    /// function.
    ///
    /// Inputs:
    /// - `m`: the function.
    ///
    /// Outputs: `jade.fs.<name>`, taking the operands `FsFn` documents.
    pub fn fs_fn(&self, m: FsFn) -> FunctionValue<'ctx> {
        let (p, i1) = (self.ptrt(), self.i1t());
        let name = format!("jade.fs.{m:?}").to_lowercase();
        match m {
            FsFn::Read | FsFn::ReadBytes => {
                self.support(&name, Some(p.into()), &[p.into(), i1.into()], |f| {
                    let (path, trust) = (f.param_ptr(0), f.param_int(1));
                    let sink =
                        if m == FsFn::Read { "fs.read(path)" } else { "fs.read_bytes(path)" };
                    f.if_(f.not(trust), |f| f.refuse_tainted(path, sink));
                    let op = if m == FsFn::Read { "read" } else { "read_bytes" };
                    let file = f.fopen(path, "rb");
                    f.if_(f.is_null(file), |f| f.io_fail(op, path));
                    let sb = f.sb_new();
                    let failed = f.read_all(file, sb);
                    f.call(f.cx.libc("fclose"), &[file.into()]);
                    f.if_(failed, |f| {
                        f.call(f.cx.libc("free"), &[f.load_ptr(sb).into()]);
                        f.io_fail(op, path);
                    });
                    let s = f.sb_finish(sb);
                    f.set_trust(s, f.zext(f.not(trust), f.cx.i64t()));
                    if m == FsFn::Read {
                        let bad = f.call_int(f.cx.utf8_invalid_at_fn(), &[s.into()]);
                        f.if_(f.sge(bad, f.i64(0)), |f| {
                            f.release(&Ty::Str, s.into());
                            let msg = f.str_cat(&[
                                f.cx.lit(&format!("{IO_ERROR}read '")),
                                path,
                                f.cx.lit("': stream did not contain valid UTF-8"),
                            ]);
                            f.fail_str(msg);
                        });
                    }
                    f.ret(s);
                })
            }
            FsFn::ReadStdinBytes => self.support(&name, Some(p.into()), &[], |f| {
                let buf = f.alloca(f.cx.i8t().array_type(CHUNK as u32));
                let sb = f.sb_new();
                f.while_(
                    |_| f.bool(true),
                    |f, exit| {
                        let n = f.call_int(
                            f.cx.libc("read"),
                            &[f.i32(0).into(), buf.into(), f.i64(CHUNK).into()],
                        );
                        f.if_(f.slt(n, f.i64(0)), |f| {
                            f.call(f.cx.libc("free"), &[f.load_ptr(sb).into()]);
                            f.io_fail("read_stdin_bytes", f.cx.lit("<stdin>"));
                        });
                        f.if_(f.eq(n, f.i64(0)), |f| f.br(exit));
                        f.sb_push(sb, buf, n);
                    },
                );
                f.sb_taint(sb, f.i64(1));
                f.ret(f.sb_finish(sb));
            }),
            FsFn::Write | FsFn::Append | FsFn::WriteBytes | FsFn::AppendBytes => {
                self.support(&name, None, &[p.into(), p.into()], |f| {
                    let (path, data) = (f.param_ptr(0), f.param_ptr(1));
                    let (op, mode) = match m {
                        FsFn::Write => ("write", "wb"),
                        FsFn::Append => ("append", "ab"),
                        FsFn::WriteBytes => ("write_bytes", "wb"),
                        _ => ("append_bytes", "ab"),
                    };
                    let file = f.fopen(path, mode);
                    f.if_(f.is_null(file), |f| f.io_fail(op, path));
                    let len = f.str_len(data);
                    let n = f.call_int(
                        f.cx.libc("fwrite"),
                        &[f.str_data(data).into(), f.i64(1).into(), len.into(), file.into()],
                    );
                    let closed = f.call_int(f.cx.libc("fclose"), &[file.into()]);
                    let bad = f.or(f.ne(n, len), f.ne(closed, f.i32(0)));
                    f.if_(bad, |f| f.io_fail(op, path));
                    f.ret_void();
                })
            }
            FsFn::WriteStdoutBytes => self.support(&name, None, &[p.into()], |f| {
                let data = f.param_ptr(0);
                let n = f.call_int(
                    f.cx.libc("write"),
                    &[f.i32(1).into(), f.str_data(data).into(), f.str_len(data).into()],
                );
                f.if_(f.slt(n, f.i64(0)), |f| {
                    f.io_fail("write_stdout_bytes", f.cx.lit("<stdout>"))
                });
                f.ret_void();
            }),
            FsFn::Exists | FsFn::IsFile | FsFn::IsDir => {
                self.support(&name, Some(i1.into()), &[p.into()], |f| {
                    let path = f.param_ptr(0);
                    let st = f.alloca(f.cx.i8t().array_type(256));
                    let r = f.call_int(f.cx.libc("stat"), &[f.str_data(path).into(), st.into()]);
                    f.if_(f.ne(r, f.i32(0)), |f| f.ret(f.bool(false)));
                    let fmt = f.and(f.stat_mode(st), f.i64(S_IFMT));
                    let r = match m {
                        FsFn::Exists => f.bool(true),
                        FsFn::IsFile => f.eq(fmt, f.i64(S_IFREG)),
                        _ => f.eq(fmt, f.i64(S_IFDIR)),
                    };
                    f.ret(r);
                })
            }
            FsFn::Size => self.support(&name, Some(self.i64t().into()), &[p.into()], |f| {
                let path = f.param_ptr(0);
                let st = f.alloca(f.cx.i8t().array_type(256));
                let r = f.call_int(f.cx.libc("stat"), &[f.str_data(path).into(), st.into()]);
                f.if_(f.ne(r, f.i32(0)), |f| f.io_fail("size", path));
                f.ret(f.load_i64(f.at_c(st, stat_layout().2)));
            }),
            FsFn::Delete | FsFn::Rmdir => self.support(&name, None, &[p.into()], |f| {
                let path = f.param_ptr(0);
                let (c, op) =
                    if m == FsFn::Delete { ("unlink", "delete") } else { ("rmdir", "rmdir") };
                let r = f.call_int(f.cx.libc(c), &[f.str_data(path).into()]);
                f.if_(f.ne(r, f.i32(0)), |f| f.io_fail(op, path));
                f.ret_void();
            }),
            FsFn::Mkdir => self.support(&name, None, &[p.into()], |f| {
                let path = f.param_ptr(0);
                // Make each parent in turn on a copy of the path, cutting it
                // at each slash; one that exists already is fine.
                let len = f.str_len(path);
                let buf = f.call_ptr(f.cx.libc("malloc"), &[f.add(len, f.i64(1)).into()]);
                f.memcpy(buf, f.str_data(path), f.add(len, f.i64(1)));
                f.for_range(f.i64(1), len, |f, i, _| {
                    let at = f.at(buf, i);
                    f.if_(f.eq(f.load_i8(at), f.i8(b'/' as i64)), |f| {
                        f.store(at, f.i8(0));
                        f.call(f.cx.libc("mkdir"), &[buf.into(), f.i32(0o777).into()]);
                        f.store(at, f.i8(b'/' as i64));
                    });
                });
                f.call(f.cx.libc("free"), &[buf.into()]);
                let r =
                    f.call_int(f.cx.libc("mkdir"), &[f.str_data(path).into(), f.i32(0o777).into()]);
                f.if_(f.ne(r, f.i32(0)), |f| {
                    let is_dir = f.call_int(f.cx.fs_fn(FsFn::IsDir), &[path.into()]);
                    f.if_(f.not(is_dir), |f| f.io_fail("mkdir", path));
                });
                f.ret_void();
            }),
            FsFn::ListDir => self.support(&name, Some(p.into()), &[p.into()], |f| {
                let path = f.param_ptr(0);
                let dir = f.call_ptr(f.cx.libc("opendir"), &[f.str_data(path).into()]);
                f.fail_if(f.is_null(dir), "list_dir: could not read directory");
                let arr = f.array_new(&Ty::Str, f.i64(8));
                f.while_(
                    |_| f.bool(true),
                    |f, exit| {
                        let ent = f.call_ptr(f.cx.libc("readdir"), &[dir.into()]);
                        f.if_(f.is_null(ent), |f| f.br(exit));
                        let name = f.at_c(ent, DIRENT_NAME);
                        let n = f.call_int(f.cx.libc("strlen"), &[name.into()]);
                        let dot =
                            f.and(f.eq(n, f.i64(1)), f.eq(f.load_i8(name), f.i8(b'.' as i64)));
                        let dotdot = f.and(
                            f.eq(n, f.i64(2)),
                            f.and(
                                f.eq(f.load_i8(name), f.i8(b'.' as i64)),
                                f.eq(f.load_i8(f.at_c(name, 1)), f.i8(b'.' as i64)),
                            ),
                        );
                        f.if_(f.not(f.or(dot, dotdot)), |f| {
                            let s = f.call_ptr(f.cx.str_from_fn(), &[name.into(), n.into()]);
                            f.array_push_owned(&Ty::Str, arr, s.into());
                        });
                    },
                );
                f.call(f.cx.libc("closedir"), &[dir.into()]);
                f.ret(arr);
            }),
            FsFn::Copy => self.support(&name, None, &[p.into(), p.into()], |f| {
                let (src, dst) = (f.param_ptr(0), f.param_ptr(1));
                let from = f.fopen(src, "rb");
                f.if_(f.is_null(from), |f| f.io_fail("copy", src));
                let to = f.fopen(dst, "wb");
                f.if_(f.is_null(to), |f| {
                    f.call(f.cx.libc("fclose"), &[from.into()]);
                    f.io_fail("copy", src);
                });
                let buf = f.alloca(f.cx.i8t().array_type(CHUNK as u32));
                f.while_(
                    |_| f.bool(true),
                    |f, exit| {
                        let n = f.call_int(
                            f.cx.libc("fread"),
                            &[buf.into(), f.i64(1).into(), f.i64(CHUNK).into(), from.into()],
                        );
                        f.if_(f.eq(n, f.i64(0)), |f| f.br(exit));
                        f.call(
                            f.cx.libc("fwrite"),
                            &[buf.into(), f.i64(1).into(), n.into(), to.into()],
                        );
                    },
                );
                let bad = f.ne(f.call_int(f.cx.libc("ferror"), &[from.into()]), f.i32(0));
                f.call(f.cx.libc("fclose"), &[from.into()]);
                let closed = f.call_int(f.cx.libc("fclose"), &[to.into()]);
                f.if_(f.or(bad, f.ne(closed, f.i32(0))), |f| f.io_fail("copy", src));
                f.ret_void();
            }),
            FsFn::Rename => self.support(&name, None, &[p.into(), p.into()], |f| {
                let (src, dst) = (f.param_ptr(0), f.param_ptr(1));
                let r = f.call_int(
                    f.cx.libc("rename"),
                    &[f.str_data(src).into(), f.str_data(dst).into()],
                );
                f.if_(f.ne(r, f.i32(0)), |f| f.io_fail("rename", src));
                f.ret_void();
            }),
        }
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function opens a file with `fopen`.
    fn fopen(&self, path: PointerValue<'ctx>, mode: &str) -> PointerValue<'ctx> {
        self.call_ptr(
            self.cx.libc("fopen"),
            &[self.str_data(path).into(), self.cx.cstr(mode).into()],
        )
    }

    /// Purpose: This function reads an open file to its end into a string
    /// builder.
    ///
    /// Outputs: true when reading failed.
    fn read_all(&self, file: PointerValue<'ctx>, sb: PointerValue<'ctx>) -> IntValue<'ctx> {
        let buf = self.alloca(self.cx.i8t().array_type(CHUNK as u32));
        self.while_(
            |_| self.bool(true),
            |f, exit| {
                let n = f.call_int(
                    f.cx.libc("fread"),
                    &[buf.into(), f.i64(1).into(), f.i64(CHUNK).into(), file.into()],
                );
                f.if_(f.eq(n, f.i64(0)), |f| f.br(exit));
                f.sb_push(sb, buf, n);
            },
        );
        self.ne(self.call_int(self.cx.libc("ferror"), &[file.into()]), self.i32(0))
    }

    /// Purpose: This function reads the file type and permission bits out of
    /// a `struct stat`.
    fn stat_mode(&self, st: PointerValue<'ctx>) -> IntValue<'ctx> {
        let (off, narrow, _) = stat_layout();
        let p = self.at_c(st, off);
        if narrow {
            let v = self.load(self.cx.ctx.i16_type(), p).into_int_value();
            self.zext(v, self.cx.i64t())
        } else {
            self.zext(self.load_i32(p), self.cx.i64t())
        }
    }

    /// Purpose: This function raises the last C library error for a file
    /// operation, from support code, and returns.
    ///
    /// Inputs:
    /// - `op`: the operation's name, such as `read`.
    /// - `path`: the path it was given.
    pub fn io_fail(&self, op: &str, path: PointerValue<'ctx>) {
        let code = self.load_i32(self.call_ptr(self.cx.libc("errno"), &[]));
        let reason = self.call_ptr(self.cx.libc("strerror"), &[code.into()]);
        let rlen = self.call_int(self.cx.libc("strlen"), &[reason.into()]);
        let reason = self.call_ptr(self.cx.str_from_fn(), &[reason.into(), rlen.into()]);
        let tail = self.format(" (os error %lld)", &[self.sext(code, self.cx.i64t())]);
        let head = self.cx.lit(&format!("{IO_ERROR}{op} '"));
        let msg = self.str_cat(&[head, path, self.cx.lit("': "), reason, tail]);
        self.release(&Ty::Str, reason.into());
        self.release(&Ty::Str, tail.into());
        self.fail_str(msg);
    }

    /// Purpose: This function raises v1's refusal when a tainted string
    /// reaches a sink, from support code, and returns.
    ///
    /// Inputs:
    /// - `s`: the string.
    /// - `sink`: how the sink is named, such as `sh.exec(cmd)`.
    pub fn refuse_tainted(&self, s: PointerValue<'ctx>, sink: &str) {
        let tainted = self.ne(self.str_trust(s), self.i64(0));
        self.fail_if(
            tainted,
            &format!(
                "refused tainted string in {sink} — value derived from an untrusted source \
                 (LLM, network, file, stdin) and cannot flow to a code-execution sink"
            ),
        );
    }
}
