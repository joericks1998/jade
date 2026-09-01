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

/// Spawn `body` as a task and wrap it in the future the language hands back.
///
/// The one place a VM task is created, so the completion signal and the
/// task-local view of "which future am I" cannot be forgotten by one caller and
/// remembered by another.
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
        let out = CURRENT_TASK.scope(mine, body).await;
        completions().notify_waiters();
        out
    });
    *fut.handle.lock() = Some(handle);
    VmValue::Future(fut)
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
