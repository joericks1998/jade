//! Async tasks: a bounded worker pool and the future object they resolve.
//!
//! This is the concurrency layer that used to live in `runtime_aot/posix.c` as
//! one detached pthread per spawn. Two things were wrong with that. It was
//! unbounded — Jade's advertised pattern is fan-out over N prompts, and
//! `pthread_create` per prompt turns a large fan-out into a resource failure
//! instead of a queue. And the future it returned carried no [`ObjHeader`], so
//! it could not participate in refcounting, which is why `program_collections_only`
//! disables refcounting for *any* program containing `Spawn`.
//!
//! ## What stays in C
//!
//! Exactly one thing: the `setjmp` frame around the task body. Jade exceptions
//! are `setjmp`/`longjmp` over a `_Thread_local` stack (`jade_exc_push_frame`
//! and friends in `common.c`), and Rust cannot call `setjmp` — a `longjmp` past
//! a Rust frame is undefined behavior regardless. So a task body runs inside a
//! C shim that catches the jump and reports `(result, error, error_type)` back
//! through out-params; this module never sees a `longjmp`.
//!
//! The shim is reached through a registered function pointer rather than a
//! direct `extern "C"` call, because `jade-runtime` is linked standalone by its
//! own unit tests, where no C runtime exists. The default invoker calls the task
//! body directly with no exception frame, which is exactly right for a test that
//! spawns a Rust closure and never raises. AOT binaries register the real shim.
//!
//! ## Why the pool grows when a worker blocks
//!
//! A fixed-size pool deadlocks on Jade's own examples:
//!
//! ```jade
//! async fn double_then_increment(x) {
//!     let d = await double(x)      // a task blocking on another task
//!     return await increment(d)
//! }
//! ```
//!
//! With N workers and N tasks all blocked in `await` on tasks still sitting in
//! the queue, nothing can ever run. Thread-per-spawn could not deadlock this
//! way, which is why it was never hit. The fix is the standard one: a worker
//! about to block announces it, and the pool spawns a replacement if there is
//! queued work and no idle worker to take it. Blocked workers do not count
//! against the target, so the bound constrains *runnable* concurrency — the
//! thing that actually costs CPU — while `await` chains stay deadlock-free.
//!
//! A hard ceiling still applies, so a pathological program fails with a clear
//! error rather than exhausting the process's thread limit.

use core::ffi::{c_char, c_void};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

use crate::gc;
use crate::heap::{ObjHeader, ObjKind};
use crate::value::JadeValue;

/// A compiled task body: `jf_task_<uid>(args, n_args) -> result`.
pub type TaskFn = extern "C" fn(*mut i64, i32) -> i64;

/// The C shim that runs a task body inside a `setjmp` frame.
///
/// Returns nonzero if the body raised. On a raise, `out_err` receives the thrown
/// value and `out_type` the struct type name (or null), so the awaiting thread
/// can re-raise with the type intact and typed `catch` arms still match.
pub type TaskInvoker = unsafe extern "C" fn(
    f: TaskFn,
    args: *mut i64,
    n: i32,
    out_result: *mut i64,
    out_err: *mut i64,
    out_type: *mut *const c_char,
) -> i32;

/// How long an idle worker waits before retiring, so a burst of blocking
/// compensation threads does not linger for the life of a long-running service.
const IDLE_TIMEOUT: Duration = Duration::from_secs(10);

/// Ceiling on total worker threads, blocked ones included. Far above any
/// reasonable target; this exists so a pathological await graph fails loudly
/// instead of taking the process's thread limit with it.
const HARD_MAX_WORKERS: usize = 512;

// ── The task-body invoker ────────────────────────────────────────────────────

static INVOKER: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());

/// Register the C `setjmp` shim. Called once from the C runtime's constructor;
/// AOT binaries always have it, standalone Rust tests never do.
///
/// # Safety
/// `f` must run the task body inside a valid jump frame and fill the out-params.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_set_task_invoker(f: TaskInvoker) {
    INVOKER.store(f as *mut c_void, Ordering::Release);
}

