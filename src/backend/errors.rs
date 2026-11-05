//! Raised errors: the thread's error slot, raising, and reporting.
//!
//! A raised error is a class value kept in a thread-local slot, `jade.err`.
//! Raising puts one there; the code after every operation that can raise
//! checks the slot and, when it is full, goes to the nearest `catch` or leaves
//! the function, releasing its locals on the way (see `mir::lower`). Nothing
//! unwinds the stack behind the program's back, so reference counts stay exact
//! and no unwinding library is linked.
//!
//! A runtime failure, such as an int overflowing or an index past the end,
//! raises a `RuntimeError` carrying the message v1 used. Support code that
//! fails raises and returns a zero value at once, since its caller checks the
//! slot next; code lowered from MIR raises and carries on to that check.
//!
//! An error nothing catches reaches the top of the program, which reports it
//! the way v1 did, `jade: <message>`, and exits with status 1.

use inkwell::module::Linkage;
use inkwell::values::{FunctionValue, GlobalValue, IntValue, PointerValue};

use super::layout::{self, TYPE_ID};
use super::{Cx, F};
use crate::mir::Ty;

/// The deepest a chain of calls may go before it raises, as in v1.
pub const RECURSION_LIMIT: i64 = 10_000;

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns a thread-local global, made the first time.
    ///
    /// Inputs:
    /// - `name`: the global's name.
    /// - `ptr`: true for a pointer slot, false for an i64 counter.
    ///
    /// Outputs: the global.
    fn thread_global(&self, name: &str, ptr: bool) -> GlobalValue<'ctx> {
        if let Some(g) = self.module.get_global(name) {
            return g;
        }
        let g = if ptr {
            let g = self.module.add_global(self.ptrt(), None, name);
            g.set_initializer(&self.ptrt().const_null());
            g
        } else {
            let g = self.module.add_global(self.i64t(), None, name);
            g.set_initializer(&self.i64t().const_zero());
            g
        };
        g.set_thread_local(true);
        g.set_linkage(Linkage::Internal);
        g
    }

    /// Purpose: This function returns the thread's error slot.
    pub fn err_slot(&self) -> PointerValue<'ctx> {
        self.thread_global("jade.err", true).as_pointer_value()
    }

    /// Purpose: This function returns the thread's call depth counter.
    pub fn depth_slot(&self) -> PointerValue<'ctx> {
        self.thread_global("jade.depth", false).as_pointer_value()
    }

    /// Purpose: This function returns `jade.set_err(e)`, which raises a class
    /// value it takes the reference of, replacing any error already there.
    pub fn set_err_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.set_err", None, &[self.ptrt().into()], |f| {
            let slot = f.cx.err_slot();
            let old = f.load_ptr(slot);
            f.store(slot, f.param_ptr(0));
            f.release(&Ty::Error, old.into());
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.raise_msg(msg)`, which raises a
    /// `RuntimeError` carrying the message. The message is borrowed.
    pub fn raise_msg_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.raise_msg", None, &[self.ptrt().into()], |f| {
            let msg = f.param_ptr(0);
            let st = f.cx.struct_ty("RuntimeError");
            let size = f.size_of(st.into());
            let p = f.call_ptr(f.cx.libc("malloc"), &[size.into()]);
            f.store(p, f.i64(1));
            f.store(f.at_c(p, TYPE_ID), f.i64(f.cx.struct_id("RuntimeError") as i64));
            f.retain(&Ty::Str, msg.into());
            f.store(f.field(st, p, 2), msg);
            f.call(f.cx.set_err_fn(), &[p.into()]);
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.error_message(e) -> str`: a
    /// `RuntimeError`'s message, the `message` field of a class with a string
    /// one, or `default` text otherwise.
    ///
    /// Inputs:
    /// - `uncaught`: true for the text an uncaught error reports, which names
    ///   the class (`uncaught Name`); false for `e.message`, which gives the
    ///   value's printed form.
    ///
    /// Outputs: the function. Its result is owned.
    pub fn error_message_fn(&self, uncaught: bool) -> FunctionValue<'ctx> {
        let name = if uncaught { "jade.error_report" } else { "jade.error_message" };
        self.support(name, Some(self.ptrt().into()), &[self.ptrt().into()], |f| {
            let e = f.param_ptr(0);
            let tid = f.type_id(e);
            let default = f.block();
            let mut cases = Vec::new();
            let mut bodies = Vec::new();
            for (id, s) in f.cx.structs.iter().enumerate() {
                if let Some(i) = s.fields.iter().position(|(n, t)| n == "message" && *t == Ty::Str)
                {
                    let b = f.block();
                    cases.push((f.i64(id as i64), b));
                    bodies.push((b, s.name.clone(), i as u32));
                }
            }
            f.switch(tid, default, &cases);
            for (b, class, i) in bodies {
                f.position(b);
                let m = f.load_ptr(f.field(f.cx.struct_ty(&class), e, i + 2));
                f.retain(&Ty::Str, m.into());
                f.ret(m);
            }
            f.position(default);
            if uncaught {
                let n = f.call_ptr(f.cx.class_name_fn(), &[tid.into()]);
                let out = f.str_cat(&[f.cx.lit("uncaught "), n]);
                f.ret(out);
            } else {
                f.ret(f.cx.lit("<struct>"));
            }
        })
    }

    /// Purpose: This function returns `jade.class_name(id) -> str`: a class's
    /// name as the program wrote it.
    fn class_name_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.class_name", Some(self.ptrt().into()), &[self.i64t().into()], |f| {
            let id = f.param_int(0);
            let default = f.block();
            let mut cases = Vec::new();
            let mut bodies = Vec::new();
            for (i, s) in f.cx.structs.iter().enumerate() {
                let b = f.block();
                cases.push((f.i64(i as i64), b));
                bodies.push((b, crate::compiler::infer::display_type_name(&s.name).to_string()));
            }
            f.switch(id, default, &cases);
            for (b, n) in bodies {
                f.position(b);
                f.ret(f.cx.lit(&n));
            }
            f.position(default);
            f.ret(f.cx.lit("error"));
        })
    }

    /// Purpose: This function returns `jade.report_uncaught()`, which reports
    /// an error left in the slot and exits with status 1, or does nothing.
    pub fn report_uncaught_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.report_uncaught", None, &[], |f| {
            let e = f.load_ptr(f.cx.err_slot());
            f.if_(f.not(f.is_null(e)), |f| {
                let m = f.call_ptr(f.cx.error_message_fn(true), &[e.into()]);
                for s in [f.cx.lit("jade: "), m, f.cx.lit("\n")] {
                    f.write_str(2, s);
                }
                f.call(f.cx.libc("exit"), &[f.i32(1).into()]);
                f.unreachable();
            });
            f.ret_void();
        })
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function raises a runtime error from support code and
    /// returns from the support function at once with a zero value, since its
    /// caller checks the error slot next.
    ///
    /// Inputs:
    /// - `msg`: the message, without the `jade: ` prefix.
    ///
    /// Outputs: none. The current block ends.
    pub fn fail(&self, msg: &str) {
        self.raise(msg);
        self.ret_zero();
    }

    /// Purpose: This function raises a runtime error built at run time from
    /// support code, and returns at once. The message is owned and released.
    pub fn fail_str(&self, msg: PointerValue<'ctx>) {
        self.call(self.cx.raise_msg_fn(), &[msg.into()]);
        self.release(&Ty::Str, msg.into());
        self.ret_zero();
    }

    /// Purpose: This function fails from support code when a condition holds.
    pub fn fail_if(&self, c: IntValue<'ctx>, msg: &str) {
        self.if_(c, |f| f.fail(msg));
    }

    /// Purpose: This function raises a runtime error and carries on, for code
    /// lowered from MIR, which checks the error slot after the operation.
    pub fn raise(&self, msg: &str) {
        let m = self.cx.lit(msg);
        self.call(self.cx.raise_msg_fn(), &[m.into()]);
    }

    /// Purpose: This function raises a runtime error built at run time and
    /// carries on. The message is owned and released.
    pub fn raise_str(&self, msg: PointerValue<'ctx>) {
        self.call(self.cx.raise_msg_fn(), &[msg.into()]);
        self.release(&Ty::Str, msg.into());
    }

    /// Purpose: This function raises when a condition holds, and carries on.
    pub fn raise_if(&self, c: IntValue<'ctx>, msg: &str) {
        self.if_(c, |f| f.raise(msg));
    }

    /// Purpose: This function returns a zero value of the function's own
    /// return type, or nothing for a void function.
    pub fn ret_zero(&self) {
        match self.func.get_type().get_return_type() {
            Some(t) => self.ret(layout::zero(t)),
            None => self.ret_void(),
        }
    }

    /// Purpose: This function says whether an error is waiting in the slot.
    pub fn err_pending(&self) -> IntValue<'ctx> {
        let e = self.load_ptr(self.cx.err_slot());
        self.not(self.is_null(e))
    }
}
