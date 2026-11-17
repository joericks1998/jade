//! The hooks: what the std library written in Jade, `src/lib/`, asks of the C
//! library and LLVM through `__sys`. See `SysFn` in `mir/mod.rs` for each
//! one's type. `sys_op` here emits every hook; the file-system hooks are in
//! `fs.rs`, the process hooks in `process.rs` and the random generator in
//! `random.rs`.
//!
//! Each hook is one operation. The float ones are an LLVM intrinsic, which
//! compiles to an instruction or a call into the C math library, or a C math
//! function directly where LLVM 18 has no intrinsic, such as `tan` and
//! `atan2`. The library function around one is a single call, which the
//! optimizer inlines.
//!
//! The clocks are `clock_gettime`: the wall clock is what a timestamp is, and
//! the monotonic clock never moves backwards, so it is what a duration is
//! measured with. The time-zone hooks set `TZ` for the moment they take and put
//! it back, under a lock, since the environment is the whole program's; that
//! state stays here rather than in library code, which the task check keeps
//! from writing a global. The zone's name comes from the C library, so it is
//! tainted, as v1's `date` output was.
//!
//! *The error slot.* A hook whose C call can fail returns an empty value and
//! leaves the C library's error code in a thread-local slot, `jade.oserr`,
//! which the `os_error` hook reads and library code turns into v1's message.
//! Only hooks write the slot, so an allocation between the hook and the read,
//! which may change C's `errno`, cannot change it.
//!
//! *Trust.* The environment's values are tainted; the arguments and the
//! working directory are the program's own. A hook that brings outside text
//! in marks it, and a hook that runs a command or reads a path refuses tainted
//! text itself, with v1's message, so library code can neither skip a check
//! nor launder a value.
//!
//! `random.rs` shares the spin lock and the wall clock, and `tasks/`
//! the sleep that `time.after`'s task runs.

mod fs;
mod process;
mod random;

use inkwell::values::{BasicValueEnum, FloatValue, FunctionValue, IntValue, PointerValue};

use super::{Cx, F};
use crate::codegen::libcache::shared::{Slot, State};
use crate::codegen::memory::layout::{ARR_DATA, DICT_KEYS, LEN};
use crate::mir::{SysFn, Ty};

/// The bytes a `struct tm` is given: more than any C library's needs.
const TM_SIZE: u32 = 128;

/// Purpose: This function says whether `Cx::sys_op` emits a hook as one
/// instruction, one LLVM intrinsic or one C math call, with no support
/// function behind it. A library function made of such hooks is small enough
/// that it is copied into its callers rather than cached; see `libcache/plan.rs`.
///
/// Inputs:
/// - `h`: the hook.
///
/// Outputs: true for the float and int hooks and `os_error`.
pub fn is_one_op(h: SysFn) -> bool {
    use SysFn::*;
    matches!(
        h,
        Floor
            | Ceil
            | Round
            | Trunc
            | Sqrt
            | Ln
            | Log2
            | Log10
            | Exp
            | Sin
            | Cos
            | Tan
            | Asin
            | Acos
            | Atan
            | Fabs
            | Atan2
            | Hypot
            | Pow
            | Minnum
            | Maxnum
            | Copysign
            | Powi
            | ToInt
            | FloatBits
            | AddSat
            | MulSat
            | WrapAdd
            | WrapSub
            | WrapMul
            | OsError
    )
}

