//! Exception frames: throw, setjmp, and frame push/pop.
//!
//! Split out of the former monolithic `lower.rs`; see this directory's README.

use super::*;

impl<'a, 'ctx> Lowerer<'a, 'ctx> {
    /// Raise `value` (a tagged word) as an untyped exception via the runtime's
    /// `jade_exc_throw_typed(value, NULL)`, then close the block with
    /// `unreachable` (throw is noreturn — it longjmps to the active handler or
    /// aborts). Callers must not emit anything after this in the same block.
    pub(super) fn throw(&self, value: IntValue<'ctx>) -> Result<(), String> {
        let void = self.ctx.void_type();
        let f = self.runtime_fn(
            "jade_exc_throw_typed",
            void.fn_type(&[self.i64t().into(), self.ptrt().into()], false),
        );
        self.builder
            .build_call(f, &[value.into(), self.ptrt().const_null().into()], "")
            .map_err(|e| e.to_string())?;
        self.builder.build_unreachable().map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Raise one of codegen's *own* failures (a zero divisor, integer overflow)
    /// as a `RuntimeError` struct, the way the VM delivers every runtime error.
    ///
    /// Distinct from `throw`, which raises a value verbatim — correct for a
    /// user's `raise x`, where the thrown value is whatever they wrote, and
    /// wrong for an error the runtime itself produced. Raising those as bare
    /// strings meant `catch e` bound a str compiled and a struct interpreted,
    /// so `e.message` and `catch RuntimeError e` both broke under `jade build`.
    ///
    /// Noreturn, like `throw`: the block is closed with `unreachable`.
    pub(super) fn throw_runtime(&self, msg: &str) -> Result<(), String> {
        let f = self.runtime_fn(
            "jrt_throw_runtime",
            self.ctx.void_type().fn_type(&[self.ptrt().into()], false),
        );
        self.builder.build_call(f, &[self.cstr(msg).into()], "").map_err(|e| e.to_string())?;
        self.builder.build_unreachable().map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Declare (once) `setjmp(ptr) -> i32` with the `returns_twice` attribute —
    /// without it LLVM may hoist code across the call and miscompile the second
    /// (longjmp) return.
    pub(super) fn setjmp_fn(&self) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("setjmp") {
            return f;
        }
        let ty = self.ctx.i32_type().fn_type(&[self.ptrt().into()], false);
        let f = self.module.add_function("setjmp", ty, None);
        let id = inkwell::attributes::Attribute::get_named_enum_kind_id("returns_twice");
        let attr = self.ctx.create_enum_attribute(id, 0);
        f.add_attribute(AttributeLoc::Function, attr);
        f
    }

    pub(super) fn push_frame(&self, buf: PointerValue<'ctx>) {
        let f = self.runtime_fn(
            "jade_exc_push_frame",
            self.ctx.void_type().fn_type(&[self.ptrt().into()], false),
        );
        self.builder.build_call(f, &[buf.into()], "").unwrap();
    }

    /// Restore the handler depth captured in the prologue. Emitted on every
    /// return from a function containing a `try`.
    ///
    /// The emitter only emits `PopHandler` on the try body's normal fall-through
    /// exit, so `try { …; return x } catch e { … }` returned with its frame
    /// still on the stack while the `jmp_buf` it points at died with the frame.
    /// The next `raise` then longjmp'd into dead stack. This is the AOT analogue
    /// of the VM keeping its `handlers` vec per call frame.
    pub(super) fn emit_exc_restore(&self) -> Result<(), String> {
        let Some(slot) = self.exc_depth_slot else { return Ok(()) };
        let i32_ty = self.ctx.i32_type();
        let saved = self
            .builder
            .build_load(i32_ty, slot, "exc_saved")
            .map_err(|e| e.to_string())?
            .into_int_value();
        let f = self
            .runtime_fn("jade_exc_restore", self.ctx.void_type().fn_type(&[i32_ty.into()], false));
        self.builder.build_call(f, &[saved.into()], "").map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Restore the recursion-depth counter to this function's entry snapshot.
    /// Unlike [`Self::emit_exc_restore`] this runs unconditionally — every
    /// function opened a frame in its prologue (`jrt_recur_enter`), not just
    /// ones with a `try` — on every return path *and* every exception landing
    /// pad, so a caught raise from deep in a call chain does not leave the
    /// counter stuck high from callee frames a longjmp skipped past.
    pub(super) fn emit_recur_restore(&self) -> Result<(), String> {
        let Some(slot) = self.recur_depth_slot else { return Ok(()) };
        let i32_ty = self.ctx.i32_type();
        let saved = self
            .builder
            .build_load(i32_ty, slot, "recur_saved")
            .map_err(|e| e.to_string())?
            .into_int_value();
        let f = self
            .runtime_fn("jrt_recur_restore", self.ctx.void_type().fn_type(&[i32_ty.into()], false));
        self.builder.build_call(f, &[saved.into()], "").map_err(|e| e.to_string())?;
        Ok(())
    }

    pub(super) fn pop_frame(&self) {
        let f = self.runtime_fn("jade_exc_pop", self.ctx.void_type().fn_type(&[], false));
        self.builder.build_call(f, &[], "").unwrap();
    }

    /// The currently-thrown value word (`jade_exc_value()`), read inside a
    /// handler's landing block.
    pub(super) fn exc_value(&self) -> IntValue<'ctx> {
        let f = self.runtime_fn("jade_exc_value", self.i64t().fn_type(&[], false));
        self.builder.build_call(f, &[], "excv").unwrap().as_any_value_enum().into_int_value()
    }
}
