//! Exception-value construction for the VM.
//!
//! The `try`/`catch`/`raise` control flow itself is dispatched inline in
//! [`super::dispatch`] (it manipulates the handler stack and register slots).
//! What lives here is the shaping of a built-in error into a catchable Jade
//! value so a `catch` block can bind and inspect it.

use super::*;

/// Build a `RuntimeError { message }` struct value for wrapping built-in errors
/// when they are caught by a `try/catch` block.
pub(crate) fn make_vm_runtime_error(message: String) -> VmValue {
    let mut sobj = StructObj::<VmValue>::new("RuntimeError");
    sobj.set_field("message", VmValue::Str(message.into()));
    VmValue::Struct(Arc::new(Mutex::new(sobj)))
}
