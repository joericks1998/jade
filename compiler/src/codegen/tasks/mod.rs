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
//!
//! `spawn.rs` makes and queues jobs and runs the worker threads; `wait.rs` has
//! `await`, `join`, `wait` and `after`. This file has the job's layout, the
//! lock, cancelling and the concurrency limit.

mod spawn;
mod wait;

use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{FunctionValue, IntValue, PointerValue};

use crate::codegen::libcache::shared::{Slot, State};
use crate::codegen::{Cx, F};
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

    fn task_mutex(&self) -> PointerValue<'ctx> {
        self.state("jade.task.mutex", State::aligned(Slot::Bytes(64))).as_pointer_value()
    }

    fn task_cond(&self) -> PointerValue<'ctx> {
        self.state("jade.task.cond", State::aligned(Slot::Bytes(64))).as_pointer_value()
    }

    fn task_counter(&self, name: &str, init: i64) -> PointerValue<'ctx> {
        self.state(name, State::aligned(Slot::Word(init))).as_pointer_value()
    }

    fn task_ptr(&self, name: &str) -> PointerValue<'ctx> {
        self.state(name, State::aligned(Slot::Ptr)).as_pointer_value()
    }

    /// Purpose: This function returns the thread-local slot naming the job the
    /// thread is running, for `cancelled()`.
    fn task_current(&self) -> PointerValue<'ctx> {
        self.state("jade.task.current", State::thread(Slot::Ptr)).as_pointer_value()
    }

    /// Purpose: This function returns `jade.task.free.<T>(job)`, which frees a
    /// job whose count reached zero, releasing its value or its error.
    pub fn task_free_fn(&self, t: &Ty) -> FunctionValue<'ctx> {
        let name = format!("jade.task.free.{}", crate::codegen::memory::rc::key(t));
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

    /// Purpose: This function reads the concurrency limit.
    pub fn max_tasks(&self) -> IntValue<'ctx> {
        self.load_i64(self.cx.task_counter("jade.task.limit", DEFAULT_LIMIT))
    }
}
