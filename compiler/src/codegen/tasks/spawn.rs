//! Starting tasks: a call to an async function makes a job and puts it on
//! the queue, and worker threads, made as they are needed up to
//! `max_tasks()`, take jobs off it and run them. `entry` is the function
//! generated for each async function a job runs: it unpacks the arguments,
//! calls it, and records the value or the error.

use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};

use super::{
    ARGS, CANCEL, DEFAULT_LIMIT, DONE, ENTRY, ERR, FAILED, FREE, NEXT, QUEUED, RUNNING, STATE,
    VALUE, WORKER_STACK,
};
use crate::codegen::memory::layout::zero;
use crate::codegen::{Cx, F};
use crate::mir::{Ty, is_heap};

impl<'ctx> Cx<'ctx> {
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
    pub(super) fn task_run_fn(&self) -> FunctionValue<'ctx> {
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
}

impl<'a, 'ctx> F<'a, 'ctx> {
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
}
