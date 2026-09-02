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
//! ## Why an awaiting thread runs the task itself
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
//! way, which is why it was never hit.
//!
//! Growing the pool when a worker blocks is the usual answer, and this module
//! does that. It is not enough on its own. `await` blocks a whole OS thread, so
//! a chain of N nested awaits pins N threads, and every pool has a last thread:
//! at [`HARD_MAX_WORKERS`] the innermost body had nobody left to run it and the
//! entire chain waited for it forever. Raising the ceiling moves the wall
//! without removing it, and a hang is a worse failure than the abort it
//! replaced.
//!
//! So the awaiting thread runs the body itself. If nobody has claimed the task
//! yet, `await` takes it and calls it inline instead of parking. Nested `await`
//! then costs *stack* rather than threads — the same budget ordinary recursion
//! spends — and the pool's bound goes back to meaning what it says: a cap on
//! how much runs in parallel, not a cap on how deep an await chain may go.
//! Deadlock now requires a genuine cycle in the await graph, which no
//! scheduling policy can rescue.
//!
//! The two mechanisms cover different cases and both are needed. Growing keeps
//! *parallelism* up when a worker parks on a task another thread is already
//! running; running inline guarantees *progress* when there is no other thread
//! to wait for.

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
    fresh_budget: i32,
    out_result: *mut i64,
    out_err: *mut i64,
    out_type: *mut *const c_char,
) -> i32;

/// How long an idle worker waits before retiring, so a burst of blocking
/// compensation threads does not linger for the life of a long-running service.
const IDLE_TIMEOUT: Duration = Duration::from_secs(10);

/// Ceiling on total worker threads, blocked ones included.
///
/// A cap on parallelism, not on await depth. Reaching it used to hang a nested
/// `await` chain; since an awaiter runs its own task rather than park, hitting
/// this only means the next body waits for a thread instead of getting one of
/// its own.
const HARD_MAX_WORKERS: usize = 512;

/// Stack for a pool worker, matching the one a compiled binary gives its main
/// body (`JRT_MAIN_STACK_SIZE` in `runtime_aot/posix.c`).
///
/// Rust's 2 MiB default made the *same* function succeed at top level and
/// overflow inside an `async fn`, and a stack overflow on a worker takes the
/// whole process down with no Jade error to read. Address space is reserved,
/// not committed, so a thread that never recurses costs nothing for the room.
const WORKER_STACK_SIZE: usize = 256 * 1024 * 1024;

// ── How many tasks may run at once ───────────────────────────────────────────

/// How many tasks run side by side when a program says nothing.
///
/// Not the core count, which is what this used to be. A Jade task is far more
/// often waiting on a socket than saturating a core — the language's own
/// advertised shape is a fan-out over N prompts — so sizing the limit to the
/// machine's parallelism sized it to the wrong resource, and a laptop and a
/// build server ran the same fan-out in a different number of waves.
///
/// 32 is a flat number both engines can promise. It is high enough that an
/// ordinary fan-out finishes in one wave, and low enough to stay well under the
/// ceiling on threads.
pub const DEFAULT_MAX_TASKS: usize = 32;

/// The live limit. Read on every scheduling decision rather than captured at
/// startup, so `set_max_tasks` takes effect on the next task rather than the
/// next run.
static MAX_TASKS: AtomicUsize = AtomicUsize::new(DEFAULT_MAX_TASKS);

/// How many tasks may run at once.
pub fn max_tasks() -> usize {
    MAX_TASKS.load(Ordering::Relaxed)
}

/// Set how many tasks may run at once, and report what is now in force.
///
/// The request is clamped to `1..=HARD_MAX_WORKERS` rather than refused. Zero
/// runnable tasks is not a state a program can want, and the ceiling is a real
/// property of the thread supply, so both ends have one honest answer to give.
/// Returning the clamped value is what makes the clamp visible: a program that
/// asks for 9999 can see it got 512 without a second call.
pub fn set_max_tasks(n: usize) -> usize {
    let effective = n.clamp(1, HARD_MAX_WORKERS);
    MAX_TASKS.store(effective, Ordering::Relaxed);
    effective
}

/// `max_tasks()` for a compiled program.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_max_tasks() -> i64 {
    max_tasks() as i64
}

/// `set_max_tasks(n)` for a compiled program, answering the effective value.
///
/// A negative argument clamps to 1 like any other out-of-range request. It
/// arrives as `i64` because that is what a Jade `int` is, and saturating the
/// cast keeps a huge value from wrapping to something small.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_set_max_tasks(n: i64) -> i64 {
    set_max_tasks(n.clamp(0, HARD_MAX_WORKERS as i64) as usize) as i64
}

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
fn invoke(f: TaskFn, args: &mut [i64], fresh_budget: bool) -> Result<i64, (i64, *const c_char)> {
    let raw = INVOKER.load(Ordering::Acquire);
    let n = args.len() as i32;
    if raw.is_null() {
        return Ok(f(args.as_mut_ptr(), n));
    }
    let shim: TaskInvoker = unsafe { core::mem::transmute(raw) };
    let mut result = 0i64;
    let mut err = 0i64;
    let mut ty: *const c_char = core::ptr::null();
    let fresh = i32::from(fresh_budget);
    let failed = unsafe { shim(f, args.as_mut_ptr(), n, fresh, &mut result, &mut err, &mut ty) };
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
    /// The caller cancelled it and is no longer waiting. See [`cancel`].
    Cancelled,
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
    /// The body, until a worker pops it off the queue or an awaiter claims it.
    /// `None` means somebody is already running it.
    pending: Option<Job>,
    /// Nobody wants this result any more.
    ///
    /// Cancelling does not stop the work — a task is a real thread running
    /// straight-line code with no point at which it could be interrupted, and
    /// pretending otherwise would be a lie about what the runtime can do. It
    /// says the *caller* has stopped waiting: `await` raises at once instead of
    /// blocking, and a task that agrees to check `cancelled()` can give up early
    /// of its own accord.
    cancelled: bool,
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
    fn new(job: Job) -> Self {
        FutureObj {
            header: ObjHeader::new(ObjKind::Future, 0),
            state: Mutex::new(FutState {
                done: false,
                consumed: false,
                result: 0,
                failed: false,
                error: 0,
                error_type: core::ptr::null(),
                pending: Some(job),
                cancelled: false,
            }),
            done_cv: Condvar::new(),
        }
    }

    /// Take the body if nobody is running it yet.
    fn claim(&self) -> Option<Job> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).pending.take()
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
        // …and anyone waiting on *several* futures at once, who cannot watch
        // every future's own condvar. See [`wait_any`].
        completions().notify_all();
    }
}

