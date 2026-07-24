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
}

/// A lazy, in-flight token stream from an inference call.
/// Wrapping in `Arc` makes it cloneable as a `VmValue`; the interior `Option`
/// enforces single-drain semantics — taking `None` on a second drain is an error.
pub struct JadeTokenStream {
    pub rx: Mutex<Option<tokio::sync::mpsc::Receiver<String>>>,
    pub tokens_handle: Mutex<Option<JoinHandle<Result<()>>>>,
    pub prompt_key: (String, Option<String>),
    /// Set when `?p` creates the stream lazily. Inference starts on first drain
    /// so callers (e.g. `stream()`) can inject grammar constraints first.
    pub lazy_prompt: Mutex<Option<String>>,
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
    let result = call_value(callee, args, &mut state, span).await;
    let raised = state.raised_exception.take();
    (result, raised)
}

