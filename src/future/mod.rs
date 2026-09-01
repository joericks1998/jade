//! `future`: the one thing you can ask a task without waiting for it.
//!
//! `await` blocks. That is the right default and it is also the whole problem
//! for anything with a loop it cannot stop: a render loop has no point at which
//! it is willing to freeze, so it can start work concurrently and then never
//! find out the answer. `f.ready()` is the missing half — a plain bool, no
//! waiting, so a loop can check on each pass and await only once the answer is
//! already sitting there.
//!
//! ```jade
//! let pending = connect(ssid)
//! let shown = false
//! while running {
//!     draw_frame()
//!     if !shown && pending.ready() {
//!         show_banner(await pending)
//!         shown = true
//!     }
//! }
//! ```
//!
//! The `await` after a true `ready()` costs nothing, which is what makes the
//! shape work rather than merely read well — it falls straight through the wait.
//!
//! ## Why there is no package form
//!
//! Every other primitive method has one: `s.upper()` is `string.upper(s)`. That
//! rule is about *package* functions, which have a receiver-first spelling to
//! mirror. A future is not a package and has no other functions, so a
//! `std/future` holding one name would be a package invented for the symmetry
//! rather than for anything a program wants to import.

use crate::builtins::BuiltinFn;
use crate::compiler::type_infer::TypeContext;
use crate::frontend::error::{JadeError, Result};
use crate::vm::VmValue;

/// `f.ready()` — has the task finished?
///
/// True once the task is over, however it ended: a task that raised is finished,
/// and the `await` that follows re-raises. A future whose result has already
/// been taken is finished too, and the second `await` reports the double await
/// on its own terms. Neither is this function's business to hide.
fn future_ready(args: &[VmValue]) -> Result<VmValue> {
    let span = crate::frontend::error::Span { line: 0, col: 0 };
    match args.first() {
        Some(VmValue::Future(f)) => {
            // `None` means the handle was taken by an `await` that has already
            // returned, so the task is certainly over.
            Ok(VmValue::Bool(f.is_settled()))
        }
        _ => Err(JadeError::NotAFuture { span }),
    }
}

/// `f.cancel()` — stop waiting for a task.
///
/// It does not stop the work, and saying so plainly is the whole design. A
/// compiled task is a real thread running straight-line code with no point at
/// which the runtime could interrupt it, so a `cancel` that claimed to kill the
/// task would be a promise only one engine could keep. What it changes is the
/// caller's side: `await` raises at once instead of blocking, and one already
/// blocked wakes and raises.
///
/// A task that wants to give up early cooperates by checking `cancelled()`.
/// That is the only thing that actually stops work, and it is the task's choice.
///
/// Cancelling twice, or cancelling something already finished, is not an error.
/// The second is the ordinary race: the answer arrived while the caller was
/// deciding it no longer wanted it, and it does not get it.
fn future_cancel(args: &[VmValue]) -> Result<VmValue> {
    let span = crate::frontend::error::Span { line: 0, col: 0 };
    match args.first() {
        Some(VmValue::Future(f)) => {
            f.cancelled.store(true, std::sync::atomic::Ordering::Release);
            // Wake anyone waiting on several futures at once, who is otherwise
            // parked until something *finishes*.
            crate::vm::async_tasks::completions().notify_waiters();
            Ok(VmValue::Nil)
        }
        _ => Err(JadeError::NotAFuture { span }),
    }
}

pub fn find_future_method(method: &str) -> Option<BuiltinFn> {
    match method {
        "ready" => Some(BuiltinFn { name: "ready", vm_impl: future_ready }),
        "cancel" => Some(BuiltinFn { name: "cancel", vm_impl: future_cancel }),
        _ => None,
    }
}

pub fn register_future_method_types(ctx: &mut TypeContext) {
    use crate::compiler::tir::JadeType;
    ctx.define_primitive_method(
        "future",
        "ready",
        JadeType::Fn { params: vec![], ret: Box::new(JadeType::Bool) },
    );
    ctx.define_primitive_method(
        "future",
        "cancel",
        JadeType::Fn { params: vec![], ret: Box::new(JadeType::Nil) },
    );
}

#[cfg(test)]
mod tests;
