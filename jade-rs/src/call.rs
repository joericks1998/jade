//! What every exported call runs inside.
//!
//! The wrapper `#[jade::export]` writes for a function lifts each argument, calls the
//! function, and lowers the result, all inside [`call`]. That catches a panic so it
//! never unwinds into the Jade program's frames, and turns an `Err`, a panic, or an
//! argument Rust refuses into status 1 with the message in `err`. The program raises
//! that message, then calls the function's `__free` companion, which runs [`free`].
//!
//! Rust's default panic hook prints a panic before it is caught. So the first call
//! installs a hook that stays quiet while an exported call is running on its thread,
//! and passes any other panic to the hook that was there before: a panic the package
//! catches itself, or one on a thread of its own, still prints.

use std::cell::Cell;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Once;

use crate::c::CVec;
use crate::convert::{Lift, Lower, Return};

thread_local! {
    /// How many exported calls are running on this thread.
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Installs the quiet panic hook, once per process.
static HOOK: Once = Once::new();

/// Purpose: This struct marks an exported call as running on this thread for as long
/// as it lives.
struct Running;

impl Running {
    fn enter() -> Self {
        HOOK.call_once(|| {
            let before = panic::take_hook();
            panic::set_hook(Box::new(move |info| {
                if DEPTH.with(Cell::get) == 0 {
                    before(info);
                }
            }));
        });
        DEPTH.with(|d| d.set(d.get() + 1));
        Running
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

/// Purpose: This function reads one argument out of its C form.
///
/// Inputs:
/// - `p`: the pointer the Jade program passed.
///
/// Outputs: the value, or the message to raise.
///
/// # Safety
///
/// `p` must point at a C form of `T`'s Jade type, valid for `'a`.
pub unsafe fn lift<'a, T: Lift<'a>>(p: *const T::C) -> Result<T, String> {
    T::lift(&*p)
}

/// Purpose: This function runs one exported call.
///
/// Inputs:
/// - `what`: the function as a Jade program writes it, `imgtools.resize`, for the
///   message a panic raises.
/// - `out`: where the result's C form goes.
/// - `err`: where the message goes when the call raises.
/// - `f`: lifts the arguments and calls the function.
///
/// Outputs: 0 when `out` holds the result, 1 when `err` holds a message.
///
/// # Safety
///
/// `out` and `err` must be valid for writes of their types.
pub unsafe fn call<R: Return>(
    what: &str,
    out: *mut R::C,
    err: *mut CVec<u8>,
    f: impl FnOnce() -> Result<R, String>,
) -> i32 {
    let running = Running::enter();
    let result = panic::catch_unwind(AssertUnwindSafe(|| f().and_then(Return::lower_return)));
    drop(running);
    let message = match result {
        Ok(Ok(c)) => {
            out.write(c);
            return 0;
        }
        Ok(Err(m)) => m,
        Err(payload) => {
            let m = match payload.downcast_ref::<&str>() {
                Some(s) => (*s).to_string(),
                None => match payload.downcast_ref::<String>() {
                    Some(s) => s.clone(),
                    None => "a panic with no message".to_string(),
                },
            };
            // A payload whose drop panics must not unwind out of here either.
            let _ = panic::catch_unwind(AssertUnwindSafe(move || drop(payload)));
            format!("{what} panicked: {m}")
        }
    };
    err.write(message.lower());
    1
}

/// Purpose: This function releases what one call handed over.
///
/// Inputs:
/// - `out`: the result's C form, or null when the call raised.
/// - `err`: the message, or null when the call returned.
///
/// Outputs: none.
///
/// # Safety
///
/// Each pointer must be null or hold what [`call`] wrote, not freed already.
pub unsafe fn free<R: Return>(out: *mut R::C, err: *mut CVec<u8>) {
    // Dropping a package's own values runs its code, which must not unwind either.
    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
        if !out.is_null() {
            R::free(&mut *out);
        }
        if !err.is_null() {
            <String as Lower>::free(&mut *err);
        }
    }));
}