/// Signalled whenever any future finishes or is cancelled.
///
/// [`wait_any`] blocks on this and rechecks its list, which is the whole reason
/// it exists: a waiter interested in N futures cannot park on N condvars. One
/// broadcast per completion is the cost, and completions are rare next to the
/// work that produced them.
struct Completions {
    lock: Mutex<u64>,
    cv: Condvar,
}

impl Completions {
    fn notify_all(&self) {
        let mut n = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        *n = n.wrapping_add(1);
        drop(n);
        self.cv.notify_all();
    }
}

fn completions() -> &'static Completions {
    static C: OnceLock<Completions> = OnceLock::new();
    C.get_or_init(|| Completions { lock: Mutex::new(0), cv: Condvar::new() })
}

// ── The pool ─────────────────────────────────────────────────────────────────

/// A task body and its arguments, waiting for a thread to run them.
///
/// The job lives *in the future it resolves* rather than in the pool's queue,
/// which is what lets an awaiter claim it — see [`await_one`]. The queue holds
/// only a pointer to the future, so claiming is one lock on that future rather
/// than a scan of the queue.
struct Job {
    f: TaskFn,
    args: Vec<i64>,
    /// Whether `args` are tagged Jade values this job took a reference to and
    /// must release when the body is done. See [`spawn`].
    owns_args: bool,
}

/// A queue entry: a future whose body nobody has picked up yet.
///
/// Carries one reference to the future, released by whichever worker pops it —
/// whether or not the body is still there to run, since an awaiter may have
/// claimed it in the meantime and left the entry behind.
struct Queued(*mut FutureObj);

// The future outlives the entry: the entry holds a reference of its own.
unsafe impl Send for Queued {}

struct Inner {
    queue: VecDeque<Queued>,
    /// Threads currently alive, blocked ones included.
    workers: usize,
    /// Threads parked waiting for work.
    idle: usize,
    /// Threads inside `await`. They hold no CPU, so they do not count against
    /// the limit — see the module docs on why a fixed pool deadlocks.
    blocked: usize,
    /// Threads inside a task body right now. This is the number `max_tasks`
    /// bounds, and it is not the same as `workers`: the pool keeps idle threads
    /// around for `IDLE_TIMEOUT` after a burst, so a wide fan-out followed by
    /// `set_max_tasks(4)` leaves far more threads alive than the program now
    /// permits to run. Bounding growth alone let all of them take work.
    running: usize,
}

pub struct Pool {
    inner: Mutex<Inner>,
    work_cv: Condvar,
}

static POOL: OnceLock<Pool> = OnceLock::new();

/// How many times the OS refused a worker thread.
///
/// Not an error, and deliberately so. The work is not lost when a thread is
/// refused: whoever awaits the future runs the body inline, so a machine out of
/// threads runs its tasks one after another and still gets the right answer.
/// Raising here would turn a correct-but-slow run into a failed one, and it
/// would fail on a loaded machine and pass on an idle one, which is a poor
/// property for something a `catch` might match.
///
/// It is worth *saying*, though. A program whose fan-out has quietly gone
/// serial looks identical to one that was always slow, so [`warn_once`] prints
/// a line the first time this happens.
static SPAWN_FAILURES: AtomicUsize = AtomicUsize::new(0);

/// Say once that the machine is out of threads, on the way to the first failure.
///
/// Once, because the condition repeats per spawn and a program in this state is
/// spawning constantly — a line per attempt would bury the run in its own
/// diagnostics. To stderr, because stdout is the program's own output and this
/// is not part of it.
///
/// Answers whether it printed, which is the only part of this with logic in it
/// and the only part a test can reach: making a real `thread::spawn` fail needs
/// a machine genuinely out of threads.
fn warn_out_of_threads() -> bool {
    if SPAWN_FAILURES.fetch_add(1, Ordering::Relaxed) != 0 {
        return false;
    }
    eprintln!(
        "jade: the OS refused a task thread, so tasks are now running one at a time.\n\
         jade: lower set_max_tasks(), or raise the process thread limit (ulimit -u)."
    );
    true
}

thread_local! {
    /// Whether this thread is one of the [`Inner::running`] count. Kept per
    /// thread rather than passed around because the thread that has to give the
    /// count back is whichever one reaches `await`, several frames inside a body
    /// that knows nothing about the pool.
    static COUNTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The process-wide pool. How wide it grows is [`max_tasks`], read live.
pub fn pool() -> &'static Pool {
    POOL.get_or_init(|| Pool {
        inner: Mutex::new(Inner {
            queue: VecDeque::new(),
            workers: 0,
            idle: 0,
            blocked: 0,
            running: 0,
        }),
        work_cv: Condvar::new(),
    })
}

impl Pool {
    /// Queue a future's body, starting a worker if there is nobody free to
    /// take it.
    fn submit(&'static self, fut: *mut FutureObj) {
        let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        st.queue.push_back(Queued(fut));
        if self.should_grow(&st) {
            st.workers += 1;
            if !self.start_worker() {
                // No thread to be had. Undo the count so a later submit retries
                // rather than believing a worker exists. The queued work is not
                // lost: whoever awaits it runs it inline, so a machine out of
                // threads does the work sequentially instead of hanging.
                st.workers -= 1;
                let _ = warn_out_of_threads();
            }
        }
        drop(st);
        self.work_cv.notify_one();
    }

