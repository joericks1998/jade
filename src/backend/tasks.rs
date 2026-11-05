//! Tasks: `async fn`, `await`, `join`, `wait`, `cancel` and the concurrency
//! limit.
//!
//! A call to an async function makes a *job*, which is also the future the call
//! returns, and puts it on one queue. Worker threads, made as they are needed
//! up to `max_tasks()`, take jobs off the queue and run them. One mutex and one
//! condition variable guard the queue and every job's state, and a job that
//! finishes wakes everyone waiting, who each look again at what they wait for.
//!
//! Two rules keep this from deadlocking or flooding the machine with threads:
//!
//! - *Awaiting a job that has not started runs it on the awaiting thread.* A
//!   task that awaits the task it just made therefore never waits for a free
//!   worker, and a chain of 2,000 tasks each awaiting the next runs on one
//!   thread rather than 2,000.
//! - *A job holds its worker while it sleeps*, so `max_tasks` limits how many
//!   tasks are in flight, which is what v1 measured.
//!
//! A job is `{ i64 rc, i64 state, i64 cancel, ptr err, ptr next, ptr entry,
//! ptr args, ptr free, T value }`. Its state is queued, running, done or
//! failed. Its entry is a function generated for the async function it runs,
//! which unpacks the arguments, calls it, and records the value or the error.
//! Its free function is generated per result type, so code that holds a future
//! without knowing what it resolves to, such as the queue, can still release
//! it.

use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use super::layout::{ARR_DATA, LEN, TYPE_ID, zero};
use super::{Cx, F};
use crate::mir::{Ty, is_heap};

/// Byte offsets in a job.
pub const STATE: i64 = 8;
pub const CANCEL: i64 = 16;
pub const ERR: i64 = 24;
pub const NEXT: i64 = 32;
pub const ENTRY: i64 = 40;
pub const ARGS: i64 = 48;
pub const FREE: i64 = 56;
/// The value's field index in a job's LLVM struct.
const VALUE: u32 = 8;

/// A job's states.
pub const QUEUED: i64 = 0;
pub const RUNNING: i64 = 1;
pub const DONE: i64 = 2;
pub const FAILED: i64 = 3;

/// The concurrency limit a program starts with, and its bounds.
const DEFAULT_LIMIT: i64 = 32;
const MAX_LIMIT: i64 = 512;