/// Run a task body, catching a Jade exception if the C shim is registered.
///
/// Without a shim the body is called directly. That is correct for the unit
/// tests here (which never raise) and would be wrong for a real AOT program —
/// which always has the shim, because the same object file defines
/// `jade_rt_exit` and is therefore always pulled in.
fn invoke(f: TaskFn, args: &mut [i64]) -> Result<i64, (i64, *const c_char)> {
    let raw = INVOKER.load(Ordering::Acquire);
    let n = args.len() as i32;
    if raw.is_null() {
        return Ok(f(args.as_mut_ptr(), n));
    }
    let shim: TaskInvoker = unsafe { core::mem::transmute(raw) };
    let mut result = 0i64;
    let mut err = 0i64;
    let mut ty: *const c_char = core::ptr::null();
    let failed = unsafe { shim(f, args.as_mut_ptr(), n, &mut result, &mut err, &mut ty) };
    if failed != 0 { Err((err, ty)) } else { Ok(result) }
}

// ── The future object ────────────────────────────────────────────────────────

/// Why an await failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskError {
    /// The word awaited is not a future.
    NotAFuture,
    /// This future's result was already taken. A future resolves once and is
    /// consumed once, matching the VM, where awaiting `.take()`s the join handle.
    DoubleAwait,
    /// The task body raised; `(value, type_name)` is re-raised by the caller on
    /// the awaiting thread, where the try/catch frame lives.
    Raised(i64, *const c_char),
}

/// Mutable state behind the future's lock.
struct FutState {
    done: bool,
    /// Set once the result has been handed to an awaiter.
    consumed: bool,
    result: i64,
    failed: bool,
    error: i64,
    error_type: *const c_char,
}

/// A handle to an in-flight task.
///
/// `#[repr(C)]` with the header first so codegen's `ObjKind` read at offset 8
/// works exactly as it does for a collection. Everything after the header is
/// Rust-private — C never looks past it.
#[repr(C)]
pub struct FutureObj {
    pub header: ObjHeader,
    state: Mutex<FutState>,
    done_cv: Condvar,
}

// The raw `error_type` is a `'static` string constant from the C runtime, and
// every other field is behind the lock or atomic.
unsafe impl Send for FutureObj {}
unsafe impl Sync for FutureObj {}

impl FutureObj {
    fn new() -> Self {
        FutureObj {
            header: ObjHeader::new(ObjKind::Future, 0),
            state: Mutex::new(FutState {
                done: false,
                consumed: false,
                result: 0,
                failed: false,
                error: 0,
                error_type: core::ptr::null(),
            }),
            done_cv: Condvar::new(),
        }
    }

    /// Publish a completed task's outcome and wake every awaiter.
    fn complete(&self, outcome: Result<i64, (i64, *const c_char)>) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match outcome {
            Ok(v) => st.result = v,
            Err((e, ty)) => {
                st.failed = true;
                st.error = e;
                st.error_type = ty;
            }
        }
        st.done = true;
        drop(st);
        self.done_cv.notify_all();
    }
}

// ── The pool ─────────────────────────────────────────────────────────────────

/// A queued task: body, arguments, and the future to resolve.
struct Job {
    f: TaskFn,
    args: Vec<i64>,
    future: *mut FutureObj,
}

// The future outlives the job (the awaiter holds a reference), and the args are
// owned outright by the job.
unsafe impl Send for Job {}

struct Inner {
    queue: VecDeque<Job>,
    /// Threads currently alive, blocked ones included.
    workers: usize,
    /// Threads parked waiting for work.
    idle: usize,
    /// Threads inside `await`. They hold no CPU, so they do not count against
    /// the target — see the module docs on why a fixed pool deadlocks.
    blocked: usize,
}

pub struct Pool {
    inner: Mutex<Inner>,
    work_cv: Condvar,
    /// Target number of *runnable* workers.
    target: usize,
}

static POOL: OnceLock<Pool> = OnceLock::new();

/// Tasks that failed to start because the hard ceiling was reached. Surfaced by
/// `spawn` as an error rather than silently dropped.
static SPAWN_FAILURES: AtomicUsize = AtomicUsize::new(0);

/// The process-wide pool, sized from `JADE_MAX_TASKS` or the machine's
/// parallelism.
pub fn pool() -> &'static Pool {
    POOL.get_or_init(|| {
        let target = std::env::var("JADE_MAX_TASKS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4));
        Pool {
            inner: Mutex::new(Inner { queue: VecDeque::new(), workers: 0, idle: 0, blocked: 0 }),
            work_cv: Condvar::new(),
            target: target.min(HARD_MAX_WORKERS),
        }
    })
}

