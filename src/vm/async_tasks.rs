//! Async task machinery for the VM: the future/token-stream handle types and the
//! standalone entry point a spawned task runs on its own `VmState`.
//!
//! The `spawn` / `await` / `join` opcodes themselves are dispatched inline in
//! [`super::dispatch`] (they manipulate register slots), but the types they
//! produce and the owned-state task body live here.

use super::*;

/// Task result type. Paired with a raised-exception slot in [`TaskBundle`] so a
/// parent task can re-raise the child's exception value with the correct type
/// (struct/string) rather than losing it.
pub(crate) type TaskOutput = std::result::Result<VmValue, JadeError>;
pub(crate) type TaskBundle = (TaskOutput, Option<VmValue>);

/// A handle to a spawned async task.  `Arc<JadeFuture>` is `Send + Sync` because
/// `Mutex` makes the inner `Option<JoinHandle>` safe to share across threads.
pub struct JadeFuture {
    pub handle: Mutex<Option<JoinHandle<TaskBundle>>>,
    /// Nobody wants this result any more. See `task::cancel` in `jade-runtime`
    /// for what that does and does not mean: it stops the *waiting*, not the
    /// work, because a compiled task is a real thread with no point at which it
    /// could be interrupted, and the two engines have to agree. So this does not
    /// abort the tokio task either, even though it could.
    pub cancelled: std::sync::atomic::AtomicBool,
}

impl JadeFuture {
    /// Whether the task is over, without waiting for it. Cancelled counts:
    /// nobody is going to read the result either way.
    pub fn is_settled(&self) -> bool {
        if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return true;
        }
        // `None` means an `await` already took the handle, so it is certainly
        // over.
        self.handle.lock().as_ref().is_none_or(|h| h.is_finished())
    }
}

/// Signalled whenever any task finishes.
///
/// `wait` watches several futures at once and cannot park on each one, so every
/// task pokes this on its way out and a waiter rechecks its list. Mirrors the
/// `Completions` condvar the compiled runtime uses, for the same reason.
pub fn completions() -> &'static tokio::sync::Notify {
    static N: std::sync::OnceLock<tokio::sync::Notify> = std::sync::OnceLock::new();
    N.get_or_init(tokio::sync::Notify::new)
}

// ── How many tasks run at once ───────────────────────────────────────────────
//
// The limit itself lives in `jade_runtime::task`, because a compiled binary
// obeys the same number through its own worker pool. What differs is how it is
// enforced. The pool *is* the limit over there: a task runs when a worker picks
// it up. Here there is no pool to size, so the count is kept by hand and the
// limit is consulted on every decision — `set_max_tasks` has to take effect on
// the next task rather than the next run, and a fixed set of semaphore permits
// could not be resized to match.

/// Tasks running right now.
static RUNNING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Signalled whenever a slot is given back, so a task waiting for one rechecks.
fn slots() -> &'static tokio::sync::Notify {
    static N: std::sync::OnceLock<tokio::sync::Notify> = std::sync::OnceLock::new();
    N.get_or_init(tokio::sync::Notify::new)
}

tokio::task_local! {
    /// Whether the task running here is holding a slot.
    ///
    /// A task-local rather than a value threaded through the interpreter,
    /// because the thing that has to give a slot back is `await`, dispatched
    /// deep inside the instruction loop. It cannot be a *thread*-local: a task
    /// may resume on a different worker after every await point, so the flag has
    /// to follow the task rather than the thread that happens to be running it.
    static HOLDS_SLOT: Arc<std::sync::atomic::AtomicBool>;
}

/// Whether this task holds a slot, and set it either way. False outside a task.
fn holds_slot(now: bool) -> bool {
    HOLDS_SLOT.try_with(|h| h.swap(now, std::sync::atomic::Ordering::AcqRel)).unwrap_or(false)
}

/// Take a slot, waiting for one if the limit is already reached.
async fn take_slot() {
    loop {
        // Registered *before* the check, so a slot released between the two
        // still wakes this waiter rather than leaving it parked on an event it
        // already missed. Same reason `wait` builds its `notified` first.
        let freed = slots().notified();
        tokio::pin!(freed);
        freed.as_mut().enable();
        let n = RUNNING.load(std::sync::atomic::Ordering::Acquire);
        if n < jade_runtime::task::max_tasks()
            && RUNNING
                .compare_exchange(
                    n,
                    n + 1,
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                )
                .is_ok()
        {
            return;
        }
        freed.await;
    }
}