    /// Whether queued work would otherwise sit unclaimed.
    ///
    /// `workers - blocked` is the runnable population; blocked workers are
    /// parked in `await` and cannot pick anything up.
    ///
    /// Saturating, because `blocked` counts *every* thread parked in `await`
    /// and `workers` counts only the pool's own. A program that awaits at the
    /// top level blocks the main thread, which is not a worker — so with a
    /// deep chain of nested awaits `blocked` passes `workers` and a plain
    /// subtraction underflows. `await`ing 1,000 deep aborted the binary on it.
    /// Zero is also the right reading: more threads blocked than the pool has
    /// means nothing is left to run the queue, which is exactly when to grow.
    fn should_grow(&self, st: &Inner) -> bool {
        // One idle worker used to be enough to refuse, which quietly pinned the
        // pool at whatever width the *previous* fan-out left behind. Eight tasks
        // submitted in a loop onto four idle workers grew to nothing: every
        // submit saw `idle > 0`, because no worker had woken up yet, so eight
        // tasks ran four at a time under a limit of sixteen. Comparing the whole
        // queue against the idle count asks the question that matters, which is
        // whether anything would sit unclaimed. It can over-create by a thread
        // or two when workers wake mid-loop, and that is the safe direction:
        // `max_tasks` still bounds it, and the other way is a limit the engine
        // does not honor.
        if st.queue.len() <= st.idle {
            return false;
        }
        let runnable = st.workers.saturating_sub(st.blocked);
        runnable < max_tasks() && st.workers < HARD_MAX_WORKERS
    }

    /// Add a worker thread, reporting whether the OS gave us one.
    ///
    /// *Both callers hold `inner`*, so this must not touch it. It used to undo
    /// the worker count here on failure, which re-locked a `std::sync::Mutex`
    /// this very thread already owned and wedged the process inside `spawn`
    /// with no way out. The success path hid it: the closure body runs on the
    /// *new* thread, so the re-lock only ever happened when `spawn` returned
    /// `Err` — that is, exactly when the machine was out of threads and the
    /// recovery path mattered most.
    #[must_use]
    fn start_worker(&'static self) -> bool {
        std::thread::Builder::new()
            .name("jade-task".to_string())
            .stack_size(WORKER_STACK_SIZE)
            .spawn(move || self.worker_loop())
            .is_ok()
    }

    fn worker_loop(&'static self) {
        loop {
            let Queued(fut) = {
                let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    // Past the limit this thread parks as though there were no
                    // work, and the release of a running slot wakes it. Checking
                    // here rather than only when growing is what makes the limit
                    // bind on threads that already exist.
                    if st.running < max_tasks()
                        && let Some(j) = st.queue.pop_front()
                    {
                        st.running += 1;
                        COUNTED.with(|c| c.set(true));
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

            // Safety: the queue entry holds a reference, so the future is live
            // here even if every other holder dropped it.
            //
            // `None` means an awaiter claimed the body and ran it itself, and
            // left this entry behind rather than scan the queue for it. Nothing
            // to do but let go of the entry's reference.
            if let Some(job) = unsafe { (*fut).claim() } {
                run_job(fut, job, true);
            }
            self.stop_running();
            unsafe { release(fut) };
        }
    }

    /// Announce that the calling thread is about to block in `await`.
    ///
    /// Spawns a replacement if that would leave queued work with nobody to run
    /// it. This is what keeps `await` chains from deadlocking a bounded pool.
    fn enter_blocking(&'static self) -> bool {
        // A thread about to park is not running a body, so it gives its slot
        // back for the duration. Without this, `set_max_tasks(1)` plus a task
        // that awaits another is a deadlock: the parent holds the only slot and
        // the child can never take one.
        let had_slot = self.stop_running();
        let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        st.blocked += 1;
        if self.should_grow(&st) {
            st.workers += 1;
            if !self.start_worker() {
                // No thread to be had. Undo the count so a later submit retries
                // rather than believing a worker exists. The queued work is not
                // lost: whoever awaits it runs it inline, so a machine out of
                // threads does the work sequentially instead of hanging.
                st.workers -= 1;
                let _ = warn_out_of_threads();
            }
        }
        drop(st);
        had_slot
    }

    /// `had_slot` is what [`Self::enter_blocking`] answered.
    fn exit_blocking(&'static self, had_slot: bool) {
        {
            let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            st.blocked -= 1;
        }
        if had_slot {
            self.start_running();
        }
    }

    /// Count this thread as running a body.
    ///
    /// Deliberately does not wait for room. A thread reaching here has either
    /// just been handed work under the gate above, or has finished waiting and
    /// is resuming a body it already started; making the second case queue would
    /// reintroduce the deadlock `enter_blocking` exists to avoid. It can leave
    /// the count briefly over the limit, which is the same allowance an awaiter
    /// running a body inline already has.
    fn start_running(&'static self) {
        if COUNTED.with(|c| c.replace(true)) {
            return;
        }
        let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        st.running += 1;
    }

    /// Stop counting this thread, and wake somebody parked on the limit.
    ///
    /// Reports whether it gave a slot back, so a caller that parks knows whether
    /// it owes one on the way out. The main thread never held one — it is not a
    /// task — and must not manufacture one by resuming.
    fn stop_running(&'static self) -> bool {
        if !COUNTED.with(|c| c.replace(false)) {
            return false;
        }
        {
            let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            st.running -= 1;
        }
        self.work_cv.notify_one();
        true
    }

    /// Current worker-thread count. For tests and diagnostics.
    pub fn worker_count(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).workers
    }

    /// How many times the OS refused a worker thread. See [`SPAWN_FAILURES`]
    /// for why that is not an error.
    pub fn spawn_failures(&self) -> usize {
        SPAWN_FAILURES.load(Ordering::Relaxed)
    }
}

// ── Running a body ───────────────────────────────────────────────────────────

thread_local! {
    /// The future the body running on this thread resolves, so `cancelled()`
    /// can answer without the program having to pass its own handle around.
    /// Restored rather than cleared, because an awaiter runs a body inline and
    /// is very likely a task itself.
    static CURRENT: core::cell::Cell<*mut FutureObj> = const {
        core::cell::Cell::new(core::ptr::null_mut())
    };
}

/// Whether the task running on this thread has been cancelled.
///
/// False outside a task, which is the honest answer: the top level is not
/// something anyone can cancel.
pub fn current_is_cancelled() -> bool {
    let f = CURRENT.with(|c| c.get());
    !f.is_null() && unsafe { is_cancelled(f) }
}

/// Run a task body and publish its outcome on `fut`.
///
/// The one place a body runs, reached from two directions: a pool worker that
/// popped it off the queue, and an awaiter that claimed it rather than park
/// (see [`await_one`]). Both must clean up identically, which is why neither
/// does it inline.
///
/// `fresh_stack` says whether the body starts at the bottom of a thread's own
/// stack, which decides whether it gets a fresh recursion budget. A worker does.
/// An awaiter does not: running the body inline puts it genuinely deeper on a
/// stack that is already in use, so it has to keep counting, or nothing bounds
/// an `await` chain and a deep one walks off the end of the stack — a SIGBUS
/// with no output at all, where the interpreter prints the answer and ordinary
/// recursion raises. The limit is the honest one either way: past it a compiled
/// binary says "recursion limit exceeded", where the interpreter, whose tasks
/// live on the heap rather than the stack, keeps going.
///
/// Does not touch `fut`'s reference count. The caller holds one either way —
/// the worker holds the queue entry's, the awaiter holds the handle's.
fn run_job(fut: *mut FutureObj, job: Job, fresh_stack: bool) {
    let mut args = job.args;
    // Snapshot before invoking: `invoke` hands the wrapper a mutable pointer,
    // and the words to release are the ones the job took a reference to, not
    // whatever is left in the buffer afterwards.
    let owned: Vec<i64> = if job.owns_args { args.clone() } else { Vec::new() };

    // Bracket the body's generator frames. A `yield`ing function that raises
    // never reaches its own `jrt_yield_pop`, so its buffer stays on the thread's
    // stack and the next `yield` to run here lands in the wrong one. Harmless
    // when a thread runs one body and dies; not harmless when pool workers are
    // reused, and much worse now that an awaiter may run a body inline — there
    // the next `yield` belongs to the awaiting function itself.
    let yield_mark = crate::coll::jrt_yield_depth();
    let prev = CURRENT.with(|c| c.replace(fut));
    let outcome = invoke(job.f, &mut args, fresh_stack);
    CURRENT.with(|c| c.set(prev));
    crate::coll::jrt_yield_truncate(yield_mark);

    // Give back the reference `spawn` took on the job's behalf. The task body
    // borrowed these — a callee never releases its parameters — so this is the
    // only release, and it happens after the body is done reading them.
    for &a in &owned {
        gc::jrt_decref(a);
    }
    unsafe { (*fut).complete(outcome) };
}

// ── Timers ───────────────────────────────────────────────────────────────────
//
// `time.after(secs)` is a future that finishes on its own, with no body to run.
// That is what lets a deadline be an ordinary argument to [`wait_any`] rather
// than a special parameter on it: waiting for something *or* a timeout is
// waiting for one of two futures.
//
// One thread for all of them, not a pool task each. A task that sleeps holds a
// worker for the duration and does not announce itself as blocked — `await` is
// the only thing that does — so a redraw loop arming a 16ms timer every frame
// would saturate the pool with sleepers and stall real work behind them. One
// thread parked on the earliest deadline costs nothing per timer.

struct Timer {
    deadline: std::time::Instant,
    fut: *mut FutureObj,
}

// The future is kept alive by the reference `after` takes for the timer.
unsafe impl Send for Timer {}

struct Timers {
    /// Earliest deadline last, so the next one to fire is a `pop`.
    pending: Mutex<Vec<Timer>>,
    cv: Condvar,
}

fn timers() -> &'static Timers {
    static T: OnceLock<Timers> = OnceLock::new();
    T.get_or_init(|| Timers { pending: Mutex::new(Vec::new()), cv: Condvar::new() })
}