impl Pool {
    /// Queue a job, starting a worker if there is nobody free to take it.
    fn submit(&'static self, job: Job) {
        let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        st.queue.push_back(job);
        if self.should_grow(&st) {
            st.workers += 1;
            self.start_worker();
        }
        drop(st);
        self.work_cv.notify_one();
    }

    /// Whether queued work would otherwise sit unclaimed.
    ///
    /// `workers - blocked` is the runnable population; blocked workers are
    /// parked in `await` and cannot pick anything up.
    fn should_grow(&self, st: &Inner) -> bool {
        if st.idle > 0 || st.queue.is_empty() {
            return false;
        }
        let runnable = st.workers - st.blocked;
        runnable < self.target && st.workers < HARD_MAX_WORKERS
    }

    fn start_worker(&'static self) {
        if std::thread::Builder::new()
            .name("jade-task".to_string())
            .spawn(move || self.worker_loop())
            .is_err()
        {
            // Could not get a thread from the OS. Undo the count so a later
            // submit retries rather than believing a worker exists.
            let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            st.workers -= 1;
            SPAWN_FAILURES.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn worker_loop(&'static self) {
        loop {
            let job = {
                let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if let Some(j) = st.queue.pop_front() {
                        break j;
                    }
                    st.idle += 1;
                    let (guard, timeout) = self
                        .work_cv
                        .wait_timeout(st, IDLE_TIMEOUT)
                        .unwrap_or_else(|e| e.into_inner());
                    st = guard;
                    st.idle -= 1;
                    // Retire when idle, so compensation threads spawned during a
                    // burst of blocking do not outlive the burst.
                    if timeout.timed_out() && st.queue.is_empty() {
                        st.workers -= 1;
                        return;
                    }
                }
            };

            let mut args = job.args;
            let outcome = invoke(job.f, &mut args);
            // Safety: `spawn` took a reference on the task's behalf, so the
            // future is guaranteed live here even if every other holder dropped
            // it while the body ran.
            unsafe {
                (*job.future).complete(outcome);
                // Release the task's reference. If nobody awaited and the handle
                // is already gone, this is what reclaims the future.
                if (*job.future).header.decref() {
                    destroy(job.future);
                }
            }
        }
    }

    /// Announce that the calling worker is about to block in `await`.
    ///
    /// Spawns a replacement if that would leave queued work with nobody to run
    /// it. This is what keeps `await` chains from deadlocking a bounded pool.
    fn enter_blocking(&'static self) {
        let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        st.blocked += 1;
        if self.should_grow(&st) {
            st.workers += 1;
            self.start_worker();
        }
    }

    fn exit_blocking(&'static self) {
        let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        st.blocked -= 1;
    }

    /// Current worker-thread count. For tests and diagnostics.
    pub fn worker_count(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).workers
    }
}

// ── Neutral cores ────────────────────────────────────────────────────────────

/// Start `f(args)` on the pool and return a future with one reference held by
/// the caller.
///
/// The returned pointer is `leak_obj`-allocated so the heap instrument counts
/// it, and header-prefixed so it can be refcounted like any other value.
pub fn spawn(f: TaskFn, args: Vec<i64>) -> *mut FutureObj {
    let fut = gc::leak_obj(FutureObj::new()) as *mut FutureObj;
    // Two owners, two references. `ObjHeader::new` starts the count at 1 for the
    // caller; the running task needs its own, because the task writes the result
    // into the future and must not be doing so through a freed allocation.
    //
    // This is not hypothetical now that futures are refcounted: a program that
    // spawns and drops the handle without awaiting would otherwise take the
    // count to zero, free the future, and leave the worker writing into
    // reclaimed memory. The worker releases its reference in `worker_loop`
    // immediately after `complete`.
    unsafe { (*fut).header.incref() };
    pool().submit(Job { f, args, future: fut });
    fut
}

/// Block until `fut` resolves and take its result.
///
/// # Safety
/// `fut` must point at a live [`FutureObj`].
pub unsafe fn await_one(fut: *mut FutureObj) -> Result<i64, TaskError> {
    let f = unsafe { &*fut };

    // Announce the block *before* waiting, so the pool can backfill a worker if
    // this thread is one of its own. Harmless when the caller is the main
    // thread: it just permits one extra worker while the main thread waits.
    let p = pool();
    p.enter_blocking();
    let mut st = f.state.lock().unwrap_or_else(|e| e.into_inner());
    while !st.done {
        st = f.done_cv.wait(st).unwrap_or_else(|e| e.into_inner());
    }
    p.exit_blocking();

    // A future resolves once and is consumed once. The VM enforces the same rule
    // by `.take()`-ing the join handle, so a second await is an error on both
    // engines rather than a silently duplicated result.
    if st.consumed {
        return Err(TaskError::DoubleAwait);
    }
    st.consumed = true;

    if st.failed { Err(TaskError::Raised(st.error, st.error_type)) } else { Ok(st.result) }
}

