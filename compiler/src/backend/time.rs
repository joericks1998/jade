//! `std::time`: the two clocks, sleeping, and UTC calendar conversion.
//!
//! The wall clock (`now`, `now_ms`) is what a timestamp is; the monotonic clock
//! never moves backwards, so it is what a duration is measured with. Both are
//! `clock_gettime`. Calendar conversion is integer arithmetic on the proleptic
//! Gregorian calendar, Howard Hinnant's `days_from_civil` and `civil_from_days`,
//! exactly as v1 computed it in `jade-runtime::timef`. `time.after` is a task,
//! so it lives in `tasks.rs`.

use inkwell::values::{FloatValue, FunctionValue, IntValue};

use super::layout::TYPE_ID;
use super::{Cx, F};

/// Seconds in a day.
const DAY: i64 = 86_400;

impl<'ctx> Cx<'ctx> {
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

    /// Purpose: This function returns `jade.time.days_from_civil(y, m, d) ->
    /// int`: days since 1970-01-01 for a date whose month is 1 through 12. A day
    /// past the month's end carries into the next.
    fn days_from_civil_fn(&self) -> FunctionValue<'ctx> {
        let i = self.i64t();
        self.support(
            "jade.time.days_from_civil",
            Some(i.into()),
            &[i.into(), i.into(), i.into()],
            |f| {
                let (y, m, d) = (f.param_int(0), f.param_int(1), f.param_int(2));
                let y = f.select_int(f.sle(m, f.i64(2)), f.sub(y, f.i64(1)), y);
                let era =
                    f.sdiv(f.select_int(f.sge(y, f.i64(0)), y, f.sub(y, f.i64(399))), f.i64(400));
                let yoe = f.sub(y, f.mul(era, f.i64(400)));
                let mp = f.srem(f.add(m, f.i64(9)), f.i64(12));
                let doy = f.add(
                    f.sdiv(f.add(f.mul(f.i64(153), mp), f.i64(2)), f.i64(5)),
                    f.sub(d, f.i64(1)),
                );
                let doe = f.add(
                    f.sub(
                        f.add(f.mul(yoe, f.i64(365)), f.sdiv(yoe, f.i64(4))),
                        f.sdiv(yoe, f.i64(100)),
                    ),
                    doy,
                );
                let days = f.sub(f.add(f.mul(era, f.i64(146_097)), doe), f.i64(719_468));
                f.ret(days);
            },
        )
    }

    /// Purpose: This function returns `jade.time.parts(ts) -> TimeParts`: a
    /// Unix timestamp broken into UTC calendar fields.
    pub fn time_parts_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.time.parts", Some(self.ptrt().into()), &[self.i64t().into()], |f| {
            let ts = f.param_int(0);
            let days = f.floor_div(ts, DAY);
            let secs = f.floor_rem(ts, DAY);
            let (year, month, day) = f.civil_from_days(days);
            let jan1 = f.call_int(
                f.cx.days_from_civil_fn(),
                &[year.into(), f.i64(1).into(), f.i64(1).into()],
            );
            let fields = [
                year,
                month,
                day,
                f.sdiv(secs, f.i64(3_600)),
                f.srem(f.sdiv(secs, f.i64(60)), f.i64(60)),
                f.srem(secs, f.i64(60)),
                // 1970-01-01 was a Thursday, weekday 4 counting from Sunday.
                f.floor_rem(f.add(days, f.i64(4)), 7),
                f.add(f.sub(days, jan1), f.i64(1)),
            ];
            let st = f.cx.struct_ty("TimeParts");
            let size = f.size_of(st.into());
            let p = f.call_ptr(f.cx.libc("malloc"), &[size.into()]);
            f.store(p, f.i64(1));
            f.store(f.at_c(p, TYPE_ID), f.i64(f.cx.struct_id("TimeParts") as i64));
            for (i, v) in fields.iter().enumerate() {
                f.store(f.field(st, p, i as u32 + 2), *v);
            }
            f.ret(p);
        })
    }

    /// Purpose: This function returns `jade.time.stamp(y, mo, d, h, mi, s) ->
    /// int`, the inverse of `parts`. Every field carries: month 13 is next
    /// January and day 0 is the last day of the month before.
    pub fn time_stamp_fn(&self) -> FunctionValue<'ctx> {
        let i = self.i64t();
        let params = [i.into(), i.into(), i.into(), i.into(), i.into(), i.into()];
        self.support("jade.time.stamp", Some(i.into()), &params, |f| {
            let p = |n: u32| f.param_int(n);
            let m0 = f.sub(p(1), f.i64(1));
            let year = f.sat("sadd", p(0), f.floor_div(m0, 12));
            let month = f.add(f.floor_rem(m0, 12), f.i64(1));
            let days =
                f.call_int(f.cx.days_from_civil_fn(), &[year.into(), month.into(), p(2).into()]);
            let mut t = f.sat_mul(days, DAY);
            t = f.sat("sadd", t, f.sat_mul(p(3), 3_600));
            t = f.sat("sadd", t, f.sat_mul(p(4), 60));
            t = f.sat("sadd", t, p(5));
            f.ret(t);
        })
    }

    /// Purpose: This function returns `jade.time.utc(ts) -> str`: ISO 8601,
    /// such as `2026-08-16T14:03:22Z`.
    pub fn time_utc_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.time.utc", Some(self.ptrt().into()), &[self.i64t().into()], |f| {
            let ts = f.param_int(0);
            let p = f.call_ptr(f.cx.time_parts_fn(), &[ts.into()]);
            let st = f.cx.struct_ty("TimeParts");
            let field = |i: u32| f.load_i64(f.field(st, p, i + 2));
            let buf = f.alloca(f.cx.i8t().array_type(64));
            let n = f.call_int(
                f.cx.libc("snprintf"),
                &[
                    buf.into(),
                    f.i64(64).into(),
                    f.cx.cstr("%04lld-%02lld-%02lldT%02lld:%02lld:%02lldZ").into(),
                    field(0).into(),
                    field(1).into(),
                    field(2).into(),
                    field(3).into(),
                    field(4).into(),
                    field(5).into(),
                ],
            );
            f.release(&crate::mir::Ty::Struct("TimeParts".into()), p.into());
            let s = f.call_ptr(f.cx.str_from_fn(), &[buf.into(), f.sext(n, f.cx.i64t()).into()]);
            f.ret(s);
        })
    }
}

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns `jade.time.local(tz) -> str`: now, as
    /// `date '+%a %b %e %H:%M:%S %Z %Y'` prints it, in the zone `tz` names or
    /// the system's when it is nil. v1 ran `date`; this formats in process,
    /// setting `TZ` for the moment it takes and putting it back, under a lock
    /// since the environment is the whole program's. The text is tainted, as
    /// v1's was.
    pub fn time_local_fn(&self) -> FunctionValue<'ctx> {
        let p = self.ptrt();
        self.support("jade.time.local", Some(p.into()), &[p.into()], |f| {
            let tz = f.param_ptr(0);
            let named = f.not(f.is_null(tz));
            let lock = f.cx.time_lock();
            f.spin_lock(lock);
            let saved = f.var(f.null());
            f.if_(named, |f| {
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
            let now = f.alloca(f.cx.i64t());
            f.store(now, f.clock(false, 1));
            let tm = f.alloca(f.cx.i8t().array_type(128));
            f.call(f.cx.libc("localtime_r"), &[now.into(), tm.into()]);
            let buf = f.alloca(f.cx.i8t().array_type(128));
            let n = f.call_int(
                f.cx.libc("strftime"),
                &[
                    buf.into(),
                    f.i64(128).into(),
                    f.cx.cstr("%a %b %e %H:%M:%S %Z %Y").into(),
                    tm.into(),
                ],
            );
            f.if_(named, |f| {
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
            f.spin_unlock(lock);
            f.ret(f.str_from_trust(buf, n, f.i64(1)));
        })
    }

    /// Purpose: This function returns the lock `time.local` holds while `TZ`
    /// is changed.
    fn time_lock(&self) -> inkwell::values::PointerValue<'ctx> {
        if let Some(g) = self.module.get_global("jade.time.lock") {
            return g.as_pointer_value();
        }
        let g = self.module.add_global(self.i64t(), None, "jade.time.lock");
        g.set_initializer(&self.i64t().const_zero());
        g.set_linkage(inkwell::module::Linkage::Internal);
        g.as_pointer_value()
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function takes a spin lock held in an i64 word.
    pub fn spin_lock(&self, lock: inkwell::values::PointerValue<'ctx>) {
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
    pub fn spin_unlock(&self, lock: inkwell::values::PointerValue<'ctx>) {
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

    /// Purpose: This function divides, rounding toward negative infinity, by a
    /// positive constant.
    fn floor_div(&self, a: IntValue<'ctx>, b: i64) -> IntValue<'ctx> {
        let q = self.sdiv(a, self.i64(b));
        let r = self.srem(a, self.i64(b));
        self.select_int(self.slt(r, self.i64(0)), self.sub(q, self.i64(1)), q)
    }

    /// Purpose: This function returns the remainder that `floor_div` leaves,
    /// which is never negative.
    fn floor_rem(&self, a: IntValue<'ctx>, b: i64) -> IntValue<'ctx> {
        let r = self.srem(a, self.i64(b));
        self.select_int(self.slt(r, self.i64(0)), self.add(r, self.i64(b)), r)
    }

    /// Purpose: This function adds or subtracts, saturating at the int range.
    fn sat(&self, op: &str, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        let name = format!("llvm.{op}.sat");
        let intr = inkwell::intrinsics::Intrinsic::find(&name).expect("intrinsic");
        let decl =
            intr.get_declaration(&self.cx.module, &[self.cx.i64t().into()]).expect("declaration");
        self.call_int(decl, &[a.into(), b.into()])
    }

    /// Purpose: This function multiplies by a positive constant, saturating.
    fn sat_mul(&self, a: IntValue<'ctx>, b: i64) -> IntValue<'ctx> {
        let (r, o) = self.checked("smul", a, self.i64(b));
        let edge =
            self.select_int(self.slt(a, self.i64(0)), self.i64(i64::MIN), self.i64(i64::MAX));
        self.select_int(o, edge, r)
    }

    /// Purpose: This function turns days since 1970-01-01 into a year, a month
    /// and a day.
    fn civil_from_days(
        &self,
        z: IntValue<'ctx>,
    ) -> (IntValue<'ctx>, IntValue<'ctx>, IntValue<'ctx>) {
        let i = |v: i64| self.i64(v);
        let z = self.add(z, i(719_468));
        let era =
            self.sdiv(self.select_int(self.sge(z, i(0)), z, self.sub(z, i(146_096))), i(146_097));
        let doe = self.sub(z, self.mul(era, i(146_097)));
        let yoe = self.sdiv(
            self.sub(
                self.add(self.sub(doe, self.sdiv(doe, i(1460))), self.sdiv(doe, i(36_524))),
                self.sdiv(doe, i(146_096)),
            ),
            i(365),
        );
        let y = self.add(yoe, self.mul(era, i(400)));
        let doy = self.sub(
            doe,
            self.sub(self.add(self.mul(i(365), yoe), self.sdiv(yoe, i(4))), self.sdiv(yoe, i(100))),
        );
        let mp = self.sdiv(self.add(self.mul(i(5), doy), i(2)), i(153));
        let d =
            self.add(self.sub(doy, self.sdiv(self.add(self.mul(i(153), mp), i(2)), i(5))), i(1));
        let m = self.select_int(self.slt(mp, i(10)), self.add(mp, i(3)), self.sub(mp, i(9)));
        let y = self.select_int(self.sle(m, i(2)), self.add(y, i(1)), y);
        (y, m, d)
    }
}
