//! Waiting on tasks: `await`, `join`, `join_settled`, `wait` and `after`.
//! Awaiting a job that has not started runs it on the awaiting thread, so a
//! task that awaits the task it just made never waits for a free worker.

use inkwell::values::{FunctionValue, PointerValue};

use super::{CANCEL, DONE, ERR, FAILED, QUEUED, RUNNING, STATE, VALUE};
use crate::codegen::memory::layout::{ARR_DATA, LEN, TYPE_ID, zero};
use crate::codegen::{Cx, F};
use crate::mir::Ty;

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns `jade.task.await.<T>(job) -> T`.
    pub fn await_fn(&self, t: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.task.await.{}", crate::codegen::memory::rc::key(t));
        let t = t.clone();
        self.support(&name, Some(self.llty(&t)), &[self.ptrt().into()], |f| {
            let job = f.param_ptr(0);
            f.settle(job, true);
            f.if_(f.err_pending(), |f| f.ret_zero());
            let jt = f.cx.job_ty(&t);
            let v = f.load(f.cx.llty(&t), f.field(jt, job, VALUE));
            f.retain(&t, v);
            f.ret(v);
        })
    }

    /// Purpose: This function returns `jade.task.join.<T>(jobs, n) -> [T]`:
    /// each job awaited in order, raising the first error.
    pub fn join_fn(&self, t: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.task.join.{}", crate::codegen::memory::rc::key(t));
        let t = t.clone();
        let ptr = self.ptrt();
        self.support(&name, Some(ptr.into()), &[ptr.into(), self.i64t().into()], |f| {
            let (jobs, n) = (f.param_ptr(0), f.param_int(1));
            let arr = f.array_new(&t, n);
            let data = f.load_ptr(f.at_c(arr, ARR_DATA));
            let et = f.cx.llty(&t);
            f.for_range(f.i64(0), n, |f, i, _| {
                let job = f.load_ptr(f.elem(f.cx.ptrt(), jobs, i));
                let v = f.callv(f.cx.await_fn(&t), &[job.into()]);
                f.if_(f.err_pending(), |f| {
                    f.release(&Ty::Array(Box::new(t.clone())), arr.into());
                    f.ret_zero();
                });
                f.store(f.elem(et, data, i), v);
                f.store(f.at_c(arr, LEN), f.add(i, f.i64(1)));
            });
            f.ret(arr);
        })
    }

    /// Purpose: This function returns `jade.task.join_settled.<S>(jobs, n) ->
    /// [S]`: each job awaited in order, its outcome recorded in a `Settled`
    /// class value `S { ok, value, error }` rather than raised.
    pub fn join_settled_fn(&self, t: &Ty, class: &str) -> FunctionValue<'ctx> {
        let name = format!("jade.task.join_settled.{class}");
        let t = t.clone();
        let class = class.to_string();
        let ptr = self.ptrt();
        self.support(&name, Some(ptr.into()), &[ptr.into(), self.i64t().into()], |f| {
            let (jobs, n) = (f.param_ptr(0), f.param_int(1));
            let st_t = Ty::Struct(class.clone());
            let arr = f.array_new(&st_t, n);
            let data = f.load_ptr(f.at_c(arr, ARR_DATA));
            let st = f.cx.struct_ty(&class);
            let fields = f.cx.struct_fields(&class);
            let vt = fields[1].clone();
            let jt = f.cx.job_ty(&t);
            f.for_range(f.i64(0), n, |f, i, _| {
                let job = f.load_ptr(f.elem(f.cx.ptrt(), jobs, i));
                f.settle(job, false);
                let state = f.load_i64(f.at_c(job, STATE));
                let cancel = f.load_i64(f.at_c(job, CANCEL));
                let ok = f.and(f.eq(state, f.i64(DONE)), f.eq(cancel, f.i64(0)));
                let size = f.size_of(st.into());
                let s = f.call_ptr(f.cx.libc("malloc"), &[size.into()]);
                f.store(s, f.i64(1));
                f.store(f.at_c(s, TYPE_ID), f.i64(f.cx.struct_id(&class) as i64));
                f.store(f.field(st, s, 2), ok);
                f.store(f.field(st, s, 3), zero(f.cx.llty(&vt)));
                f.if_(ok, |f| {
                    let v = f.load(f.cx.llty(&t), f.field(jt, job, VALUE));
                    f.retain(&t, v);
                    f.store(f.field(st, s, 3), f.wrap(&vt, v));
                });
                // A job that succeeded has no error, so this is null or the
                // failure's error.
                let e = f.load_ptr(f.at_c(job, ERR));
                f.retain(&Ty::Error, e.into());
                f.store(f.field(st, s, 4), e);
                f.store(f.elem(f.cx.ptrt(), data, i), s);
                f.store(f.at_c(arr, LEN), f.add(i, f.i64(1)));
            });
            f.ret(arr);
        })
    }

    /// Purpose: This function returns `jade.task.wait(jobs, n) -> int`: the
    /// lowest index of a finished job, waiting until there is one.
    pub fn wait_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support(
            "jade.task.wait",
            Some(self.i64t().into()),
            &[ptr.into(), self.i64t().into()],
            |f| {
                let (jobs, n) = (f.param_ptr(0), f.param_int(1));
                f.lock();
                f.while_(
                    |f| f.bool(true),
                    |f, _| {
                        f.for_range(f.i64(0), n, |f, i, _| {
                            let job = f.load_ptr(f.elem(f.cx.ptrt(), jobs, i));
                            let st = f.load_i64(f.at_c(job, STATE));
                            f.if_(f.sge(st, f.i64(DONE)), |f| {
                                f.unlock();
                                f.ret(i);
                            });
                        });
                        f.call(
                            f.cx.libc("pthread_cond_wait"),
                            &[f.cx.task_cond().into(), f.cx.task_mutex().into()],
                        );
                    },
                );
                f.ret(f.i64(-1));
            },
        )
    }

    /// Purpose: This function returns `jade.task.after(secs) -> future of nil`:
    /// a job that only sleeps.
    pub fn after_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.task.after", Some(self.ptrt().into()), &[self.f64t().into()], |f| {
            let secs = f.param(0);
            let args = f.call_ptr(f.cx.libc("malloc"), &[f.i64(8).into()]);
            f.store(args, secs);
            let entry = f.cx.support(
                "jade.task.entry.after",
                None,
                &[f.cx.ptrt().into(), f.cx.ptrt().into()],
                |g| {
                    let (job, a) = (g.param_ptr(0), g.param_ptr(1));
                    g.call(g.cx.sleep_fn(), &[g.load_f64(a).into()]);
                    g.call(g.cx.libc("free"), &[a.into()]);
                    g.lock();
                    g.store(g.at_c(job, STATE), g.i64(DONE));
                    g.broadcast();
                    g.unlock();
                    g.ret_void();
                },
            );
            let job = f.new_job(&Ty::Nil, entry, args);
            f.ret(job);
        })
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function waits for a job to finish, running it here if it
    /// has not started, and with `raise` its error is raised.
    ///
    /// Inputs:
    /// - `job`: the job.
    /// - `raise`: true to raise a failure or a cancellation.
    ///
    /// Outputs: none.
    pub fn settle(&self, job: PointerValue<'ctx>, raise: bool) {
        let state_p = self.at_c(job, STATE);
        self.lock();
        if raise {
            let cancelled = self.ne(self.load_i64(self.at_c(job, CANCEL)), self.i64(0));
            self.if_(cancelled, |f| {
                f.unlock();
                f.raise("awaiting a cancelled task");
                f.ret_zero();
            });
        }
        let queued = self.eq(self.load_i64(state_p), self.i64(QUEUED));
        self.if_(queued, |f| {
            f.store(state_p, f.i64(RUNNING));
            f.unlock();
            f.call(f.cx.task_run_fn(), &[job.into()]);
            f.lock();
        });
        self.while_(
            |f| f.slt(f.load_i64(state_p), f.i64(DONE)),
            |f, _| {
                f.call(
                    f.cx.libc("pthread_cond_wait"),
                    &[f.cx.task_cond().into(), f.cx.task_mutex().into()],
                );
            },
        );
        self.unlock();
        if raise {
            let failed = self.eq(self.load_i64(state_p), self.i64(FAILED));
            self.if_(failed, |f| {
                let e = f.load_ptr(f.at_c(job, ERR));
                f.retain(&Ty::Error, e.into());
                f.call(f.cx.set_err_fn(), &[e.into()]);
            });
        }
    }
}