/// Where `tm_gmtoff` sits in a `struct tm`, after nine ints and padding, in
/// glibc, musl and macOS alike.
const TM_GMTOFF: i64 = 40;

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function computes a hook.
    ///
    /// Inputs:
    /// - `f`: the function being built.
    /// - `h`: the hook.
    /// - `args`: its operands, of the types `SysFn::params` names.
    ///
    /// Outputs: the result and its type.
    pub fn sys_op(
        &self,
        f: &F<'_, 'ctx>,
        h: SysFn,
        args: &[BasicValueEnum<'ctx>],
    ) -> (BasicValueEnum<'ctx>, Ty) {
        let intr = |name: &str| f.float_intrinsic(name, args);
        let libm = |name: &str| f.callv(self.libc(name), args);
        let int = |i: usize| args[i].into_int_value();
        // A hook that returns nothing: its call, and nil.
        let void = |func: FunctionValue<'ctx>| -> BasicValueEnum<'ctx> {
            f.call(func, args);
            f.i8(0).into()
        };
        let v: BasicValueEnum<'ctx> = match h {
            SysFn::Floor => intr("llvm.floor"),
            SysFn::Ceil => intr("llvm.ceil"),
            SysFn::Round => intr("llvm.round"),
            SysFn::Trunc => intr("llvm.trunc"),
            SysFn::Sqrt => intr("llvm.sqrt"),
            SysFn::Ln => intr("llvm.log"),
            SysFn::Log2 => intr("llvm.log2"),
            SysFn::Log10 => intr("llvm.log10"),
            SysFn::Exp => intr("llvm.exp"),
            SysFn::Sin => intr("llvm.sin"),
            SysFn::Cos => intr("llvm.cos"),
            SysFn::Fabs => intr("llvm.fabs"),
            SysFn::Pow => intr("llvm.pow"),
            SysFn::Minnum => intr("llvm.minnum"),
            SysFn::Maxnum => intr("llvm.maxnum"),
            SysFn::Copysign => intr("llvm.copysign"),
            SysFn::Tan => libm("tan"),
            SysFn::Asin => libm("asin"),
            SysFn::Acos => libm("acos"),
            SysFn::Atan => libm("atan"),
            SysFn::Atan2 => libm("atan2"),
            SysFn::Hypot => libm("hypot"),
            SysFn::Powi => {
                // v1 passed the exponent as an i32, so a larger one wraps.
                let e = f.trunc(int(1), self.i32t());
                let i = inkwell::intrinsics::Intrinsic::find("llvm.powi").expect("intrinsic");
                let decl = i
                    .get_declaration(&self.module, &[self.f64t().into(), self.i32t().into()])
                    .expect("declaration");
                f.callv(decl, &[args[0], e.into()])
            }
            SysFn::ToInt => f.float_to_int_sat(args[0].into_float_value()).into(),
            SysFn::FloatBits => f.b.build_bit_cast(args[0], self.f64t(), "").expect("positioned"),
            SysFn::AddSat => {
                let i = inkwell::intrinsics::Intrinsic::find("llvm.sadd.sat").expect("intrinsic");
                let decl =
                    i.get_declaration(&self.module, &[self.i64t().into()]).expect("declaration");
                f.callv(decl, args)
            }
            SysFn::MulSat => {
                let (a, b) = (int(0), int(1));
                let (r, over) = f.checked("smul", a, b);
                // The true product is negative when exactly one side is.
                let negative = f.xor(f.slt(a, f.i64(0)), f.slt(b, f.i64(0)));
                let edge = f.select_int(negative, f.i64(i64::MIN), f.i64(i64::MAX));
                f.select_int(over, edge, r).into()
            }
            SysFn::WrapAdd => f.add(int(0), int(1)).into(),
            SysFn::WrapSub => f.sub(int(0), int(1)).into(),
            SysFn::WrapMul => f.mul(int(0), int(1)).into(),
            SysFn::Now => f.clock(false, 1).into(),
            SysFn::NowMs => f.clock(false, 1000).into(),
            SysFn::Monotonic => f.monotonic().into(),
            SysFn::Sleep => {
                f.call(self.sleep_fn(), args);
                f.i8(0).into()
            }
            SysFn::After => f.callv(self.after_fn(), args),
            SysFn::LocalOffset => f.callv(self.local_offset_fn(), args),
            SysFn::LocalZone => f.callv(self.local_zone_fn(), args),
            SysFn::OsError => f.load_i64(self.os_err_slot()).into(),
            SysFn::Strerror => f.callv(self.sys_strerror_fn(), args),
            SysFn::Lossy => f.callv(self.utf8_lossy_fn(), args),
            SysFn::KeysInOrder => f.callv(self.sys_keys_in_order_fn(), args),
            SysFn::Getenv => f.callv(self.sys_getenv_fn(), args),
            SysFn::Setenv => void(self.sys_setenv_fn()),
            SysFn::Args => f.callv(self.sys_args_fn(), args),
            SysFn::Cwd => f.callv(self.sys_cwd_fn(), args),
            SysFn::ReadText => f.callv(self.sys_read_fn(true), args),
            SysFn::ReadFile => f.callv(self.sys_read_fn(false), args),
            SysFn::WriteFile => void(self.sys_write_file_fn()),
            SysFn::Stat => f.callv(self.sys_stat_fn(), args),
            SysFn::Unlink => void(self.sys_path_call_fn("unlink")),
            SysFn::Rmdir => void(self.sys_path_call_fn("rmdir")),
            SysFn::Mkdir => void(self.sys_path_call_fn("mkdir")),
            SysFn::Rename => void(self.sys_path_call_fn("rename")),
            SysFn::CopyFile => void(self.sys_copy_file_fn()),
            SysFn::ReadDir => f.callv(self.sys_read_dir_fn(), args),
            SysFn::Shell => f.callv(self.sys_shell_fn(), args),
            SysFn::Spawn => f.callv(self.sys_spawn_fn(), args),
            SysFn::ReadAll => f.callv(self.sys_read_all_fn(), args),
            SysFn::ReadChunk => f.callv(self.sys_read_chunk_fn(), args),
            SysFn::Drain => f.callv(self.sys_drain_fn(), args),
            SysFn::WriteFd => void(self.sys_write_fd_fn()),
            SysFn::WriteChild => void(self.sys_write_child_fn()),
            SysFn::Close => {
                f.close_fd(int(0));
                f.i8(0).into()
            }
            SysFn::Wait => f.callv(self.sys_wait_fn(), args),
            SysFn::Kill => {
                f.kill_child(int(0));
                f.i8(0).into()
            }
            SysFn::RandBelow => f.callv(self.sys_rand_below_fn(), args),
            SysFn::RandSeed => void(self.sys_rand_seed_fn()),
        };
        (v, h.result())
    }

    /// Purpose: This function returns the thread's error slot, which the
    /// fallible hooks write and `os_error` reads.
    pub fn os_err_slot(&self) -> PointerValue<'ctx> {
        self.state("jade.oserr", State::thread(Slot::Word(0))).as_pointer_value()
    }

    /// Purpose: This function returns `jade.sys.strerror(code) -> str`: the C
    /// library's text for an error code.
    fn sys_strerror_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.sys.strerror", Some(p.into()), &[self.i64t().into()], |f| {
            let code = f.trunc(f.param_int(0), f.cx.i32t());
            let reason = f.call_ptr(f.cx.libc("strerror"), &[code.into()]);
            let n = f.call_int(f.cx.libc("strlen"), &[reason.into()]);
            f.ret(f.call_ptr(f.cx.str_from_fn(), &[reason.into(), n.into()]));
        })
    }

    /// Purpose: This function returns `jade.sys.keys_in_order(d) -> [str]`: a
    /// dict of strings' keys in the order they were added, which is the order
    /// its entries are stored in.
    fn sys_keys_in_order_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.sys.keys_in_order", Some(p.into()), &[p.into()], |f| {
            let d = f.param_ptr(0);
            let n = f.load_i64(f.at_c(d, LEN));
            let keys = f.load_ptr(f.at_c(d, DICT_KEYS));
            let arr = f.array_new(&Ty::Str, n);
            let data = f.load_ptr(f.at_c(arr, ARR_DATA));
            f.for_range(f.i64(0), n, |f, i, _| {
                let k = f.load_ptr(f.elem(f.cx.ptrt(), keys, i));
                f.retain(&Ty::Str, k.into());
                f.store(f.elem(f.cx.ptrt(), data, i), k);
            });
            f.store(f.at_c(arr, LEN), n);
            f.ret(arr);
        })
    }

    /// Purpose: This function returns the globals the C `main` stores its
    /// `argc` and `argv` in, for the `args` hook.
    pub fn args_globals(&self) -> (PointerValue<'ctx>, PointerValue<'ctx>) {
        (
            self.state("jade.argc", State::global(Slot::Word(0))).as_pointer_value(),
            self.state("jade.argv", State::global(Slot::Ptr)).as_pointer_value(),
        )
    }

    /// Purpose: This function returns `jade.sys.getenv(name) -> str?`: a
    /// variable's value, tainted, or nil when it is not set or is not UTF-8,
    /// as Rust's `env::var` gave.
    fn sys_getenv_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.sys.getenv", Some(p.into()), &[p.into()], |f| {
            let v = f.call_ptr(f.cx.libc("getenv"), &[f.str_data(f.param_ptr(0)).into()]);
            f.if_(f.is_null(v), |f| f.ret(f.null()));
            let n = f.call_int(f.cx.libc("strlen"), &[v.into()]);
            let s = f.str_from_trust(v, n, f.i64(1));
            let bad = f.call_int(f.cx.utf8_invalid_at_fn(), &[s.into()]);
            f.if_(f.sge(bad, f.i64(0)), |f| {
                f.release(&Ty::Str, s.into());
                f.ret(f.null());
            });
            f.ret(s);
        })
    }

    /// Purpose: This function returns `jade.sys.setenv(name, value)`.
    fn sys_setenv_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.sys.setenv", None, &[p.into(), p.into()], |f| {
            let (k, v) = (f.param_ptr(0), f.param_ptr(1));
            f.call(
                f.cx.libc("setenv"),
                &[f.str_data(k).into(), f.str_data(v).into(), f.i32(1).into()],
            );
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.sys.args() -> [str]`: the
    /// program's arguments, its own path first, read from where the C `main`
    /// leaves them, which it does when this function exists.
    fn sys_args_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.sys.args", Some(self.ptrt().into()), &[], |f| {
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
        })
    }

    /// Purpose: This function returns `jade.sys.cwd() -> str`: the working
    /// directory, or "" when it cannot be read.
    fn sys_cwd_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.sys.cwd", Some(self.ptrt().into()), &[], |f| {
            let buf = f.alloca(f.cx.i8t().array_type(4096));
            let r = f.call_ptr(f.cx.libc("getcwd"), &[buf.into(), f.i64(4096).into()]);
            f.if_(f.is_null(r), |f| f.ret(f.cx.lit("")));
            let n = f.call_int(f.cx.libc("strlen"), &[buf.into()]);
            f.ret(f.call_ptr(f.cx.str_from_fn(), &[buf.into(), n.into()]));
        })
    }

    /// Purpose: This function returns `jade.time.sleep(secs)`. A delay that is
    /// not positive returns at once.
    pub fn sleep_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.time.sleep", None, &[self.f64t().into()], |f| {
            let secs = f.param(0).into_float_value();
            let positive = f.fcmp(inkwell::FloatPredicate::OGT, secs, f.f64(0.0));
            f.if_(positive, |f| {
                let ts = f
                    .alloca(f.cx.ctx.struct_type(&[f.cx.i64t().into(), f.cx.i64t().into()], false));
                let whole = f.float_to_int_sat(secs);
                let frac = f.fsub(secs, f.itof(whole));
                let nanos = f.float_to_int_sat(f.fmul(frac, f.f64(1e9)));
                let nanos = f.select_int(f.slt(nanos, f.i64(0)), f.i64(0), nanos);
                f.store(ts, whole);
                f.store(f.at_c(ts, 8), nanos);
                f.call(f.cx.libc("nanosleep"), &[ts.into(), f.null().into()]);
            });
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.time.local_offset(tz, ts) -> int`:
    /// seconds east of UTC at an instant, in the zone `tz` names or the
    /// system's when it is nil.
    fn local_offset_fn(&self) -> FunctionValue<'ctx> {
        let (p, i) = (self.ptrt(), self.i64t());
        self.support("jade.time.local_offset", Some(i.into()), &[p.into(), i.into()], |f| {
            let out = f.var(f.i64(0));
            f.in_zone(f.param_ptr(0), f.param_int(1), |f, tm| {
                f.store(out, f.load_i64(f.at_c(tm, TM_GMTOFF)));
            });
            f.ret(f.load_i64(out));
        })
    }

    /// Purpose: This function returns `jade.time.local_zone(tz, ts) -> str`:
    /// the zone's abbreviation at an instant, as `strftime`'s `%Z` writes it,
    /// tainted.
    fn local_zone_fn(&self) -> FunctionValue<'ctx> {
        let (p, i) = (self.ptrt(), self.i64t());
        self.support("jade.time.local_zone", Some(p.into()), &[p.into(), i.into()], |f| {
            let out = f.var(f.null());
            f.in_zone(f.param_ptr(0), f.param_int(1), |f, tm| {
                // Written while `TZ` is still set, since the C library may keep
                // the name in storage the next `tzset` reuses.
                let buf = f.alloca(f.cx.i8t().array_type(TM_SIZE));
                let n = f.call_int(
                    f.cx.libc("strftime"),
                    &[buf.into(), f.i64(TM_SIZE as i64).into(), f.cx.cstr("%Z").into(), tm.into()],
                );
                f.store(out, f.str_from_trust(buf, n, f.i64(1)));
            });
            f.ret(f.load_ptr(out));
        })
    }

    /// Purpose: This function returns the lock the time-zone hooks hold while
    /// `TZ` is changed.
    fn time_lock(&self) -> PointerValue<'ctx> {
        self.state("jade.time.lock", State::global(Slot::Word(0))).as_pointer_value()
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function calls an LLVM intrinsic overloaded on `f64`.
    ///
    /// Inputs:
    /// - `name`: the intrinsic, such as `llvm.floor`.
    /// - `args`: its arguments.
    ///
    /// Outputs: its result.
    fn float_intrinsic(&self, name: &str, args: &[BasicValueEnum<'ctx>]) -> BasicValueEnum<'ctx> {
        let intr = inkwell::intrinsics::Intrinsic::find(name).expect("LLVM has the intrinsic");
        let decl =
            intr.get_declaration(&self.cx.module, &[self.cx.f64t().into()]).expect("declaration");
        self.callv(decl, args)
    }

    /// Purpose: This function turns a float into an int, saturating at the
    /// ends and giving 0 for NaN.
    pub fn float_to_int_sat(&self, v: FloatValue<'ctx>) -> IntValue<'ctx> {
        let intr = inkwell::intrinsics::Intrinsic::find("llvm.fptosi.sat").expect("intrinsic");
        let decl = intr
            .get_declaration(&self.cx.module, &[self.cx.i64t().into(), self.cx.f64t().into()])
            .expect("declaration");
        self.call_int(decl, &[v.into()])
    }

    /// Purpose: This function breaks an instant into local time in a zone,
    /// and runs `body` on the result while the zone is still set.
    ///
    /// Inputs:
    /// - `tz`: the zone's name, or null for the system's.
    /// - `ts`: the instant, in Unix seconds.
    /// - `body`: what to read from the `struct tm`, given a pointer to it.
    ///
    /// Outputs: none. `TZ` is put back as it was, and the lock let go.
    fn in_zone(
        &self,
        tz: PointerValue<'ctx>,
        ts: IntValue<'ctx>,
        body: impl FnOnce(&F<'a, 'ctx>, PointerValue<'ctx>),
    ) {
        let named = self.not(self.is_null(tz));
        let lock = self.cx.time_lock();
        self.spin_lock(lock);
        let saved = self.var(self.null());
        self.if_(named, |f| {
            let old = f.call_ptr(f.cx.libc("getenv"), &[f.cx.cstr("TZ").into()]);
            f.if_(f.not(f.is_null(old)), |f| {
                f.store(saved, f.call_ptr(f.cx.libc("strdup"), &[old.into()]));
            });
            f.call(
                f.cx.libc("setenv"),
                &[f.cx.cstr("TZ").into(), f.str_data(tz).into(), f.i32(1).into()],
            );
            f.call(f.cx.libc("tzset"), &[]);
        });
        let at = self.alloca(self.cx.i64t());
        self.store(at, ts);
        let tm = self.alloca(self.cx.i8t().array_type(TM_SIZE));
        self.call(self.cx.libc("localtime_r"), &[at.into(), tm.into()]);
        body(self, tm);
        self.if_(named, |f| {
            let old = f.load_ptr(saved);
            f.if_else(
                f.is_null(old),
                |f| {
                    f.call(f.cx.libc("unsetenv"), &[f.cx.cstr("TZ").into()]);
                },
                |f| {
                    f.call(
                        f.cx.libc("setenv"),
                        &[f.cx.cstr("TZ").into(), old.into(), f.i32(1).into()],
                    );
                    f.call(f.cx.libc("free"), &[old.into()]);
                },
            );
            f.call(f.cx.libc("tzset"), &[]);
        });
        self.spin_unlock(lock);
    }

    /// Purpose: This function records that a fallible hook succeeded.
    pub fn os_ok(&self) {
        self.store(self.cx.os_err_slot(), self.i64(0));
    }

    /// Purpose: This function records the C library's error code, `errno`, as
    /// what a fallible hook failed with. Call it right after the failing call.
    pub fn os_fail(&self) {
        let code = self.load_i32(self.call_ptr(self.cx.libc("errno"), &[]));
        self.os_fail_with(code);
    }

    /// Purpose: This function records an error code a C call returned rather
    /// than left in `errno`, as `posix_spawn` does.
    pub fn os_fail_with(&self, code: IntValue<'ctx>) {
        self.store(self.cx.os_err_slot(), self.sext(code, self.cx.i64t()));
    }

    /// Purpose: This function records whether a fallible hook's C call
    /// succeeded, reading `errno` when it did not. Call it right after the call.
    pub fn os_result(&self, ok: IntValue<'ctx>) {
        let code = self.var(self.i64(0));
        self.if_(self.not(ok), |f| {
            let e = f.load_i32(f.call_ptr(f.cx.libc("errno"), &[]));
            f.store(code, f.sext(e, f.cx.i64t()));
        });
        self.store(self.cx.os_err_slot(), self.load_i64(code));
    }

    /// Purpose: This function makes an `[int]` holding some values.
    pub fn int_array(&self, items: &[IntValue<'ctx>]) -> PointerValue<'ctx> {
        let arr = self.array_new(&Ty::Int, self.i64(items.len() as i64));
        let data = self.load_ptr(self.at_c(arr, ARR_DATA));
        for (i, v) in items.iter().enumerate() {
            self.store(self.elem(self.cx.i64t(), data, self.i64(i as i64)), *v);
        }
        self.store(self.at_c(arr, LEN), self.i64(items.len() as i64));
        arr
    }

    /// Purpose: This function raises v1's refusal when a tainted string
    /// reaches a sink, from a hook, and returns.
    ///
    /// Inputs:
    /// - `s`: the string.
    /// - `sink`: how the sink is named, such as `spawn(argv)`.
    pub fn refuse_tainted(&self, s: PointerValue<'ctx>, sink: &str) {
        let tainted = self.ne(self.str_trust(s), self.i64(0));
        self.fail_if(tainted, &format!("{REFUSED}{}", refusal_tail(sink)));
    }

    /// Purpose: This function raises v1's refusal when a tainted string
    /// reaches a sink a library function names, from a hook, and returns.
    ///
    /// Inputs:
    /// - `s`: the string.
    /// - `function`: the library function, such as `sh.exec`, as a string.
    /// - `arg`: the argument `s` was, such as `cmd`.
    pub fn refuse_tainted_in(
        &self,
        s: PointerValue<'ctx>,
        function: PointerValue<'ctx>,
        arg: &str,
    ) {
        let tainted = self.ne(self.str_trust(s), self.i64(0));
        self.if_(tainted, |f| {
            let tail = f.cx.lit(&refusal_tail(&format!("({arg})")));
            f.fail_str(f.str_cat(&[f.cx.lit(REFUSED), function, tail]));
        });
    }

    /// Purpose: This function takes a spin lock held in an i64 word.
    pub fn spin_lock(&self, lock: PointerValue<'ctx>) {
        self.while_(
            |f| {
                let old =
                    f.b.build_atomicrmw(
                        inkwell::AtomicRMWBinOp::Xchg,
                        lock,
                        f.i64(1),
                        inkwell::AtomicOrdering::Acquire,
                    )
                    .expect("positioned");
                f.ne(old, f.i64(0))
            },
            |_, _| {},
        );
    }

    /// Purpose: This function gives a spin lock back.
    pub fn spin_unlock(&self, lock: PointerValue<'ctx>) {
        let st = self.b.build_store(lock, self.i64(0)).expect("positioned");
        st.set_atomic_ordering(inkwell::AtomicOrdering::Release).expect("an atomic store");
        st.set_alignment(8).expect("aligned");
    }

    /// Purpose: This function reads a clock as a whole number of units.
    ///
    /// Inputs:
    /// - `monotonic`: true for the clock that never moves backwards, false
    ///   for the wall clock.
    /// - `per_sec`: the units in a second: 1 for seconds, 1000 for milliseconds.
    ///
    /// Outputs: the reading.
    pub fn clock(&self, monotonic: bool, per_sec: i64) -> IntValue<'ctx> {
        let (secs, nanos) = self.timespec(monotonic);
        let whole = self.mul(secs, self.i64(per_sec));
        self.add(whole, self.sdiv(nanos, self.i64(1_000_000_000 / per_sec)))
    }

    /// Purpose: This function reads the monotonic clock in seconds.
    pub fn monotonic(&self) -> FloatValue<'ctx> {
        let (secs, nanos) = self.timespec(true);
        self.fadd(self.itof(secs), self.fdiv(self.itof(nanos), self.f64(1e9)))
    }

    /// Purpose: This function calls `clock_gettime`.
    fn timespec(&self, monotonic: bool) -> (IntValue<'ctx>, IntValue<'ctx>) {
        // CLOCK_REALTIME is 0 everywhere; CLOCK_MONOTONIC differs.
        let id = match (monotonic, cfg!(target_os = "macos")) {
            (false, _) => 0,
            (true, true) => 6,
            (true, false) => 1,
        };
        let i = self.cx.i64t();
        let ts = self.alloca(self.cx.ctx.struct_type(&[i.into(), i.into()], false));
        self.call(self.cx.libc("clock_gettime"), &[self.i32(id).into(), ts.into()]);
        (self.load_i64(ts), self.load_i64(self.at_c(ts, 8)))
    }
}

/// How v1's refusal of a tainted string at a sink begins.
const REFUSED: &str = "refused tainted string in ";

/// Purpose: This function returns the rest of v1's refusal of a tainted
/// string at a sink, from the sink's name on.
///
/// Inputs:
/// - `sink`: how the sink is named, such as `sh.exec(cmd)`, or the end of its
///   name.
///
/// Outputs: the text after `REFUSED`.
fn refusal_tail(sink: &str) -> String {
    format!(
        "{sink} — value derived from an untrusted source (LLM, network, file, stdin) \
         and cannot flow to a code-execution sink"
    )
}