/// Give a slot back.
fn give_slot() {
    RUNNING.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    slots().notify_waiters();
}

/// Run `f` without holding a slot, because a parked task is not a running one.
///
/// This is what keeps the limit from deadlocking a program that awaits inside a
/// task. At `set_max_tasks(1)`, a task that awaits a second task holds the only
/// slot, and the child can never start; releasing it across the wait means every
/// slot holder is always making progress toward giving one back. It mirrors the
/// compiled pool's `blocked` count, which excludes parked threads from the same
/// limit for the same reason.
pub(crate) async fn parked<F: std::future::Future>(f: F) -> F::Output {
    if !holds_slot(false) {
        // The top level holds no slot: it is not a task.
        return f.await;
    }
    give_slot();
    let out = f.await;
    take_slot().await;
    holds_slot(true);
    out
}

/// Releases a task's slot however its body ends, a panic included.
struct SlotGuard(Arc<std::sync::atomic::AtomicBool>);

impl Drop for SlotGuard {
    fn drop(&mut self) {
        if self.0.swap(false, std::sync::atomic::Ordering::AcqRel) {
            give_slot();
        }
    }
}

/// Spawn `body` as a task and wrap it in the future the language hands back.
///
/// The one place a VM task is created, so the completion signal and the
/// task-local view of "which future am I" cannot be forgotten by one caller and
/// remembered by another.
///
/// The body stays a runtime *task*, not a thread of its own. That is what makes
/// a deep `await` chain cost nothing: `examples/async/deep_nesting` nests 2,000
/// levels, and each parked level is a suspended future rather than a thread.
/// Giving each body a blocking thread made the same fixture deadlock the moment
/// the chain outgrew the thread pool.
///
/// What keeps a *blocking* task from holding a worker hostage is [`blocking`],
/// called by the builtins that wait on the outside world. So depth costs
/// futures and width costs threads, which is the right way round.
pub fn spawn_task<F>(body: F) -> VmValue
where
    F: std::future::Future<Output = TaskBundle> + Send + 'static,
{
    let fut = Arc::new(JadeFuture {
        handle: Mutex::new(None),
        cancelled: std::sync::atomic::AtomicBool::new(false),
    });
    let mine = Arc::clone(&fut);
    let handle = tokio::spawn(async move {
        // Waiting for a slot happens here, not in the caller: `let f = work()`
        // hands back a future immediately, and a spawn that blocked until the
        // machine was free would make starting a fan-out serial.
        take_slot().await;
        let held = Arc::new(std::sync::atomic::AtomicBool::new(true));
        // Owns the slot from here on, so a body that panics still gives it back.
        let _slot = SlotGuard(Arc::clone(&held));
        let out = HOLDS_SLOT.scope(held, CURRENT_TASK.scope(mine, body)).await;
        completions().notify_waiters();
        out
    });
    *fut.handle.lock() = Some(handle);
    VmValue::Future(fut)
}

/// Run a call that is about to block, without holding a runtime worker hostage.
///
/// A Jade task is a runtime task rather than a thread, so an HTTP request or a
/// `sleep` that simply blocked would stop the worker it landed on from running
/// anything else. The interpreter then ran one task per core whatever `max_tasks`
/// said, and `set_max_tasks(32)` on an eight-core laptop was a promise only the
/// compiled engine could keep: sixteen requests took two waves under `jade run`
/// and one from the same program built.
///
/// `block_in_place` is the runtime's own answer — it moves the worker's other
/// work elsewhere for the duration. It exists only on the multi-threaded
/// runtime, which is what `jade run` builds; the interpreter's unit tests use a
/// current-thread one, where the call would panic and there is no worker to free
/// anyway. So this asks before it uses it.
pub fn blocking<T>(f: impl FnOnce() -> T) -> T {
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current() {
        Ok(h) if h.runtime_flavor() == RuntimeFlavor::MultiThread => tokio::task::block_in_place(f),
        _ => f(),
    }
}

tokio::task_local! {
    /// The future the task running here resolves, so `cancelled()` can answer
    /// without the program passing its own handle around.
    pub(crate) static CURRENT_TASK: Arc<JadeFuture>;
}

/// Whether the task running here has been cancelled. False outside a task,
/// which is the honest answer: the top level is not something anyone cancels.
pub fn current_is_cancelled() -> bool {
    CURRENT_TASK
        .try_with(|f| f.cancelled.load(std::sync::atomic::Ordering::Acquire))
        .unwrap_or(false)
}

