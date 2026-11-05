//! Runtime errors: what a program does when an operation cannot go on.
//!
//! An int that overflows, an index past the end, a key that is not there: each
//! reports `jade: <message>` on standard error and exits with status 1, which
//! is exactly what v1 printed for an error nothing caught. Exceptions are the
//! next phase of the static backend; when they land, this is the one place that
//! turns these failures into a raised `RuntimeError` instead.

use inkwell::values::{FunctionValue, IntValue, PointerValue};

use super::{Cx, F};

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns `jade.fail(msg: str)`, which reports a
    /// runtime error and stops the program.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the function.
    pub fn fail_fn(&self) -> FunctionValue<'ctx> {
        let ptr = self.ptrt();
        let f = self.support("jade.fail", None, &[ptr.into()], |f| {
            let msg = f.param_ptr(0);
            let prefix = f.cx.lit("jade: ");
            let nl = f.cx.lit("\n");
            for s in [prefix, msg, nl] {
                f.write_str(2, s);
            }
            f.call(f.cx.libc("exit"), &[f.i32(1).into()]);
            f.unreachable();
        });
        let noreturn = inkwell::attributes::Attribute::get_named_enum_kind_id("noreturn");
        let attr = self.ctx.create_enum_attribute(noreturn, 0);
        f.add_attribute(inkwell::attributes::AttributeLoc::Function, attr);
        f
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function reports a fixed runtime error and stops.
    ///
    /// Inputs:
    /// - `msg`: the message, without the `jade: ` prefix.
    ///
    /// Outputs: none. The current block ends.
    pub fn fail(&self, msg: &str) {
        let m = self.cx.lit(msg);
        self.fail_str(m);
    }

    /// Purpose: This function reports a runtime error built at run time.
    ///
    /// Inputs:
    /// - `msg`: a Jade string holding the message.
    ///
    /// Outputs: none. The current block ends.
    pub fn fail_str(&self, msg: PointerValue<'ctx>) {
        self.call(self.cx.fail_fn(), &[msg.into()]);
        self.unreachable();
    }

    /// Purpose: This function fails when a condition holds.
    ///
    /// Inputs:
    /// - `c`: the condition.
    /// - `msg`: the message.
    ///
    /// Outputs: none.
    pub fn fail_if(&self, c: IntValue<'ctx>, msg: &str) {
        self.if_(c, |f| f.fail(msg));
    }
}