/// Start the timer thread if it is not already running, reporting whether one
/// is now there to fire deadlines.
///
/// Not a `Once`. A `Once` counts an *attempt*, so a machine that was briefly out
/// of threads at the first `after` left every later `time.after` in that process
/// waiting on a deadline nothing would ever fire — a permanent hang from a
/// transient condition. Retrying per call costs an atomic on the common path and
/// makes the failure recoverable.
///
/// The answer matters because a timer future has no body: if nothing fires it,
/// `await` waits forever. That is why [`after`] reports the failure instead of
/// handing back a future that can never finish.
fn start_timer_thread() -> bool {
    /// Set once a timer thread is actually running.
    static RUNNING: AtomicUsize = AtomicUsize::new(0);
    if RUNNING.load(Ordering::Acquire) == 1 {
        return true;
    }
    static STARTING: Mutex<()> = Mutex::new(());
    let _g = STARTING.lock().unwrap_or_else(|e| e.into_inner());
    // Re-checked under the lock: another caller may have started it while this
    // one waited.
    if RUNNING.load(Ordering::Acquire) == 1 {
        return true;
    }
    {
        let t = timers();
        // A program with no timers never creates this thread at all.
        let started = std::thread::Builder::new().name("jade-timer".to_string()).spawn(move || {
            loop {
                let mut q = t.pending.lock().unwrap_or_else(|e| e.into_inner());
                let now = std::time::Instant::now();
                // Everything already due, in one pass.
                let mut fired: Vec<*mut FutureObj> = Vec::new();
                while q.last().is_some_and(|x| x.deadline <= now) {
                    fired.push(q.pop().expect("just checked").fut);
                }
                let next = q.last().map(|x| x.deadline);
                drop(q);

                for fut in fired {
                    // Safety: `after` took a reference for this timer, so the
                    // future is live until the release below.
                    unsafe {
                        (*fut).complete(Ok(crate::value::NIL_BITS as i64));
                        release(fut);
                    }
                }

                let q = t.pending.lock().unwrap_or_else(|e| e.into_inner());
                let _ = match next {
                    // Park until the earliest deadline, or until `after` adds an
                    // earlier one and wakes us.
                    Some(d) => t.cv.wait_timeout(q, d.saturating_duration_since(now)),
                    None => t.cv.wait_timeout(q, IDLE_TIMEOUT),
                };
            }
        });
        if started.is_ok() {
            RUNNING.store(1, Ordering::Release);
            return true;
        }
    }
    false
}

