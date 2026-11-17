//! The process hooks behind `std::sh`, `std::http` and `std::uhttp`, which
//! are written in Jade, `src/lib/sh.jde`, `http.jde` and `uhttp.jde`.
//!
//! A child starts with `posix_spawn`, and its pipes are made so it inherits no
//! other end by accident. `shell` starts `/bin/sh -c cmd` and is a
//! code-execution sink, so it refuses a tainted command itself; `spawn` starts
//! a program found on `PATH`, curl, and refuses a tainted program name. The
//! hooks that read what a child writes mark it tainted, so a command's output
//! cannot be fed back in as a command and a response body is untrusted.
//! `drain` reads two pipes together with `poll`, so a command that fills one
//! while the program waits on the other cannot stall.

use inkwell::values::{FunctionValue, IntValue, PointerValue};

use crate::codegen::memory::layout::{ARR_DATA, LEN};
use crate::codegen::{Cx, F};
use crate::mir::Ty;

/// `POLLIN` and `POLLHUP`, the same on macOS and Linux.
const POLLIN: i64 = 0x1;
const POLLHUP: i64 = 0x10;
/// `F_SETFD` and `FD_CLOEXEC`, the same on macOS and Linux.
const F_SETFD: i64 = 2;
const FD_CLOEXEC: i64 = 1;
/// `SIGPIPE`, `SIGTERM` and `SIG_IGN`, the same on macOS and Linux.
const SIGPIPE: i64 = 13;
const SIGTERM: i64 = 15;
const SIG_IGN: i64 = 1;
/// `EINVAL`, the same on macOS and Linux: what `spawn` reports for no program.
const EINVAL: i64 = 22;
/// The size of one read of a child's output.
const CHUNK: i64 = 64 * 1024;

