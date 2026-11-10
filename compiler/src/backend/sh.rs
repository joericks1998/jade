//! `std::sh`: running a command through `sh -c`.
//!
//! The shell starts with `posix_spawn`. `exec` and `output` capture its output
//! through two pipes, read together with `poll` so a command that fills one
//! pipe while the program waits on the other cannot stall; `run` lets the
//! command write to the program's own output. Captured text is decoded as
//! UTF-8 with bad sequences replaced, as v1 did with Rust's `from_utf8_lossy`.
//!
//! *Every function here is a code-execution sink*, so each refuses a tainted
//! command, and what a command prints is tainted, so its output cannot be fed
//! back in as a command.

use inkwell::values::{FunctionValue, IntValue, PointerValue};

use super::fs::IO_ERROR;
use super::layout::TYPE_ID;
use super::{Cx, F};
use crate::mir::{ShFn, StrMethod, Ty};

/// `POLLIN` and `POLLHUP`, the same on macOS and Linux.
const POLLIN: i64 = 0x1;
const POLLHUP: i64 = 0x10;
/// `F_SETFD` and `FD_CLOEXEC`, the same on macOS and Linux.
const F_SETFD: i64 = 2;
const FD_CLOEXEC: i64 = 1;

/// Purpose: This struct says which descriptors a spawned child gets as its
/// standard streams; `None` shares the program's. `quiet` sends its stderr to
/// `/dev/null`.
#[derive(Clone, Copy)]
pub struct Stdio<'ctx> {
    pub stdin: Option<IntValue<'ctx>>,
    pub stdout: Option<IntValue<'ctx>>,
    pub stderr: Option<IntValue<'ctx>>,
    pub quiet: bool,
}

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns the support function behind a `std::sh`
    /// function: `exec(cmd) -> str`, `run(cmd) -> int` or `output(cmd) ->
    /// ShOutput`.
    pub fn sh_fn(&self, m: ShFn) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        let name = format!("jade.sh.{m:?}").to_lowercase();
        let ret = match m {
            ShFn::Run => self.i64t().into(),
            _ => p.into(),
        };
        self.support(&name, Some(ret), &[p.into()], |f| {
            let cmd = f.param_ptr(0);
            let label = match m {
                ShFn::Exec => "sh.exec",
                ShFn::Run => "sh.run",
                ShFn::Output => "sh.output",
            };
            f.refuse_tainted(cmd, &format!("{label}(cmd)"));
            if m == ShFn::Run {
                let (pid, err) = f.spawn_shell(cmd, None);
                f.if_(f.ne(err, f.i32(0)), |f| f.spawn_fail(label, err));
                f.ret(f.wait_child(pid));
                return;
            }
            let (out_r, out_w) = f.pipe();
            let (err_r, err_w) = f.pipe();
            let (pid, err) = f.spawn_shell(cmd, Some((out_w, err_w)));
            f.close(out_w);
            f.close(err_w);
            f.if_(f.ne(err, f.i32(0)), |f| {
                f.close(out_r);
                f.close(err_r);
                f.spawn_fail(label, err);
            });
            let (out, errs) = (f.sb_new(), f.sb_new());
            f.drain_pipes(out_r, err_r, out, errs);
            f.close(out_r);
            f.close(err_r);
            let code = f.wait_child(pid);
            let stdout = f.sb_lossy(out);
            let stderr = f.sb_lossy(errs);
            if m == ShFn::Output {
                let st = f.cx.struct_ty("ShOutput");
                let size = f.size_of(st.into());
                let o = f.call_ptr(f.cx.libc("malloc"), &[size.into()]);
                f.store(o, f.i64(1));
                f.store(f.at_c(o, TYPE_ID), f.i64(f.cx.struct_id("ShOutput") as i64));
                f.store(f.field(st, o, 2), stdout);
                f.store(f.field(st, o, 3), stderr);
                f.store(f.field(st, o, 4), code);
                f.ret(o);
                return;
            }
            f.if_(f.ne(code, f.i64(0)), |f| {
                f.release(&Ty::Str, stdout.into());
                let a = f.str_method(StrMethod::TrimStart, &[stderr.into()]).into_pointer_value();
                let b = f.str_method(StrMethod::TrimEnd, &[a.into()]).into_pointer_value();
                let head = f.format(
                    &format!("{IO_ERROR}sh.exec: command exited with code %lld: "),
                    &[code],
                );
                let msg = f.str_cat(&[head, b]);
                for s in [stderr, a, b, head] {
                    f.release(&Ty::Str, s.into());
                }
                f.fail_str(msg);
            });
            f.release(&Ty::Str, stderr.into());
            // exec drops trailing newlines, and only those.
            let len = f.var(f.str_len(stdout));
            let data = f.str_data(stdout);
            f.while_(
                |f| {
                    let n = f.load_i64(len);
                    let more = f.var(f.bool(false));
                    f.if_(f.sgt(n, f.i64(0)), |f| {
                        let last = f.load_i8(f.at(data, f.sub(n, f.i64(1))));
                        f.store(more, f.eq(last, f.i8(10)));
                    });
                    f.load(f.cx.i1t(), more).into_int_value()
                },
                |f, _| f.store(len, f.sub(f.load_i64(len), f.i64(1))),
            );
            let r = f.copy_bytes(stdout, f.i64(0), f.load_i64(len));
            f.release(&Ty::Str, stdout.into());
            f.ret(r);
        })
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function starts `/bin/sh -c cmd`.
    ///
    /// Inputs:
    /// - `cmd`: the command.
    /// - `pipes`: the write ends to give the shell as its stdout and stderr,
    ///   or `None` to let it share the program's.
    ///
    /// Outputs: the child's pid and `posix_spawn`'s result, 0 on success.
    fn spawn_shell(
        &self,
        cmd: PointerValue<'ctx>,
        pipes: Option<(IntValue<'ctx>, IntValue<'ctx>)>,
    ) -> (IntValue<'ctx>, IntValue<'ctx>) {
        let p = self.cx.ptrt();
        let argv = self.alloca(p.array_type(4));
        let slot = |i: i64| self.elem(p, argv, self.i64(i));
        self.store(slot(0), self.cx.cstr("sh"));
        self.store(slot(1), self.cx.cstr("-c"));
        self.store(slot(2), self.str_data(cmd));
        self.store(slot(3), self.null());
        let io = Stdio {
            stdin: None,
            stdout: pipes.map(|(o, _)| o),
            stderr: pipes.map(|(_, e)| e),
            quiet: false,
        };
        self.spawn(self.cx.cstr("/bin/sh"), argv, false, io)
    }

    /// Purpose: This function starts a program with `posix_spawn`.
    ///
    /// Inputs:
    /// - `file`: the program, a C string.
    /// - `argv`: a null-terminated array of C strings.
    /// - `search`: true to look `file` up on `PATH`.
    /// - `io`: which descriptors to give the child as its standard streams.
    ///
    /// Outputs: the child's pid and `posix_spawn`'s result, 0 on success.
    pub fn spawn(
        &self,
        file: PointerValue<'ctx>,
        argv: PointerValue<'ctx>,
        search: bool,
        io: Stdio<'ctx>,
    ) -> (IntValue<'ctx>, IntValue<'ctx>) {
        let pid = self.alloca(self.cx.i32t());
        let fa = self.alloca(self.cx.i8t().array_type(256));
        let lib = |n: &str| self.cx.libc(n);
        self.call(lib("posix_spawn_file_actions_init"), &[fa.into()]);
        for (fd, to) in [(io.stdin, 0), (io.stdout, 1), (io.stderr, 2)] {
            if let Some(fd) = fd {
                self.call(
                    lib("posix_spawn_file_actions_adddup2"),
                    &[fa.into(), fd.into(), self.i32(to).into()],
                );
            }
        }
        if io.quiet {
            self.call(
                lib("posix_spawn_file_actions_addopen"),
                &[
                    fa.into(),
                    self.i32(2).into(),
                    self.cx.cstr("/dev/null").into(),
                    self.i32(1).into(),
                    self.i32(0).into(),
                ],
            );
        }
        let env = self.load_ptr(self.cx.environ());
        let call = if search { "posix_spawnp" } else { "posix_spawn" };
        let r = self.call_int(
            lib(call),
            &[pid.into(), file.into(), fa.into(), self.null().into(), argv.into(), env.into()],
        );
        self.call(lib("posix_spawn_file_actions_destroy"), &[fa.into()]);
        (self.load_i32(pid), r)
    }

    /// Purpose: This function makes a pipe whose ends a spawned child does not
    /// inherit by accident.
    ///
    /// Outputs: the read end and the write end.
    pub fn pipe(&self) -> (IntValue<'ctx>, IntValue<'ctx>) {
        let fds = self.alloca(self.cx.i32t().array_type(2));
        self.call(self.cx.libc("pipe"), &[fds.into()]);
        let fd = |i: i64| self.load_i32(self.elem(self.cx.i32t(), fds, self.i64(i)));
        for i in 0..2 {
            self.call(
                self.cx.libc("fcntl"),
                &[fd(i).into(), self.i32(F_SETFD).into(), self.i32(FD_CLOEXEC).into()],
            );
        }
        (fd(0), fd(1))
    }

    /// Purpose: This function raises `<label>: could not spawn shell: <reason>`
    /// from support code, and returns.
    fn spawn_fail(&self, label: &str, err: IntValue<'ctx>) {
        let reason = self.call_ptr(self.cx.libc("strerror"), &[err.into()]);
        let rlen = self.call_int(self.cx.libc("strlen"), &[reason.into()]);
        let reason = self.call_ptr(self.cx.str_from_fn(), &[reason.into(), rlen.into()]);
        let tail = self.format(" (os error %lld)", &[self.sext(err, self.cx.i64t())]);
        let head = self.cx.lit(&format!("{IO_ERROR}{label}: could not spawn shell: "));
        let msg = self.str_cat(&[head, reason, tail]);
        self.release(&Ty::Str, reason.into());
        self.release(&Ty::Str, tail.into());
        self.fail_str(msg);
    }

    /// Purpose: This function reads two pipes to their ends, into two string
    /// builders.
    fn drain_pipes(
        &self,
        a: IntValue<'ctx>,
        b: IntValue<'ctx>,
        sa: PointerValue<'ctx>,
        sb: PointerValue<'ctx>,
    ) {
        // Two `struct pollfd { int fd; short events; short revents; }`.
        let pfd = self.alloca(self.cx.i64t().array_type(2));
        let open = self.alloca(self.cx.i1t().array_type(2));
        let buf = self.alloca(self.cx.i8t().array_type(65536));
        for (k, fd) in [(0, a), (1, b)] {
            self.store(self.at_c(pfd, k * 8), fd);
            self.store(self.elem(self.cx.i1t(), open, self.i64(k)), self.bool(true));
        }
        let is_open = |f: &F<'_, 'ctx>, k: i64| {
            f.load(f.cx.i1t(), f.elem(f.cx.i1t(), open, f.i64(k))).into_int_value()
        };
        self.while_(
            |f| f.or(is_open(f, 0), is_open(f, 1)),
            |f, _| {
                for k in 0..2 {
                    // A closed pipe is skipped by giving poll a negative fd.
                    let fd = if k == 0 { a } else { b };
                    let live = is_open(f, k);
                    f.store(f.at_c(pfd, k * 8), f.select_int(live, fd, f.i32(-1)));
                    f.store(
                        f.at_c(pfd, k * 8 + 4),
                        f.cx.ctx.i16_type().const_int(POLLIN as u64, false),
                    );
                    f.store(f.at_c(pfd, k * 8 + 6), f.cx.ctx.i16_type().const_zero());
                }
                let n =
                    f.call_int(f.cx.libc("poll"), &[pfd.into(), f.i64(2).into(), f.i32(-1).into()]);
                f.if_(f.sge(n, f.i32(0)), |f| {
                    for (k, fd, dst) in [(0, a, sa), (1, b, sb)] {
                        let rev =
                            f.load(f.cx.ctx.i16_type(), f.at_c(pfd, k * 8 + 6)).into_int_value();
                        let rev = f.zext(rev, f.cx.i64t());
                        let ready = f.ne(f.and(rev, f.i64(POLLIN | POLLHUP)), f.i64(0));
                        f.if_(f.and(ready, is_open(f, k)), |f| {
                            let got = f.call_int(
                                f.cx.libc("read"),
                                &[fd.into(), buf.into(), f.i64(65536).into()],
                            );
                            f.if_else(
                                f.sgt(got, f.i64(0)),
                                |f| f.sb_push(dst, buf, got),
                                |f| f.store(f.elem(f.cx.i1t(), open, f.i64(k)), f.bool(false)),
                            );
                        });
                    }
                });
            },
        );
    }

    /// Purpose: This function closes a file descriptor.
    pub fn close(&self, fd: IntValue<'ctx>) {
        self.call(self.cx.libc("close"), &[fd.into()]);
    }

    /// Purpose: This function waits for a child and returns its exit code, or
    /// -1 when a signal ended it.
    pub fn wait_child(&self, pid: IntValue<'ctx>) -> IntValue<'ctx> {
        let status = self.alloca(self.cx.i32t());
        self.store(status, self.i32(0));
        self.call(self.cx.libc("waitpid"), &[pid.into(), status.into(), self.i32(0).into()]);
        let st = self.sext(self.load_i32(status), self.cx.i64t());
        let exited = self.eq(self.and(st, self.i64(0x7f)), self.i64(0));
        let code = self.and(self.ashr(st, self.i64(8)), self.i64(0xff));
        self.select_int(exited, code, self.i64(-1))
    }

    /// Purpose: This function turns a string builder holding a command's
    /// output into tainted text, replacing invalid UTF-8.
    pub fn sb_lossy(&self, sb: PointerValue<'ctx>) -> PointerValue<'ctx> {
        let raw = self.sb_finish(sb);
        let s = self.call_ptr(self.cx.utf8_lossy_fn(), &[raw.into()]);
        self.release(&Ty::Bytes, raw.into());
        self.set_trust(s, self.i64(1));
        s
    }
}

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns the C library's `environ`, the process
    /// environment a spawned shell inherits.
    fn environ(&self) -> PointerValue<'ctx> {
        if let Some(g) = self.module.get_global("environ") {
            return g.as_pointer_value();
        }
        let g = self.module.add_global(self.ptrt(), None, "environ");
        g.set_linkage(inkwell::module::Linkage::External);
        g.as_pointer_value()
    }
}