/// A future that finishes with nil after `secs`, with no task behind it.
///
/// A negative or non-finite delay is treated as zero: the deadline is already
/// past, which is the only reading that does not invent a wait.
pub fn after(secs: f64) -> *mut FutureObj {
    // Null rather than a future nothing can finish. A timer future has no body,
    // so with no thread to fire it `await` would wait for the life of the
    // process — a hang, which is the worst way for this to fail. The caller
    // turns it into an error a program can read.
    if !start_timer_thread() {
        return core::ptr::null_mut();
    }
    let d = if secs.is_finite() && secs > 0.0 {
        std::time::Duration::from_secs_f64(secs)
    } else {
        std::time::Duration::ZERO
    };
    // A timer future has no body, so nothing will ever claim its `pending`. The
    // job is a placeholder that never runs: `await` on it finds `pending` taken
    // by nobody and simply waits, which is exactly right.
    let fut = gc::leak_obj(FutureObj::new(Job { f: never, args: Vec::new(), owns_args: false }))
        as *mut FutureObj;
    // The timer's own reference, released when it fires. Without it a program
    // that drops the handle would free the future out from under the thread.
    unsafe { (*fut).header.incref() };
    // Claimed here and now, so no worker can ever pick this up and run `never`.
    let _ = unsafe { (*fut).claim() };

    let t = timers();
    let mut q = t.pending.lock().unwrap_or_else(|e| e.into_inner());
    let deadline = std::time::Instant::now() + d;
    q.push(Timer { deadline, fut });
    // Descending by deadline, so the next to fire is a `pop`.
    q.sort_by_key(|t| std::cmp::Reverse(t.deadline));
    drop(q);
    t.cv.notify_all();
    fut
}

/// The body a timer future never runs. See [`after`].
extern "C" fn never(_args: *mut i64, _n: i32) -> i64 {
    crate::value::NIL_BITS as i64
}

// ── Neutral cores ────────────────────────────────────────────────────────────

/// Start `f(args)` on the pool and return a future with one reference held by
/// the caller.
///
/// The returned pointer is `leak_obj`-allocated so the heap instrument counts
/// it, and header-prefixed so it can be refcounted like any other value.
pub fn spawn(f: TaskFn, args: Vec<i64>, owns_args: bool) -> *mut FutureObj {
    // The task outlives the expression that spawned it, so it cannot borrow its
    // arguments from the spawning frame — that frame releases its slots as soon
    // as it returns, and the task would then be reading freed memory:
    //
    //   fn go(n) { let a = [n, n]; return sum(a) }   // `a` released here
    //   let f = go(1)                                 // task still holds it
    //   await f                                       // use-after-free
    //
    // So the job takes its own reference to every argument, released in
    // `worker_loop` once the body has run. Non-heap words (the common case: an
    // int, a bool) no-op in both directions.
    //
    // `owns_args` says whether the words are *tagged Jade values* at all. They
    // are from `jrt_spawn`, the one entry a compiled program uses. They are not
    // from this crate's own tests, which hand tasks raw untagged integers —
    // reading one of those as a value dereferences whatever its low bits
    // happen to say, which is a null deref for a small odd number.
    if owns_args {
        for &a in &args {
            gc::jrt_incref(a);
        }
    }
    let fut = gc::leak_obj(FutureObj::new(Job { f, args, owns_args })) as *mut FutureObj;
    // Two owners, two references. `ObjHeader::new` starts the count at 1 for the
    // caller; the queue entry needs its own, because the future carries the body
    // and the result and must not be freed out from under either.
    //
    // This is not hypothetical now that futures are refcounted: a program that
    // spawns and drops the handle without awaiting would otherwise take the
    // count to zero, free the future, and leave a worker reading a reclaimed
    // allocation to decide there was nothing to run. The entry's reference is
    // released in `worker_loop` the moment it is popped.
    unsafe { (*fut).header.incref() };
    pool().submit(fut);
    fut
}

/// Block until `fut` resolves and take its result.
///
/// # Safety
/// `fut` must point at a live [`FutureObj`].
pub unsafe fn await_one(fut: *mut FutureObj) -> Result<i64, TaskError> {
    let f = unsafe { &*fut };

    // Run it here rather than wait for a thread that may never come.
    //
    // This thread is about to do nothing at all, so if nobody has claimed the
    // body it takes it. That is what makes a deep `await` chain finish: each
    // level costs a stack frame instead of an OS thread, so the depth limit is
    // the stack's, not the pool's. See the module docs.
    //
    // Only the body this thread is actually waiting on, never an arbitrary
    // queued one. Helping with unrelated work would deepen this stack without
    // moving this thread any closer to returning.
    if let Some(job) = f.claim() {
        run_job(fut, job, false);
    } else {
        // Somebody else is running it. Announce the block *before* waiting, so
        // the pool can backfill a worker if this thread is one of its own.
        // Harmless when the caller is the main thread: it just permits one
        // extra worker while the main thread waits.
        let p = pool();
        let had_slot = p.enter_blocking();
        let mut st = f.state.lock().unwrap_or_else(|e| e.into_inner());
        while !st.done && !st.cancelled {
            st = f.done_cv.wait(st).unwrap_or_else(|e| e.into_inner());
        }
        drop(st);
        p.exit_blocking(had_slot);
    }

    let mut st = f.state.lock().unwrap_or_else(|e| e.into_inner());

    // Cancelled outranks everything, including a result that arrived anyway.
    // The caller said it had stopped waiting, and handing it the answer after
    // that would make `cancel` mean nothing on a task that was about to finish.
    if st.cancelled {
        return Err(TaskError::Cancelled);
    }

    // A future resolves once and is consumed once. The VM enforces the same rule
    // by `.take()`-ing the join handle, so a second await is an error on both
    // engines rather than a silently duplicated result.
    if st.consumed {
        return Err(TaskError::DoubleAwait);
    }
    st.consumed = true;

    if st.failed { Err(TaskError::Raised(st.error, st.error_type)) } else { Ok(st.result) }
}

