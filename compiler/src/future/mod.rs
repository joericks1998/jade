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

use crate::typechecker::type_infer::TypeContext;

/// The methods a `future` receiver has, by name.
pub const FUTURE_METHODS: &[&str] = &["ready", "cancel"];

pub fn register_future_method_types(ctx: &mut TypeContext) {
    use crate::typechecker::tir::JadeType;
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