/// A lazy token stream from an inference call — and, once drained, the buffer
/// holding what it produced.
///
/// A stream **is a buffer**. Draining one fills `text`, and every later read
/// returns that text rather than failing: reading a stream twice gives the same
/// value twice, exactly as it does for a `yield`ing function's stream. Before
/// v1.2.4 the receiver was taken on first drain and a second read raised
/// `DoubleStreamDrain`, so `print(?p)` followed by any use of the same value
/// was an error rather than the obvious thing.
///
/// The stream also carries the constraints its inference call should run under.
/// That is what let `stream(?p, mute_on=[g])` be deleted: `?p |> g` produces a
/// stream carrying `g`'s grammar and mute anchors, so printing it streams live
/// and mutes, and reading it gives the full text including the muted span.
pub struct JadeTokenStream {
    pub rx: Mutex<Option<tokio::sync::mpsc::Receiver<String>>>,
    pub tokens_handle: Mutex<Option<JoinHandle<Result<()>>>>,
    pub prompt_key: (String, Option<String>),
    /// Set when `?p` creates the stream lazily. Inference starts on first drain,
    /// so the constraints below are in place before the request goes out.
    pub lazy_prompt: Mutex<Option<String>>,
    /// The full text once drained. `Some` means this stream is now a buffer.
    pub text: Mutex<Option<String>>,
    /// GBNF to constrain sampling with, from a `?p |> g` stage.
    pub grammar: Option<String>,
    /// Suppress output from the first token (a Grammar with no anchor).
    pub start_muted: bool,
    /// Strings that enter muted mode on match (a Grammar's `anchor`).
    pub region_start: Vec<String>,
    /// Strings that leave muted mode on match (a Grammar's `stop`).
    pub region_stop: Vec<String>,
}

impl JadeTokenStream {
    /// A lazy stream over `prompt`, with no constraints.
    pub fn lazy(prompt: String) -> Self {
        JadeTokenStream {
            rx: Mutex::new(None),
            tokens_handle: Mutex::new(None),
            prompt_key: (prompt.clone(), None),
            lazy_prompt: Mutex::new(Some(prompt)),
            text: Mutex::new(None),
            grammar: None,
            start_muted: false,
            region_start: Vec::new(),
            region_stop: Vec::new(),
        }
    }

    /// A lazy stream constrained by a Grammar value.
    ///
    /// The mute rule mirrors what `stream(?p, mute_on=[g])` did: an explicit
    /// anchor starts a muted region that a `stop` ends, and a Grammar with no
    /// anchor means the whole reply is structured output, so suppression starts
    /// at the first token.
    pub fn constrained(prompt: String, g: &jade_runtime::grammarf::GrammarObj) -> Self {
        let mut s = Self::lazy(prompt);
        s.grammar = Some(g.to_gbnf());
        match (&g.anchor, &g.stop) {
            (Some(a), stop) => {
                s.region_start = vec![a.clone()];
                s.region_stop = stop.iter().cloned().collect();
            }
            (None, stop) => {
                s.start_muted = true;
                s.region_stop = stop.iter().cloned().collect();
            }
        }
        s
    }
}

impl Drop for JadeFuture {
    fn drop(&mut self) {
        // Abort any un-awaited task so it does not run forever as a detached thread.
        let guard = self.handle.get_mut();
        if let Some(handle) = guard.take() {
            handle.abort();
        }
    }
}

pub(crate) async fn call_value_standalone(
    callee: VmValue,
    args: Vec<VmValue>,
    mut state: VmState,
    span: Span,
) -> TaskBundle {
    // `call_value_body`, not `call_value`: this *is* the task, so treating an
    // async callee as async again would spawn a second one and hand the awaiter
    // a future where it expects a value.
    let result = call_value_body(callee, args, &mut state, span).await;
    let raised = state.raised_exception.take();
    (result, raised)
}

/// The same, for a task started from a function *value* — see `call_value`.
pub(crate) async fn call_fn_standalone(
    cf: std::sync::Arc<crate::bytecode::CompiledFn>,
    args: Vec<VmValue>,
    mut state: VmState,
    span: Span,
) -> TaskBundle {
    let result = call_fn(&cf, args, &mut state, span).await;
    let raised = state.raised_exception.take();
    (result, raised)
}