/// Whether `fut` has finished, without waiting for it.
///
/// The one thing you can ask a future that does not block. `await` is otherwise
/// the only way to read one, which makes a task useless to anything that cannot
/// afford to stop — a render loop has no point at which it is willing to freeze,
/// so it could start work concurrently and then never find out the answer.
///
/// A future that already handed its result to an awaiter counts as finished: it
/// is done, and the second `await` raises on its own terms.
///
/// # Safety
/// `fut` must point at a live [`FutureObj`].
pub unsafe fn is_done(fut: *mut FutureObj) -> bool {
    let f = unsafe { &*fut };
    let st = f.state.lock().unwrap_or_else(|e| e.into_inner());
    st.done
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

/// Stop waiting for `fut`.
///
/// It does *not* stop the work. A task is a real thread running straight-line
/// code with no point at which the runtime could interrupt it, and a `cancel`
/// that claimed otherwise would be a lie about what it can do. What it changes
/// is the caller's side: an `await` raises at once rather than blocking, and one
/// already blocked wakes up and raises.
///
/// A task that wants to give up early can cooperate by checking [`is_cancelled`]
/// — `cancelled()` in Jade — which is the only way work actually stops.
///
/// Cancelling twice, or cancelling something already finished, is not an error.
/// The second is the ordinary race: the answer arrived while the caller was
/// deciding it no longer wanted it, and it does not get it.
///
/// # Safety
/// `fut` must point at a live [`FutureObj`].
pub unsafe fn cancel(fut: *mut FutureObj) {
    let f = unsafe { &*fut };
    let mut st = f.state.lock().unwrap_or_else(|e| e.into_inner());
    if st.cancelled {
        return;
    }
    st.cancelled = true;
    drop(st);
    f.done_cv.notify_all();
    completions().notify_all();
}

/// Whether `fut` has been cancelled.
///
/// # Safety
/// `fut` must point at a live [`FutureObj`].
pub unsafe fn is_cancelled(fut: *mut FutureObj) -> bool {
    let f = unsafe { &*fut };
    f.state.lock().unwrap_or_else(|e| e.into_inner()).cancelled
}

/// Block until one of `futs` is finished or cancelled, and answer which.
///
/// The index, not the value, and it consumes nothing: the caller then `await`s
/// the one that is ready, which costs nothing because it is. That keeps the
/// resolve-once rule intact and composes with `ready()` rather than duplicating
/// it — and it is what lets a deadline be an ordinary member of the list, since
/// `time.after(0.5)` is a future like any other.
///
/// Answers the *lowest* ready index when several are, so a program that puts a
/// timeout last sees real work first on the pass where both arrived.
///
/// `NOT_A_FUTURE` for a list holding something that is not a future, and
/// `NOTHING_TO_WAIT_FOR` for an empty one — waiting for nothing would block
/// forever, which is never what the caller meant.
///
/// # Safety
/// Every element of `futs` must point at a live [`FutureObj`].
pub unsafe fn wait_any(futs: &[*mut FutureObj]) -> i32 {
    if futs.is_empty() {
        return NOTHING_TO_WAIT_FOR;
    }
    let c = completions();
    let p = pool();
    // Announced for the same reason `await` announces: this thread may be one of
    // the pool's own, and it is about to hold none of the CPU it was counted for.
    let had_slot = p.enter_blocking();
    let mut seen = c.lock.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        for (i, &f) in futs.iter().enumerate() {
            let st = unsafe { (*f).state.lock().unwrap_or_else(|e| e.into_inner()) };
            if st.done || st.cancelled {
                drop(st);
                drop(seen);
                p.exit_blocking(had_slot);
                return i as i32;
            }
        }
        seen = c.cv.wait(seen).unwrap_or_else(|e| e.into_inner());
    }
}

/// [`wait_any`]'s answer for an empty list.
pub const NOTHING_TO_WAIT_FOR: i32 = -2;

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
    // Give back a result nobody took.
    //
    // A future is not a container, so there is no cascade — but the one word it
    // holds may be a reference, and this is the last chance to release it.
    // `consumed` is the test: an awaited future handed its reference to the
    // awaiter, an un-awaited one still owns it. Without this, `let f = mk(i)` in
    // a loop leaked one object per iteration whenever the task returned a
    // collection, which is the shape of a fire-and-forget task in a service.
    let orphan = {
        let st = unsafe { (*fut).state.lock().unwrap_or_else(|e| e.into_inner()) };
        if st.consumed {
            0
        } else if st.failed {
            st.error
        } else {
            st.result
        }
    };
    gc::jrt_decref(orphan);
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

/// `jrt_future_ready`'s answer for a word that is not a future.
pub const NOT_A_FUTURE: i32 = -1;

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
    spawn(f, argv, true)
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
        Err(TaskError::Cancelled) => {
            unsafe {
                *failed = 4;
                *err = 0;
            }
            0
        }
    }
}

/// Whether a tagged word is a finished future.
///
/// Three answers rather than two, because a Rust frame cannot raise: `1` done,
/// `0` still running, and `NOT_A_FUTURE` for a word that is not a future at all,
/// which the C forwarder turns into the error the interpreter raises. Same shape
/// as `jrt_len_chunk`, and for the same reason.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_future_ready(word: i64) -> i32 {
    match as_future(word) {
        Some(f) => i32::from(unsafe { is_done(f) }),
        None => NOT_A_FUTURE,
    }
}

/// A future that finishes with nil after `secs`. See [`after`].
/// A future for a tagged seconds word, or `NOT_A_NUMBER` for anything that is
/// not one. The C forwarder raises on that, the way the interpreter does.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_time_after(word: i64) -> i64 {
    match crate::timef::seconds_of(word) {
        Some(secs) => {
            let fut = after(secs);
            if fut.is_null() {
                return NO_TIMER_THREAD;
            }
            JadeValue::from_ptr(fut as *const ()).bits() as i64
        }
        None => NOT_A_NUMBER,
    }
}