/// A worker thread's stack, which an awaited chain of tasks may run deep on.
const WORKER_STACK: i64 = 256 * 1024 * 1024;

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns a job's LLVM type for a result type.
    pub fn job_ty(&self, t: &Ty) -> StructType<'ctx> {
        let (i, p) = (self.i64t(), self.ptrt());
        self.ctx.struct_type(
            &[
                i.into(),
                i.into(),
                i.into(),
                p.into(),
                p.into(),
                p.into(),
                p.into(),
                p.into(),
                self.llty(t),
            ],
            false,
        )
    }

    /// Purpose: This function returns a process-wide global, made the first time.
    fn task_global(
        &self,
        name: &str,
        ty: BasicTypeEnum<'ctx>,
        init: BasicValueEnum<'ctx>,
    ) -> PointerValue<'ctx> {
        if let Some(g) = self.module.get_global(name) {
            return g.as_pointer_value();
        }
        let g = self.module.add_global(ty, None, name);
        g.set_initializer(&init);
        g.set_linkage(inkwell::module::Linkage::Internal);
        g.set_alignment(16);
        g.as_pointer_value()
    }

    fn task_mutex(&self) -> PointerValue<'ctx> {
        let t = self.i8t().array_type(64);
        self.task_global("jade.task.mutex", t.into(), t.const_zero().into())
    }

    fn task_cond(&self) -> PointerValue<'ctx> {
        let t = self.i8t().array_type(64);
        self.task_global("jade.task.cond", t.into(), t.const_zero().into())
    }

    fn task_counter(&self, name: &str, init: i64) -> PointerValue<'ctx> {
        self.task_global(name, self.i64t().into(), self.i64t().const_int(init as u64, true).into())
    }

    fn task_ptr(&self, name: &str) -> PointerValue<'ctx> {
        self.task_global(name, self.ptrt().into(), self.ptrt().const_null().into())
    }

    /// Purpose: This function returns the thread-local slot naming the job the
    /// thread is running, for `cancelled()`.
    fn task_current(&self) -> PointerValue<'ctx> {
        if let Some(g) = self.module.get_global("jade.task.current") {
            return g.as_pointer_value();
        }
        let g = self.module.add_global(self.ptrt(), None, "jade.task.current");
        g.set_initializer(&self.ptrt().const_null());
        g.set_thread_local(true);
        g.set_linkage(inkwell::module::Linkage::Internal);
        g.as_pointer_value()
    }

    /// Purpose: This function returns `jade.task.init()`, which sets up the
    /// lock and condition variable before the program runs.
    pub fn task_init_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.task.init", None, &[], |f| {
            f.call(f.cx.libc("pthread_mutex_init"), &[f.cx.task_mutex().into(), f.null().into()]);
            f.call(f.cx.libc("pthread_cond_init"), &[f.cx.task_cond().into(), f.null().into()]);
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.task.run(job)`, which runs a job on
    /// the calling thread, recording it as the thread's current job meanwhile.
    fn task_run_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.task.run", None, &[self.ptrt().into()], |f| {
            let job = f.param_ptr(0);
            let cur = f.cx.task_current();
            let prev = f.load_ptr(cur);
            f.store(cur, job);
            let entry = f.load_ptr(f.at_c(job, ENTRY));
            let args = f.load_ptr(f.at_c(job, ARGS));
            let ty = f.cx.ctx.void_type().fn_type(&[f.cx.ptrt().into(), f.cx.ptrt().into()], false);
            f.b.build_indirect_call(ty, entry, &[job.into(), args.into()], "").expect("positioned");
            f.store(cur, prev);
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.task.worker(_)`, a worker thread's
    /// body: take a job when the limit allows, run it, and repeat.
    fn task_worker_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        self.support("jade.task.worker", Some(ptr.into()), &[ptr.into()], |f| {
            let head = f.cx.task_ptr("jade.task.head");
            let tail = f.cx.task_ptr("jade.task.tail");
            let running = f.cx.task_counter("jade.task.running", 0);
            let idle = f.cx.task_counter("jade.task.idle", 0);
            let limit = f.cx.task_counter("jade.task.limit", DEFAULT_LIMIT);
            f.while_(
                |f| f.bool(true),
                |f, _| {
                    f.lock();
                    f.while_(
                        |f| {
                            let empty = f.is_null(f.load_ptr(head));
                            let full = f.sge(f.load_i64(running), f.load_i64(limit));
                            f.or(empty, full)
                        },
                        |f, _| {
                            f.store(idle, f.add(f.load_i64(idle), f.i64(1)));
                            f.call(
                                f.cx.libc("pthread_cond_wait"),
                                &[f.cx.task_cond().into(), f.cx.task_mutex().into()],
                            );
                            f.store(idle, f.sub(f.load_i64(idle), f.i64(1)));
                        },
                    );
                    let queued_n = f.cx.task_counter("jade.task.queued", 0);
                    f.store(queued_n, f.sub(f.load_i64(queued_n), f.i64(1)));
                    let job = f.load_ptr(head);
                    let next = f.load_ptr(f.at_c(job, NEXT));
                    f.store(head, next);
                    f.if_(f.is_null(next), |f| f.store(tail, f.null()));
                    // An awaiter may have claimed it already and run it itself.
                    let state_p = f.at_c(job, STATE);
                    let queued = f.eq(f.load_i64(state_p), f.i64(QUEUED));
                    f.if_else(
                        queued,
                        |f| {
                            f.store(state_p, f.i64(RUNNING));
                            f.store(running, f.add(f.load_i64(running), f.i64(1)));
                            f.unlock();
                            f.call(f.cx.task_run_fn(), &[job.into()]);
                            f.lock();
                            f.store(running, f.sub(f.load_i64(running), f.i64(1)));
                            f.broadcast();
                            f.unlock();
                        },
                        |f| f.unlock(),
                    );
                    // The queue's reference.
                    f.release(&Ty::Future(Box::new(Ty::Nil)), job.into());
                },
            );
            f.ret(f.null());
        })
    }

    /// Purpose: This function returns `jade.task.submit(job)`, which queues a
    /// job and makes a worker when none is free and the limit allows one.
    fn task_submit_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.task.submit", None, &[self.ptrt().into()], |f| {
            let job = f.param_ptr(0);
            let head = f.cx.task_ptr("jade.task.head");
            let tail = f.cx.task_ptr("jade.task.tail");
            let workers = f.cx.task_counter("jade.task.workers", 0);
            let idle = f.cx.task_counter("jade.task.idle", 0);
            let limit = f.cx.task_counter("jade.task.limit", DEFAULT_LIMIT);
            f.lock();
            let t = f.load_ptr(tail);
            f.if_else(f.is_null(t), |f| f.store(head, job), |f| f.store(f.at_c(t, NEXT), job));
            f.store(tail, job);
            // A worker is made when more jobs wait than workers are idle, since
            // an idle worker woken now may not take its job for a while.
            let queued = f.cx.task_counter("jade.task.queued", 0);
            let waiting = f.add(f.load_i64(queued), f.i64(1));
            f.store(queued, waiting);
            let none_free = f.sgt(waiting, f.load_i64(idle));
            let room = f.slt(f.load_i64(workers), f.load_i64(limit));
            f.if_(f.and(none_free, room), |f| {
                f.store(workers, f.add(f.load_i64(workers), f.i64(1)));
                let attr = f.alloca(f.cx.i8t().array_type(128));
                let thread = f.alloca(f.cx.i8t().array_type(16));
                f.call(f.cx.libc("pthread_attr_init"), &[attr.into()]);
                f.call(
                    f.cx.libc("pthread_attr_setstacksize"),
                    &[attr.into(), f.i64(WORKER_STACK).into()],
                );
                let worker = f.cx.task_worker_fn().as_global_value().as_pointer_value();
                f.call(
                    f.cx.libc("pthread_create"),
                    &[thread.into(), attr.into(), worker.into(), f.null().into()],
                );
                f.call(f.cx.libc("pthread_detach"), &[f.load_ptr(thread).into()]);
                f.call(f.cx.libc("pthread_attr_destroy"), &[attr.into()]);
            });
            f.broadcast();
            f.unlock();
            f.ret_void();
        })
    }

    /// Purpose: This function returns the entry a job runs for an async
    /// function: unpack the arguments, call the function, record the outcome.
    ///
    /// Inputs:
    /// - `target`: the async function's body, which returns its value.
    /// - `params`: its parameter types.
    /// - `ret`: its return type.
    ///
    /// Outputs: `jade.task.entry.<name>(job, args)`.
    pub fn task_entry_fn(
        &self,
        target: FunctionValue<'ctx>,
        params: &[Ty],
        ret: &Ty,
    ) -> FunctionValue<'ctx> {
        let name = format!("jade.task.entry.{}", target.get_name().to_string_lossy());
        let ptr = self.ptrt();
        let params = params.to_vec();
        let ret = ret.clone();
        self.support(&name, None, &[ptr.into(), ptr.into()], |f| {
            let (job, args) = (f.param_ptr(0), f.param_ptr(1));
            let at = f.cx.args_ty(&params);
            let vals: Vec<BasicValueEnum> = (0..params.len())
                .map(|i| f.load(f.cx.llty(&params[i]), f.field(at, args, i as u32)))
                .collect();
            let jt = f.cx.job_ty(&ret);
            // A task cancelled before it started does not start.
            let cancelled = f.ne(f.load_i64_atomic(f.at_c(job, CANCEL)), f.i64(0));
            f.if_else(
                cancelled,
                |f| {
                    f.lock();
                    f.store(f.at_c(job, STATE), f.i64(FAILED));
                },
                |f| {
                    let v = f.call(target, &vals);
                    f.lock();
                    f.if_else(
                        f.err_pending(),
                        |f| {
                            let slot = f.cx.err_slot();
                            f.store(f.at_c(job, ERR), f.load_ptr(slot));
                            f.store(slot, f.null());
                            f.store(f.at_c(job, STATE), f.i64(FAILED));
                        },
                        |f| {
                            if let Some(v) = v {
                                f.store(f.field(jt, job, VALUE), v);
                            }
                            f.store(f.at_c(job, STATE), f.i64(DONE));
                        },
                    );
                },
            );
            f.broadcast();
            f.unlock();
            for (i, p) in params.iter().enumerate() {
                if is_heap(p) {
                    f.release(p, vals[i]);
                }
            }
            f.call(f.cx.libc("free"), &[args.into()]);
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.task.free.<T>(job)`, which frees a
    /// job whose count reached zero, releasing its value or its error.
    pub fn task_free_fn(&self, t: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.task.free.{}", super::rc::key(t));
        let t = t.clone();
        self.support(&name, None, &[self.ptrt().into()], |f| {
            let job = f.param_ptr(0);
            let e = f.load_ptr(f.at_c(job, ERR));
            f.release(&Ty::Error, e.into());
            if is_heap(&t) {
                let done = f.eq(f.load_i64(f.at_c(job, STATE)), f.i64(DONE));
                f.if_(done, |f| {
                    let jt = f.cx.job_ty(&t);
                    let v = f.load(f.cx.llty(&t), f.field(jt, job, VALUE));
                    f.release(&t, v);
                });
            }
            f.call(f.cx.libc("free"), &[job.into()]);
            f.ret_void();
        })
    }

    /// Purpose: This function returns the struct type a job's arguments are
    /// packed in.
    pub fn args_ty(&self, params: &[Ty]) -> StructType<'ctx> {
        let fields: Vec<BasicTypeEnum> = params.iter().map(|p| self.llty(p)).collect();
        // An empty struct still needs a byte, so `malloc` has a size to allocate.
        if fields.is_empty() {
            return self.ctx.struct_type(&[self.i8t().into()], false);
        }
        self.ctx.struct_type(&fields, false)
    }

    /// Purpose: This function returns `jade.task.await.<T>(job) -> T`.
    pub fn await_fn(&self, t: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.task.await.{}", super::rc::key(t));
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
        let name = format!("jade.task.join.{}", super::rc::key(t));
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

    /// Purpose: This function returns `jade.task.sleep(secs)`.
    pub fn sleep_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.task.sleep", None, &[self.f64t().into()], |f| {
            let secs = f.param(0).into_float_value();
            let ts =
                f.alloca(f.cx.ctx.struct_type(&[f.cx.i64t().into(), f.cx.i64t().into()], false));
            let whole = f.ftoi(secs);
            let frac = f.fsub(secs, f.itof(whole));
            let nanos = f.ftoi(f.fmul(frac, f.f64(1e9)));
            let ok = f.sge(whole, f.i64(0));
            f.if_(ok, |f| {
                f.store(ts, whole);
                f.store(f.at_c(ts, 8), nanos);
                f.call(f.cx.libc("nanosleep"), &[ts.into(), f.null().into()]);
            });
            f.ret_void();
        })
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
    pub fn lock(&self) {
        self.call(self.cx.libc("pthread_mutex_lock"), &[self.cx.task_mutex().into()]);
    }

    pub fn unlock(&self) {
        self.call(self.cx.libc("pthread_mutex_unlock"), &[self.cx.task_mutex().into()]);
    }

    pub fn broadcast(&self) {
        self.call(self.cx.libc("pthread_cond_broadcast"), &[self.cx.task_cond().into()]);
    }

    /// Purpose: This function makes a job and queues it.
    ///
    /// Inputs:
    /// - `ret`: its result type.
    /// - `entry`: the function it runs.
    /// - `args`: its packed arguments, which it frees.
    ///
    /// Outputs: the job, with one reference for the caller.
    pub fn new_job(
        &self,
        ret: &Ty,
        entry: FunctionValue<'ctx>,
        args: PointerValue<'ctx>,
    ) -> PointerValue<'ctx> {
        let jt = self.cx.job_ty(ret);
        let size = self.size_of(jt.into());
        let job = self.call_ptr(self.cx.libc("malloc"), &[size.into()]);
        // One reference for the caller and one for the queue.
        self.store(job, self.i64(2));
        self.store(self.at_c(job, STATE), self.i64(QUEUED));
        self.store(self.at_c(job, CANCEL), self.i64(0));
        self.store(self.at_c(job, ERR), self.null());
        self.store(self.at_c(job, NEXT), self.null());
        self.store(self.at_c(job, ENTRY), entry.as_global_value().as_pointer_value());
        self.store(self.at_c(job, ARGS), args);
        let free = self.cx.task_free_fn(ret).as_global_value().as_pointer_value();
        self.store(self.at_c(job, FREE), free);
        self.store(self.field(jt, job, VALUE), zero(self.cx.llty(ret)));
        self.call(self.cx.task_submit_fn(), &[job.into()]);
        job
    }

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

    /// Purpose: This function says whether the running task was cancelled.
    pub fn cancelled(&self) -> IntValue<'ctx> {
        let cur = self.load_ptr(self.cx.task_current());
        let flag = self.var(self.bool(false));
        self.if_(self.not(self.is_null(cur)), |f| {
            f.store(flag, f.ne(f.load_i64(f.at_c(cur, CANCEL)), f.i64(0)));
        });
        self.load(self.cx.i1t(), flag).into_int_value()
    }

    /// Purpose: This function sets the concurrency limit, clamped, and returns it.
    pub fn set_max_tasks(&self, n: IntValue<'ctx>) -> IntValue<'ctx> {
        let lo = self.select_int(self.slt(n, self.i64(1)), self.i64(1), n);
        let v = self.select_int(self.sgt(lo, self.i64(MAX_LIMIT)), self.i64(MAX_LIMIT), lo);
        self.lock();
        self.store(self.cx.task_counter("jade.task.limit", DEFAULT_LIMIT), v);
        self.broadcast();
        self.unlock();
        v
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
    pub fn monotonic(&self) -> inkwell::values::FloatValue<'ctx> {
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

    /// Purpose: This function reads the concurrency limit.
    pub fn max_tasks(&self) -> IntValue<'ctx> {
        self.load_i64(self.cx.task_counter("jade.task.limit", DEFAULT_LIMIT))
    }
}
