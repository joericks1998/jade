//! The file-system hooks behind `std::fs`, which is written in Jade,
//! `src/lib/fs.jde`.
//!
//! Each hook is one C library operation. Reading and writing go through
//! `fopen`, which is the same on every Unix. `stat` and `readdir` fill structs
//! that differ by platform, so `stat_layout` and `DIRENT_NAME` hold the
//! offsets for the targets Jade builds on. A hook that fails returns an empty
//! value and leaves the C library's error code in the thread's error slot
//! (see `sys.rs`), at the moment v1 read it, so the message library code
//! builds from it names the same reason v1's did.
//!
//! *Trust.* What the read hooks return came from outside the program, so it
//! is tainted, unless the call says `trust`. Reading a path that is itself
//! tainted is refused unless the call says the same, because a path a model
//! or a file chose can name anything on the machine. Both happen here, where
//! library code cannot skip them.

use inkwell::values::{FunctionValue, IntValue, PointerValue};

use crate::codegen::{Cx, F};
use crate::mir::Ty;

/// The size of the stack buffer files are read and copied through.
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
    /// Purpose: This function returns `jade.sys.read_text(path, trust, sink)
    /// -> str?` or `jade.sys.read_file(path, trust, sink) -> bytes`: a whole
    /// file, tainted unless `trust`, with a tainted path refused unless
    /// `trust`. The text form gives nil for octets that are not UTF-8.
    ///
    /// Inputs:
    /// - `text`: true for the text form.
    pub fn sys_read_fn(&self, text: bool) -> FunctionValue<'ctx> {
        let (p, i1) = (self.ptrt(), self.i1t());
        let name = if text { "jade.sys.read_text" } else { "jade.sys.read_file" };
        self.support(name, Some(p.into()), &[p.into(), i1.into(), p.into()], |f| {
            let (path, trust, sink) = (f.param_ptr(0), f.param_int(1), f.param_ptr(2));
            f.if_(f.not(trust), |f| f.refuse_tainted_in(path, sink, "path"));
            // What a failure returns: nil text, or empty bytes.
            let empty = |f: &F<'_, 'ctx>| if text { f.null() } else { f.cx.lit("") };
            f.os_ok();
            let file = f.fopen(path, "rb");
            f.if_(f.is_null(file), |f| {
                f.os_fail();
                f.ret(empty(f));
            });
            let sb = f.sb_new();
            let failed = f.read_file_into(file, sb);
            f.call(f.cx.libc("fclose"), &[file.into()]);
            f.if_(failed, |f| {
                f.call(f.cx.libc("free"), &[f.load_ptr(sb).into()]);
                f.os_fail();
                f.ret(empty(f));
            });
            let s = f.sb_finish(sb);
            f.set_trust(s, f.zext(f.not(trust), f.cx.i64t()));
            if text {
                let bad = f.call_int(f.cx.utf8_invalid_at_fn(), &[s.into()]);
                f.if_(f.sge(bad, f.i64(0)), |f| {
                    f.release(&Ty::Str, s.into());
                    f.ret(f.null());
                });
            }
            f.ret(s);
        })
    }

    /// Purpose: This function returns `jade.sys.write_file(path, data,
    /// append)`: write a file's octets, or add them to its end, making the
    /// file when it is not there.
    pub fn sys_write_file_fn(&self) -> FunctionValue<'ctx> {
        let (p, i1) = (self.ptrt(), self.i1t());
        self.support("jade.sys.write_file", None, &[p.into(), p.into(), i1.into()], |f| {
            let (path, data, append) = (f.param_ptr(0), f.param_ptr(1), f.param_int(2));
            f.os_ok();
            let mode = f.select(append, f.cx.cstr("ab"), f.cx.cstr("wb")).into_pointer_value();
            let file = f.call_ptr(f.cx.libc("fopen"), &[f.str_data(path).into(), mode.into()]);
            f.if_(f.is_null(file), |f| {
                f.os_fail();
                f.ret_void();
            });
            let len = f.str_len(data);
            let n = f.call_int(
                f.cx.libc("fwrite"),
                &[f.str_data(data).into(), f.i64(1).into(), len.into(), file.into()],
            );
            let closed = f.call_int(f.cx.libc("fclose"), &[file.into()]);
            f.if_(f.or(f.ne(n, len), f.ne(closed, f.i32(0))), |f| f.os_fail());
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.sys.stat(path) -> [int]`: the
    /// mode and the size, or zeros when `stat` fails.
    pub fn sys_stat_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.sys.stat", Some(p.into()), &[p.into()], |f| {
            let st = f.alloca(f.cx.i8t().array_type(256));
            let r = f.call_int(f.cx.libc("stat"), &[f.str_data(f.param_ptr(0)).into(), st.into()]);
            let ok = f.eq(r, f.i32(0));
            f.os_result(ok);
            let mode = f.select_int(ok, f.stat_mode(st), f.i64(0));
            let size = f.select_int(ok, f.load_i64(f.at_c(st, stat_layout().2)), f.i64(0));
            f.ret(f.int_array(&[mode, size]));
        })
    }

    /// Purpose: This function returns a hook that makes one C library call on
    /// a path, or on two, and records whether it failed: `unlink`, `rmdir`,
    /// `mkdir`, with mode 0777, or `rename`.
    ///
    /// Inputs:
    /// - `c`: the C function.
    pub fn sys_path_call_fn(&self, c: &'static str) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        let params = if c == "rename" { vec![p.into(), p.into()] } else { vec![p.into()] };
        self.support(&format!("jade.sys.{c}"), None, &params, |f| {
            let path = f.str_data(f.param_ptr(0));
            let r = match c {
                "mkdir" => f.call_int(f.cx.libc("mkdir"), &[path.into(), f.i32(0o777).into()]),
                "rename" => f.call_int(
                    f.cx.libc("rename"),
                    &[path.into(), f.str_data(f.param_ptr(1)).into()],
                ),
                _ => f.call_int(f.cx.libc(c), &[path.into()]),
            };
            f.os_result(f.eq(r, f.i32(0)));
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.sys.copy_file(src, dst)`: copy a
    /// file's octets in pieces. A failure is the error at the moment v1 read
    /// it: opening the source, opening the destination after the source is
    /// closed again, or the last C call after a failed read or close.
    pub fn sys_copy_file_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.sys.copy_file", None, &[p.into(), p.into()], |f| {
            let (src, dst) = (f.param_ptr(0), f.param_ptr(1));
            f.os_ok();
            let from = f.fopen(src, "rb");
            f.if_(f.is_null(from), |f| {
                f.os_fail();
                f.ret_void();
            });
            let to = f.fopen(dst, "wb");
            f.if_(f.is_null(to), |f| {
                f.call(f.cx.libc("fclose"), &[from.into()]);
                f.os_fail();
                f.ret_void();
            });
            let buf = f.alloca(f.cx.i8t().array_type(CHUNK as u32));
            f.while_(
                |f| f.bool(true),
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
            f.if_(f.or(bad, f.ne(closed, f.i32(0))), |f| f.os_fail());
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.sys.read_dir(path) -> [str]`:
    /// every entry's name, `.` and `..` included, in the order the file system
    /// gives them, as the program's own text. An empty array when the
    /// directory cannot be opened.
    pub fn sys_read_dir_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.sys.read_dir", Some(p.into()), &[p.into()], |f| {
            let dir = f.call_ptr(f.cx.libc("opendir"), &[f.str_data(f.param_ptr(0)).into()]);
            f.os_result(f.not(f.is_null(dir)));
            f.if_(f.is_null(dir), |f| f.ret(f.array_new(&Ty::Str, f.i64(0))));
            let arr = f.array_new(&Ty::Str, f.i64(8));
            f.while_(
                |_| f.bool(true),
                |f, exit| {
                    let ent = f.call_ptr(f.cx.libc("readdir"), &[dir.into()]);
                    f.if_(f.is_null(ent), |f| f.br(exit));
                    let name = f.at_c(ent, DIRENT_NAME);
                    let n = f.call_int(f.cx.libc("strlen"), &[name.into()]);
                    let s = f.call_ptr(f.cx.str_from_fn(), &[name.into(), n.into()]);
                    f.array_push_owned(&Ty::Str, arr, s.into());
                },
            );
            f.call(f.cx.libc("closedir"), &[dir.into()]);
            f.ret(arr);
        })
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
    fn read_file_into(&self, file: PointerValue<'ctx>, sb: PointerValue<'ctx>) -> IntValue<'ctx> {
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
}