/// `jrt_time_after`'s answer for an argument that is not a number. An odd word
/// with the immediate tag, so it can never be mistaken for a real future.
pub const NOT_A_NUMBER: i64 = -1;

/// `jrt_time_after`'s answer when the OS refused the timer thread. Distinct from
/// [`NOT_A_NUMBER`] because the two need different messages: one is a mistake in
/// the program, the other is the machine being out of threads.
pub const NO_TIMER_THREAD: i64 = -3;

/// Stop waiting for a tagged word. `NOT_A_FUTURE` if it is not one.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_future_cancel(word: i64) -> i32 {
    match as_future(word) {
        Some(f) => {
            unsafe { cancel(f) };
            0
        }
        None => NOT_A_FUTURE,
    }
}

/// Whether the task running on this thread has been cancelled.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_task_cancelled() -> i32 {
    i32::from(current_is_cancelled())
}

/// Block until one of `n` tagged words is finished or cancelled; answer which.
///
/// # Safety
/// `words` must point at `n` readable words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_wait_any(words: *const i64, n: i32) -> i32 {
    if n <= 0 || words.is_null() {
        return NOTHING_TO_WAIT_FOR;
    }
    let list = unsafe { core::slice::from_raw_parts(words, n as usize) };
    let mut futs = Vec::with_capacity(list.len());
    for &w in list {
        match as_future(w) {
            Some(f) => futs.push(f),
            None => return NOT_A_FUTURE,
        }
    }
    unsafe { wait_any(&futs) }
}

/// Await every future and report each outcome separately, for `settle = true`.
///
/// `out_vals[i]` is the value a task returned or the value it raised, and
/// `out_ok[i]` says which. Nothing raises here: reporting every outcome is the
/// whole point, and the caller builds the dicts.
///
/// The return is for *misuse* rather than outcome — `NOT_A_FUTURE` for a member
/// that is not one, `DOUBLE_AWAITED` for one already taken. Those are mistakes
/// in the program rather than things a task did, so `settle` does not turn them
/// into data; the C forwarder raises.
///
/// # Safety
/// `words` must point at `n` readable words, and both out-params at `n`
/// writable ones.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_join_settle(
    words: *const i64,
    n: i32,
    out_vals: *mut i64,
    out_ok: *mut i32,
) -> i32 {
    if n <= 0 || words.is_null() {
        return 0;
    }
    let list = unsafe { core::slice::from_raw_parts(words, n as usize) };
    // Every member checked before any is awaited, so a misuse is reported
    // without first blocking on tasks whose results are about to be discarded.
    let mut futs = Vec::with_capacity(list.len());
    for &w in list {
        match as_future(w) {
            Some(f) => futs.push(f),
            None => return NOT_A_FUTURE,
        }
    }
    for (i, &f) in futs.iter().enumerate() {
        let (v, ok) = match unsafe { await_one(f) } {
            Ok(v) => (v, 1),
            Err(TaskError::Raised(e, _)) => (e, 0),
            Err(TaskError::Cancelled) => (crate::value::NIL_BITS as i64, 0),
            Err(TaskError::DoubleAwait) => return DOUBLE_AWAITED,
            Err(TaskError::NotAFuture) => return NOT_A_FUTURE,
        };
        unsafe {
            *out_vals.add(i) = v;
            *out_ok.add(i) = ok;
        }
    }
    0
}