/// Await every future in argument order, returning the first failure.
///
/// Matches the VM: a failing task does not cancel the others, they run to
/// completion. Awaiting all of them before returning the error keeps that true
/// and stops a still-running task from outliving the future it writes to.
///
/// # Safety
/// Every element of `futs` must point at a live [`FutureObj`].
pub unsafe fn join_all(futs: &[*mut FutureObj]) -> Result<Vec<i64>, TaskError> {
    let mut out = Vec::with_capacity(futs.len());
    let mut first_err = None;
    for &f in futs {
        match unsafe { await_one(f) } {
            Ok(v) => out.push(v),
            Err(e) => {
                out.push(0);
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

/// Release one reference to a future, reclaiming it if that was the last.
///
/// **This is how a holder lets go of a future** — not [`destroy`], which
/// assumes the count already reached zero. A future has two owners: whoever
/// holds the handle, and the running task, which writes the result into it.
/// Freeing on the handle's say-so alone frees it out from under a worker that
/// has completed but not yet released its own reference, and the worker's
/// `decref` then writes into reclaimed memory — which glibc reports as a
/// corrupted heap, far from the cause.
///
/// # Safety
/// `fut` must point at a live [`FutureObj`] the caller holds a reference to,
/// and must not be used afterwards.
pub unsafe fn release(fut: *mut FutureObj) {
    if unsafe { (*fut).header.decref() } {
        unsafe { destroy(fut) };
    }
}

/// Reclaim a future whose refcount reached zero. Called from the collector's
/// `ObjKind::Future` arm and from [`release`], never directly — use [`release`].
///
/// # Safety
/// `fut` must be a live, `rc == 0` [`FutureObj`] from [`spawn`], unreferenced
/// afterwards.
pub unsafe fn destroy(fut: *mut FutureObj) {
    // The precondition was documented and nothing checked it, so callers broke
    // it and the damage surfaced as heap corruption in an unrelated allocation.
    debug_assert_eq!(
        unsafe { (*fut).header.rc() },
        0,
        "destroy on a future with live references — use release(); a worker may \
         still be writing its result into this allocation"
    );
    // A future is not a collection: its result is a single word, not a Vec of
    // children, so there is no cascade. The result word may itself be a
    // collection reference, and dropping an un-awaited future drops that
    // reference with it. `free_leaked` drops the `FutureObj` and returns its block
    // to the pool `leak_obj` took it from (and records the free).
    unsafe { gc::free_leaked(fut) };
}

// ── AOT C-ABI surface ────────────────────────────────────────────────────────
//
// These keep the *shapes* the C implementations had, so `src/codegen/` needs no
// change in this increment: a future is still an opaque pointer that only flows
// to await/join. Making it a tagged, refcounted first-class value — which is
// what finally lets `program_collections_only` stop disabling refcounting for
// async programs — is the next step and touches codegen.
//
// Errors travel back through out-params rather than being thrown here, because
// throwing means `longjmp` and that must happen on the awaiting thread, in C,
// where the jump frame lives. The thin C forwarders do it.

/// Start `f(args)`; returns an opaque future the caller must eventually free.
///
/// # Safety
/// `args` must point at `n` readable words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_spawn(f: TaskFn, args: *const i64, n: i32) -> *mut FutureObj {
    let argv = if n > 0 && !args.is_null() {
        unsafe { core::slice::from_raw_parts(args, n as usize) }.to_vec()
    } else {
        Vec::new()
    };
    spawn(f, argv)
}

/// Resolve a tagged word to a live future, or `None` if it is not one.
///
/// This is the check that stops `await 5` from being a segfault. Previously
/// codegen `int_to_ptr`'d whatever word was in the register and dereferenced
/// it; now the tag says whether it is a heap pointer at all, and the `ObjKind`
/// byte says whether that object is a future.
fn as_future(word: i64) -> Option<*mut FutureObj> {
    let v = JadeValue::from_bits(word as u64);
    if !v.is_ptr() {
        return None;
    }
    let p = v.as_ptr() as *mut FutureObj;
    let kind = unsafe { (*(p as *const ObjHeader)).kind };
    if kind == ObjKind::Future as u8 { Some(p) } else { None }
}

/// Await a tagged word. Reports `NotAFuture` rather than dereferencing a
/// non-pointer.
///
/// # Safety
/// The out-params must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_await_word(
    word: i64,
    failed: *mut i32,
    err: *mut i64,
    ty: *mut *const c_char,
) -> i64 {
    match as_future(word) {
        Some(f) => unsafe { report(await_one(f), failed, err, ty) },
        None => unsafe { report(Err(TaskError::NotAFuture), failed, err, ty) },
    }
}

/// Join tagged words. A non-future anywhere in the list reports `NotAFuture`.
///
/// # Safety
/// `words` must point at `n` readable words and `out` at `n` writable ones.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_join_words(
    words: *const i64,
    n: i32,
    out: *mut i64,
    failed: *mut i32,
    err: *mut i64,
    ty: *mut *const c_char,
) {
    let list = if n > 0 && !words.is_null() {
        unsafe { core::slice::from_raw_parts(words, n as usize) }.to_vec()
    } else {
        Vec::new()
    };

    let mut first: Option<TaskError> = None;
    for (i, &w) in list.iter().enumerate() {
        let slot = match as_future(w) {
            Some(f) => match unsafe { await_one(f) } {
                Ok(v) => v,
                Err(e) => {
                    if first.is_none() {
                        first = Some(e);
                    }
                    0
                }
            },
            None => {
                if first.is_none() {
                    first = Some(TaskError::NotAFuture);
                }
                0
            }
        };
        unsafe { *out.add(i) = slot };
    }

    unsafe { report(first.map_or(Ok(0), Err), failed, err, ty) };
}

/// Await `fut`. On a raised exception or a double await, `*failed` is set and
/// the C forwarder throws; otherwise the result is returned.
///
/// # Safety
/// `fut` must be live; the out-params must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_await_impl(
    fut: *mut FutureObj,
    failed: *mut i32,
    err: *mut i64,
    ty: *mut *const c_char,
) -> i64 {
    unsafe { report(await_one(fut), failed, err, ty) }
}

