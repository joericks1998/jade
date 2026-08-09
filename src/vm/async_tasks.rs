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
    let result = call_value(callee, args, &mut state, span).await;
    let raised = state.raised_exception.take();
    (result, raised)
}