/// Purpose: This struct says which descriptors a spawned child gets as its
/// standard streams; `None` shares the program's. `quiet` sends its stderr to
/// `/dev/null`.
#[derive(Clone, Copy)]
struct Stdio<'ctx> {
    stdin: Option<IntValue<'ctx>>,
    stdout: Option<IntValue<'ctx>>,
    stderr: Option<IntValue<'ctx>>,
    quiet: bool,
}

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns `jade.sys.shell(cmd, sink, capture) ->
    /// [int]`: start `/bin/sh -c cmd`, refusing a tainted command, and give
    /// its pid and, when `capture`, the read ends of its standard output and
    /// standard error, else -1 for each. A failure to start gives -1s and
    /// leaves `posix_spawn`'s answer in the error slot.
    pub fn sys_shell_fn(&self) -> FunctionValue<'ctx> {
        let (p, i1) = (self.ptrt(), self.i1t());
        self.support("jade.sys.shell", Some(p.into()), &[p.into(), p.into(), i1.into()], |f| {
            let (cmd, sink, capture) = (f.param_ptr(0), f.param_ptr(1), f.param_int(2));
            f.refuse_tainted_in(cmd, sink, "cmd");
            f.os_ok();
            let none = f.i64(-1);
            f.if_(capture, |f| {
                let (out_r, out_w) = f.pipe();
                let (err_r, err_w) = f.pipe();
                let (pid, err) = f.spawn_shell(cmd, Some((out_w, err_w)));
                f.close(out_w);
                f.close(err_w);
                f.if_(f.ne(err, f.i32(0)), |f| {
                    f.close(out_r);
                    f.close(err_r);
                    f.os_fail_with(err);
                    f.ret(f.int_array(&[none, none, none]));
                });
                let fds = [f.to_i64(pid), f.to_i64(out_r), f.to_i64(err_r)];
                f.ret(f.int_array(&fds));
            });
            let (pid, err) = f.spawn_shell(cmd, None);
            f.if_(f.ne(err, f.i32(0)), |f| {
                f.os_fail_with(err);
                f.ret(f.int_array(&[none, none, none]));
            });
            f.ret(f.int_array(&[f.to_i64(pid), none, none]));
        })
    }

    /// Purpose: This function returns `jade.sys.spawn(argv, input) -> [int]`:
    /// start `argv[0]`, found on `PATH`, with its standard output a pipe, its
    /// standard error going nowhere, and with `input` its standard input a
    /// pipe. Gives the pid, the write end of its input or -1, and the read end
    /// of its output. A failure to start gives -1s and leaves the reason in
    /// the error slot.
    pub fn sys_spawn_fn(&self) -> FunctionValue<'ctx> {
        let (p, i1) = (self.ptrt(), self.i1t());
        self.support("jade.sys.spawn", Some(p.into()), &[p.into(), i1.into()], |f| {
            let (list, input) = (f.param_ptr(0), f.param_int(1));
            let none = f.i64(-1);
            let n = f.load_i64(f.at_c(list, LEN));
            let items = f.load_ptr(f.at_c(list, ARR_DATA));
            f.os_ok();
            f.if_(f.eq(n, f.i64(0)), |f| {
                f.os_fail_with(f.i32(EINVAL));
                f.ret(f.int_array(&[none, none, none]));
            });
            let program = f.load_ptr(f.elem(f.cx.ptrt(), items, f.i64(0)));
            f.refuse_tainted(program, "spawn(argv)");
            let ptr = f.cx.ptrt();
            let argv =
                f.call_ptr(f.cx.libc("malloc"), &[f.mul(f.add(n, f.i64(1)), f.i64(8)).into()]);
            f.for_range(f.i64(0), n, |f, i, _| {
                let s = f.load_ptr(f.elem(ptr, items, i));
                f.store(f.elem(ptr, argv, i), f.str_data(s));
            });
            f.store(f.elem(ptr, argv, n), f.null());
            let file = f.str_data(program);
            let (out_r, out_w) = f.pipe();
            let in_w = f.var(none);
            let pid = f.var(f.i32(0));
            let err = f.var(f.i32(0));
            f.if_else(
                input,
                |f| {
                    let (in_r, w) = f.pipe();
                    let io =
                        Stdio { stdin: Some(in_r), stdout: Some(out_w), stderr: None, quiet: true };
                    let (child, e) = f.spawn(file, argv, true, io);
                    f.close(in_r);
                    f.store(in_w, f.to_i64(w));
                    f.store(pid, child);
                    f.store(err, e);
                },
                |f| {
                    let io = Stdio { stdin: None, stdout: Some(out_w), stderr: None, quiet: true };
                    let (child, e) = f.spawn(file, argv, true, io);
                    f.store(pid, child);
                    f.store(err, e);
                },
            );
            f.close(out_w);
            f.call(f.cx.libc("free"), &[argv.into()]);
            let (in_w, pid, err) = (f.load_i64(in_w), f.load_i32(pid), f.load_i32(err));
            f.if_(f.ne(err, f.i32(0)), |f| {
                f.if_(input, |f| f.close(f.trunc(in_w, f.cx.i32t())));
                f.close(out_r);
                f.os_fail_with(err);
                f.ret(f.int_array(&[none, none, none]));
            });
            f.ret(f.int_array(&[f.to_i64(pid), in_w, f.to_i64(out_r)]));
        })
    }

    /// Purpose: This function returns `jade.sys.read_all(fd) -> bytes`: read
    /// a descriptor to its end, tainted. A failure keeps what came before it
    /// and leaves the reason in the error slot.
    pub fn sys_read_all_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.sys.read_all", Some(p.into()), &[self.i64t().into()], |f| {
            let fd = f.trunc(f.param_int(0), f.cx.i32t());
            f.os_ok();
            let buf = f.alloca(f.cx.i8t().array_type(CHUNK as u32));
            let sb = f.sb_new();
            f.while_(
                |f| f.bool(true),
                |f, exit| {
                    let n = f
                        .call_int(f.cx.libc("read"), &[fd.into(), buf.into(), f.i64(CHUNK).into()]);
                    f.if_(f.slt(n, f.i64(0)), |f| {
                        f.os_fail();
                        f.br(exit);
                    });
                    f.if_(f.eq(n, f.i64(0)), |f| f.br(exit));
                    f.sb_push(sb, buf, n);
                },
            );
            f.sb_taint(sb, f.i64(1));
            f.ret(f.sb_finish(sb));
        })
    }

    /// Purpose: This function returns `jade.sys.read_chunk(fd) -> bytes`: one
    /// read of up to 64 KiB, tainted; empty at the end or on a failure.
    pub fn sys_read_chunk_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.sys.read_chunk", Some(p.into()), &[self.i64t().into()], |f| {
            let fd = f.trunc(f.param_int(0), f.cx.i32t());
            let buf = f.alloca(f.cx.i8t().array_type(CHUNK as u32));
            let n = f.call_int(f.cx.libc("read"), &[fd.into(), buf.into(), f.i64(CHUNK).into()]);
            let n = f.select_int(f.slt(n, f.i64(0)), f.i64(0), n);
            f.ret(f.str_from_trust(buf, n, f.i64(1)));
        })
    }

    /// Purpose: This function returns `jade.sys.drain(out, err) -> [bytes]`:
    /// read two pipes to their ends together, both tainted.
    pub fn sys_drain_fn(&self) -> FunctionValue<'ctx> {
        let (p, i) = (self.ptrt(), self.i64t());
        self.support("jade.sys.drain", Some(p.into()), &[i.into(), i.into()], |f| {
            let a = f.trunc(f.param_int(0), f.cx.i32t());
            let b = f.trunc(f.param_int(1), f.cx.i32t());
            let (out, errs) = (f.sb_new(), f.sb_new());
            f.drain_pipes(a, b, out, errs);
            f.sb_taint(out, f.i64(1));
            f.sb_taint(errs, f.i64(1));
            let arr = f.array_new(&Ty::Bytes, f.i64(2));
            f.array_push_owned(&Ty::Bytes, arr, f.sb_finish(out).into());
            f.array_push_owned(&Ty::Bytes, arr, f.sb_finish(errs).into());
            f.ret(arr);
        })
    }

    /// Purpose: This function returns `jade.sys.write_fd(fd, data)`: one
    /// `write` of every octet, a failure left in the error slot.
    pub fn sys_write_fd_fn(&self) -> FunctionValue<'ctx> {
        let (p, i) = (self.ptrt(), self.i64t());
        self.support("jade.sys.write_fd", None, &[i.into(), p.into()], |f| {
            let (fd, data) = (f.trunc(f.param_int(0), f.cx.i32t()), f.param_ptr(1));
            f.os_ok();
            let n = f.call_int(
                f.cx.libc("write"),
                &[fd.into(), f.str_data(data).into(), f.str_len(data).into()],
            );
            f.if_(f.slt(n, f.i64(0)), |f| f.os_fail());
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.sys.write_child(fd, data)`: one
    /// `write` to a child's input with `SIGPIPE` ignored, so a child that
    /// exits early does not take the program down with a broken pipe.
    pub fn sys_write_child_fn(&self) -> FunctionValue<'ctx> {
        let (p, i) = (self.ptrt(), self.i64t());
        self.support("jade.sys.write_child", None, &[i.into(), p.into()], |f| {
            let (fd, data) = (f.trunc(f.param_int(0), f.cx.i32t()), f.param_ptr(1));
            let ign = f.b.build_int_to_ptr(f.i64(SIG_IGN), f.cx.ptrt(), "").expect("positioned");
            let old = f.call_ptr(f.cx.libc("signal"), &[f.i32(SIGPIPE).into(), ign.into()]);
            f.call(
                f.cx.libc("write"),
                &[fd.into(), f.str_data(data).into(), f.str_len(data).into()],
            );
            f.call(f.cx.libc("signal"), &[f.i32(SIGPIPE).into(), old.into()]);
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.sys.wait(pid) -> int`: wait for
    /// a child, giving its exit code, or -1 when a signal ended it.
    pub fn sys_wait_fn(&self) -> FunctionValue<'ctx> {
        let i = self.i64t();
        self.support("jade.sys.wait", Some(i.into()), &[i.into()], |f| {
            let pid = f.trunc(f.param_int(0), f.cx.i32t());
            f.ret(f.wait_child(pid));
        })
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function closes a descriptor held in an int.
    pub fn close_fd(&self, fd: IntValue<'ctx>) {
        self.close(self.trunc(fd, self.cx.i32t()));
    }

    /// Purpose: This function sends a child `SIGTERM`.
    pub fn kill_child(&self, pid: IntValue<'ctx>) {
        let pid = self.trunc(pid, self.cx.i32t());
        self.call(self.cx.libc("kill"), &[pid.into(), self.i32(SIGTERM).into()]);
    }

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
    fn spawn(
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
    fn pipe(&self) -> (IntValue<'ctx>, IntValue<'ctx>) {
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
        let buf = self.alloca(self.cx.i8t().array_type(CHUNK as u32));
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
                                &[fd.into(), buf.into(), f.i64(CHUNK).into()],
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
    fn close(&self, fd: IntValue<'ctx>) {
        self.call(self.cx.libc("close"), &[fd.into()]);
    }

    /// Purpose: This function waits for a child and returns its exit code, or
    /// -1 when a signal ended it.
    fn wait_child(&self, pid: IntValue<'ctx>) -> IntValue<'ctx> {
        let status = self.alloca(self.cx.i32t());
        self.store(status, self.i32(0));
        self.call(self.cx.libc("waitpid"), &[pid.into(), status.into(), self.i32(0).into()]);
        let st = self.sext(self.load_i32(status), self.cx.i64t());
        let exited = self.eq(self.and(st, self.i64(0x7f)), self.i64(0));
        let code = self.and(self.ashr(st, self.i64(8)), self.i64(0xff));
        self.select_int(exited, code, self.i64(-1))
    }
}

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns the C library's `environ`, the process
    /// environment a spawned child inherits.
    fn environ(&self) -> PointerValue<'ctx> {
        if let Some(g) = self.module.get_global("environ") {
            return g.as_pointer_value();
        }
        let g = self.module.add_global(self.ptrt(), None, "environ");
        g.set_linkage(inkwell::module::Linkage::External);
        g.as_pointer_value()
    }
}