/// Await every future in order, writing results into `out`. Reports the first
/// failure the same way as `jrt_await_impl`.
///
/// # Safety
/// `futs` must point at `n` live futures and `out` at `n` writable words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_join_impl(
    futs: *const *mut FutureObj,
    n: i32,
    out: *mut i64,
    failed: *mut i32,
    err: *mut i64,
    ty: *mut *const c_char,
) {
    let list = if n > 0 && !futs.is_null() {
        unsafe { core::slice::from_raw_parts(futs, n as usize) }.to_vec()
    } else {
        Vec::new()
    };

    // Await every future even after one fails, matching the VM: a failing task
    // does not cancel its siblings, they run to completion. Results are written
    // per-slot as they arrive, so `out` is fully initialized on every path and
    // the successful entries survive alongside a reported failure.
    let mut first: Option<TaskError> = None;
    for (i, &f) in list.iter().enumerate() {
        let slot = match unsafe { await_one(f) } {
            Ok(v) => v,
            Err(e) => {
                if first.is_none() {
                    first = Some(e);
                }
                0
            }
        };
        unsafe { *out.add(i) = slot };
    }

    unsafe { report(first.map_or(Ok(0), Err), failed, err, ty) };
}

/// Translate a task outcome into the out-param protocol the C forwarders use.
///
/// # Safety
/// All out-params must be writable.
unsafe fn report(
    r: Result<i64, TaskError>,
    failed: *mut i32,
    err: *mut i64,
    ty: *mut *const c_char,
) -> i64 {
    unsafe {
        *failed = 0;
        *err = 0;
        *ty = core::ptr::null();
    }
    match r {
        Ok(v) => v,
        Err(TaskError::Raised(e, t)) => {
            unsafe {
                *failed = 1;
                *err = e;
                *ty = t;
            }
            0
        }
        Err(TaskError::DoubleAwait) => {
            unsafe {
                *failed = 2;
                *err = 0;
            }
            0
        }
        Err(TaskError::NotAFuture) => {
            unsafe {
                *failed = 3;
                *err = 0;
            }
            0
        }
    }
}