/// `jrt_join_settle`'s answer for a member already awaited.
pub const DOUBLE_AWAITED: i32 = -3;

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
        let f = spawn(double, vec![21], false);
        assert_eq!(unsafe { await_one(f) }, Ok(42));
        unsafe { release(f) };
    }

    #[test]
    fn second_await_is_an_error() {
        let _g = exclusive();
        let f = spawn(double, vec![1], false);
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
        let futs: Vec<_> = [1i64, 2, 3].iter().map(|&n| spawn(double, vec![n], false)).collect();
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
        let futs: Vec<_> = (0..N as i64).map(|n| spawn(echo_double, vec![n], false)).collect();
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
        let futs: Vec<_> = (0..200).map(|_| spawn(double, vec![1], false)).collect();
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

    /// Puts the limit back however the test ends, so one that fails mid-way
    /// does not leave every later test running under its number.
    struct RestoreMaxTasks(usize);

    impl Drop for RestoreMaxTasks {
        fn drop(&mut self) {
            set_max_tasks(self.0);
        }
    }

    fn with_max_tasks(n: usize) -> RestoreMaxTasks {
        let prev = max_tasks();
        set_max_tasks(n);
        RestoreMaxTasks(prev)
    }

    /// The warning has to be once per process. The condition repeats per spawn,
    /// and a program in this state is spawning constantly, so a line per attempt
    /// would bury the run in its own diagnostics.
    ///
    /// This is as close as a test gets to the real thing: making `thread::spawn`
    /// fail needs a machine actually out of threads, which is not something to
    /// arrange from inside the suite.
    #[test]
    fn the_out_of_threads_warning_is_printed_once() {
        let _g = exclusive();
        let before = pool().spawn_failures();
        // Only the very first failure in the process prints. Any earlier test
        // that hit one would have taken that turn, so the first call here is
        // only expected to print when the count is still zero.
        assert_eq!(warn_out_of_threads(), before == 0);
        assert!(!warn_out_of_threads(), "a second failure must stay quiet");
        assert!(!warn_out_of_threads());
        assert_eq!(pool().spawn_failures(), before + 3, "every failure still counts");
    }

    /// The timer thread has to be startable more than once in a process's life.
    /// It used to be a `Once`, which counted the *attempt*: a machine briefly
    /// out of threads at the first `after` left every later `time.after` waiting
    /// on a deadline nothing would fire, turning a transient condition into a
    /// permanent hang.
    #[test]
    fn the_timer_thread_answers_whether_it_is_running() {
        let _g = exclusive();
        assert!(start_timer_thread(), "the timer thread should start");
        assert!(start_timer_thread(), "asking again should find the running one");
    }

    /// A deadline that cannot be armed must not come back as a future, because a
    /// timer future has no body and awaiting one would never return. The null is
    /// what `jrt_time_after` turns into a sentinel the C forwarder can throw on.
    #[test]
    fn after_hands_back_a_real_future_when_the_timer_is_running() {
        let _g = exclusive();
        let fut = after(0.01);
        assert!(!fut.is_null(), "a timer that armed must produce a future");
        // An int word is the simplest thing `seconds_of` accepts, and zero
        // seconds is a deadline already past.
        let armed = jrt_time_after(JadeValue::from_int(0).bits() as i64);
        assert_ne!(armed, NO_TIMER_THREAD, "the timer is running, so this must arm");
        assert_ne!(armed, NOT_A_NUMBER);
        assert_eq!(unsafe { await_one(fut) }, Ok(crate::value::NIL_BITS as i64));
        unsafe { release(fut) };
    }

    #[test]
    fn the_limit_starts_at_the_documented_default() {
        let _g = exclusive();
        assert_eq!(DEFAULT_MAX_TASKS, 32);
        assert_eq!(max_tasks(), DEFAULT_MAX_TASKS, "something changed the limit and left it");
    }

    /// Both ends clamp rather than refuse, and the answer is what was set. A
    /// caller that asks for more than the thread supply allows has to be able to
    /// see that without a second call.
    #[test]
    fn set_max_tasks_clamps_and_answers_with_what_took_effect() {
        let _g = exclusive();
        let _restore = with_max_tasks(DEFAULT_MAX_TASKS);

        assert_eq!(set_max_tasks(8), 8);
        assert_eq!(max_tasks(), 8);

        assert_eq!(set_max_tasks(0), 1, "zero runnable tasks is not a state to allow");
        assert_eq!(set_max_tasks(9999), HARD_MAX_WORKERS);
        assert_eq!(max_tasks(), HARD_MAX_WORKERS);

        // The C entry takes the same path, negatives included.
        assert_eq!(jrt_set_max_tasks(-5), 1);
        assert_eq!(jrt_max_tasks(), 1);
    }

    /// The limit has to bound what actually runs, not merely be stored.
    #[test]
    fn the_limit_bounds_peak_concurrency() {
        let _g = exclusive();
        let _restore = with_max_tasks(2);
        CONCURRENT.store(0, Ordering::SeqCst);
        PEAK.store(0, Ordering::SeqCst);
        let futs: Vec<_> = (0..24).map(|_| spawn(observe_concurrency, vec![], false)).collect();
        for &f in &futs {
            let _ = unsafe { await_one(f) };
        }
        // Awaiting runs a task inline when nobody has claimed it, so the waiter
        // is one more runner than the pool itself would allow.
        let peak = PEAK.load(Ordering::SeqCst);
        assert!(peak <= 3, "24 tasks peaked at {peak} under a limit of 2");
        for f in futs {
            unsafe { release(f) };
        }
    }

    /// Peak simultaneous execution should track `max_tasks` rather than the
    /// number of queued tasks.
    #[test]
    fn concurrency_is_bounded_not_serialized() {
        let _g = exclusive();
        CONCURRENT.store(0, Ordering::SeqCst);
        PEAK.store(0, Ordering::SeqCst);
        let futs: Vec<_> = (0..32).map(|_| spawn(observe_concurrency, vec![], false)).collect();
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
        let inner = spawn(double, vec![n], false);
        let r = unsafe { await_one(inner) }.expect("inner task must resolve");
        unsafe { release(inner) };
        r
    }

    #[test]
    fn nested_await_does_not_deadlock_a_bounded_pool() {
        let _g = exclusive();
        // Comfortably more outer tasks than any plausible task limit, so the
        // deadlock is certain rather than timing-dependent if compensation is
        // removed.
        const OUTER: i64 = 64;

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let futs: Vec<_> =
                (0..OUTER).map(|n| spawn(outer_awaits_inner, vec![n], false)).collect();
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

    /// A task that awaits a task that awaits a task, deeper than the pool can
    /// ever have threads for.
    extern "C" fn chain(args: *mut i64, _n: i32) -> i64 {
        let n = unsafe { *args };
        if n <= 0 {
            return 0;
        }
        let next = spawn(chain, vec![n - 1], false);
        let r = unsafe { await_one(next) }.expect("inner task must resolve");
        unsafe { release(next) };
        r + 1
    }

    /// An `await` chain finishes even when it is longer than the pool's ceiling.
    ///
    /// `nested_await_does_not_deadlock_a_bounded_pool` covers the case growth
    /// alone handles: workers blocked on tasks still in the queue, with room to
    /// spawn replacements. This is the case growth cannot reach. Every level of
    /// the chain parks a thread, so past `HARD_MAX_WORKERS` there is no thread
    /// left to give the innermost body and every level above waits on it
    /// forever. It only completes because an awaiter runs the body itself when
    /// nobody else has claimed it — which is also why the depth here has to
    /// exceed the ceiling rather than merely the limit.
    #[test]
    fn an_await_chain_deeper_than_the_pool_still_finishes() {
        let _g = exclusive();
        const DEPTH: i64 = HARD_MAX_WORKERS as i64 + 64;

        // A thread of its own with room to recurse: the chain now runs on one
        // stack rather than one thread per level, which is the whole point.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                let f = spawn(chain, vec![DEPTH], false);
                let r = unsafe { await_one(f) }.expect("chain must resolve");
                unsafe { release(f) };
                let _ = tx.send(r);
            })
            .expect("driver thread");

        // A hang would otherwise take the whole test binary with it rather than
        // reporting a failure.
        match rx.recv_timeout(Duration::from_secs(120)) {
            Ok(depth) => assert_eq!(depth, DEPTH),
            Err(_) => panic!(
                "an await chain {DEPTH} deep hung: every level parked a thread, the pool \
                 ran out at {HARD_MAX_WORKERS}, and the innermost body had nobody to run it"
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
        // A never-submitted future, so the body it carries is never claimed and
        // nothing but this test holds a reference.
        let job = Job { f: double, args: vec![0], owns_args: false };
        let fut = gc::leak_obj(FutureObj::new(job)) as *mut FutureObj;
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
        let f = spawn(double, vec![5], false);
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