/// Release a future. Until futures are refcounted values (next step), codegen
/// owns this call — which is exactly why they leak today: the old
/// `jade_future_free` had no call sites at all.
///
/// # Safety
/// `fut` must be live and unreferenced afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_future_free(fut: *mut FutureObj) {
    // Release, not destroy: the running task holds its own reference until it
    // has written the result, so freeing on the handle's word alone is a
    // use-after-free whenever the worker has not finished releasing.
    unsafe { release(fut) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

    /// The pool, the heap instrument, and the call counters below are all
    /// process-global, so tests that assert on them need exclusive access.
    /// Cargo runs tests in one process on many threads by default, which
    /// otherwise makes every count here a race against the other tests.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn exclusive() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    // Task bodies must be `extern "C" fn`, so the tests use module-level
    // functions with static state rather than closures.

    static DOUBLE_CALLS: AtomicUsize = AtomicUsize::new(0);
    extern "C" fn double(args: *mut i64, n: i32) -> i64 {
        DOUBLE_CALLS.fetch_add(1, Ordering::Relaxed);
        assert_eq!(n, 1);
        unsafe { *args * 2 }
    }

    static ECHO_CALLS: AtomicUsize = AtomicUsize::new(0);
    extern "C" fn echo_double(args: *mut i64, _n: i32) -> i64 {
        ECHO_CALLS.fetch_add(1, Ordering::Relaxed);
        unsafe { *args * 2 }
    }

    static CONCURRENT: AtomicI64 = AtomicI64::new(0);
    static PEAK: AtomicI64 = AtomicI64::new(0);
    extern "C" fn observe_concurrency(_args: *mut i64, _n: i32) -> i64 {
        let now = CONCURRENT.fetch_add(1, Ordering::SeqCst) + 1;
        PEAK.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(20));
        CONCURRENT.fetch_sub(1, Ordering::SeqCst);
        0
    }

    #[test]
    fn spawn_and_await_returns_the_result() {
        let _g = exclusive();
        let f = spawn(double, vec![21]);
        assert_eq!(unsafe { await_one(f) }, Ok(42));
        unsafe { release(f) };
    }

    #[test]
    fn second_await_is_an_error() {
        let _g = exclusive();
        let f = spawn(double, vec![1]);
        assert_eq!(unsafe { await_one(f) }, Ok(2));
        assert_eq!(
            unsafe { await_one(f) },
            Err(TaskError::DoubleAwait),
            "a future resolves once and is consumed once, matching the VM"
        );
        unsafe { release(f) };
    }

    #[test]
    fn join_returns_results_in_argument_order() {
        let _g = exclusive();
        let futs: Vec<_> = [1i64, 2, 3].iter().map(|&n| spawn(double, vec![n])).collect();
        assert_eq!(unsafe { join_all(&futs) }, Ok(vec![2, 4, 6]));
        for f in futs {
            unsafe { release(f) };
        }
    }

    /// Far more tasks than workers must all complete — the queue is what bounds
    /// concurrency, not what drops work.
    #[test]
    fn every_task_runs_even_when_far_over_subscribed() {
        let _g = exclusive();
        ECHO_CALLS.store(0, Ordering::Relaxed);
        const N: usize = 500;
        let futs: Vec<_> = (0..N as i64).map(|n| spawn(echo_double, vec![n])).collect();
        for (i, &f) in futs.iter().enumerate() {
            assert_eq!(unsafe { await_one(f) }, Ok(i as i64 * 2));
        }
        assert_eq!(ECHO_CALLS.load(Ordering::Relaxed), N);
        for f in futs {
            unsafe { release(f) };
        }
    }

    /// The point of the pool: 500 tasks must not become 500 threads.
    #[test]
    fn pool_bounds_thread_count() {
        let _g = exclusive();
        let futs: Vec<_> = (0..200).map(|_| spawn(double, vec![1])).collect();
        for &f in &futs {
            let _ = unsafe { await_one(f) };
        }
        let workers = pool().worker_count();
        assert!(workers <= HARD_MAX_WORKERS, "worker count {workers} exceeded the hard ceiling");
        assert!(
            workers < 200,
            "200 tasks produced {workers} threads — the pool is not bounding anything"
        );
        for f in futs {
            unsafe { release(f) };
        }
    }

    /// Peak simultaneous execution should track the pool target rather than the
    /// number of queued tasks.
    #[test]
    fn concurrency_is_bounded_not_serialized() {
        let _g = exclusive();
        CONCURRENT.store(0, Ordering::SeqCst);
        PEAK.store(0, Ordering::SeqCst);
        let futs: Vec<_> = (0..32).map(|_| spawn(observe_concurrency, vec![])).collect();
        for &f in &futs {
            let _ = unsafe { await_one(f) };
        }
        let peak = PEAK.load(Ordering::SeqCst);
        assert!(peak > 1, "tasks ran one at a time — the pool is not concurrent");
        for f in futs {
            unsafe { release(f) };
        }
    }

    /// A task that itself awaits another task. This is the case a fixed-size
    /// pool deadlocks on, and it is not hypothetical — `double_then_increment`
    /// in `examples/async/basic` has exactly this shape.
    ///
    /// With T workers and far more than T outer tasks, every worker ends up
    /// blocked in `await` while the inner tasks it is waiting for sit behind the
    /// remaining outer tasks in the queue. Nothing can progress unless the pool
    /// backfills a worker when one blocks.
    extern "C" fn outer_awaits_inner(args: *mut i64, _n: i32) -> i64 {
        let n = unsafe { *args };
        let inner = spawn(double, vec![n]);
        let r = unsafe { await_one(inner) }.expect("inner task must resolve");
        unsafe { release(inner) };
        r
    }

    #[test]
    fn nested_await_does_not_deadlock_a_bounded_pool() {
        let _g = exclusive();
        // Comfortably more outer tasks than any plausible pool target, so the
        // deadlock is certain rather than timing-dependent if compensation is
        // removed.
        const OUTER: i64 = 64;

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let futs: Vec<_> = (0..OUTER).map(|n| spawn(outer_awaits_inner, vec![n])).collect();
            let mut sum = 0i64;
            for &f in &futs {
                sum += unsafe { await_one(f) }.expect("outer task must resolve");
            }
            for f in futs {
                unsafe { release(f) };
            }
            let _ = tx.send(sum);
        });

        // A deadlock would otherwise hang the whole test binary rather than
        // reporting a failure.
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(sum) => assert_eq!(sum, (0..OUTER).map(|n| n * 2).sum::<i64>()),
            Err(_) => panic!(
                "nested awaits deadlocked the pool: every worker blocked in await \
                 while the tasks they wait on stayed queued"
            ),
        }
    }

    /// Releasing the last reference reclaims the future and records the free.
    ///
    /// Tested without a task, so it is a statement about the accounting alone:
    /// `spawn` is what gives a future a second owner, and the point here is that
    /// the count moves when the *last* holder lets go.
    #[test]
    fn releasing_the_last_reference_records_the_free() {
        let _g = exclusive();
        // `exclusive()` orders the task tests against each other; the live count
        // is global to the whole binary, so this also needs the counter lock.
        let _c = crate::gc::test_support::lock_counter();
        let before = gc::jrt_heap_live_count();
        let fut = gc::leak_obj(FutureObj::new()) as *mut FutureObj;
        assert_eq!(gc::jrt_heap_live_count(), before + 1, "leak_obj must be instrumented");
        unsafe { release(fut) }; // sole reference
        assert_eq!(gc::jrt_heap_live_count(), before, "release at zero must free");
    }

    /// The worker releases its reference once it has written the result.
    ///
    /// This is the half of the two-owner protocol that a handle-side test
    /// cannot see, and it is what the use-after-free came from: the worker's
    /// release used to land on an allocation the handle had already freed.
    ///
    /// Asserted on the future's own refcount rather than the global live count.
    /// The count is process-wide and frees arrive asynchronously — a worker from
    /// an *earlier* test can reclaim its future in the middle of this one, so
    /// the baseline moves under you. Reading the header here is safe because we
    /// still hold our own reference, and the poll is bounded so a worker that
    /// never releases still fails.
    #[test]
    fn the_worker_releases_its_reference_after_completing() {
        let _g = exclusive();
        let f = spawn(double, vec![5]);
        assert_eq!(unsafe { await_one(f) }.unwrap(), 10);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while unsafe { (*f).header.rc() } > 1 && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(
            unsafe { (*f).header.rc() },
            1,
            "the worker never released its reference — only the handle's is left"
        );

        // Ours is now the last one, so this is the release that reclaims it.
        unsafe { release(f) };
    }
}
