//! Reference counting and scope exit.
//!
//! Split out of the former monolithic `lower.rs`; see this directory's README.

use super::*;

impl<'a, 'ctx> Lowerer<'a, 'ctx> {
    /// An `i1` that is true when `w` is a **heap** word — a pointer (dict/array/
    /// struct/fn/future), a boxed float, or a string, i.e. tag ∈ {1,3,5}. Ints
    /// (low bit 0) and immediates (nil/bool, tag 7) are non-heap and never need
    /// refcounting. Equivalent to `runtime::value::is_heap`: odd **and** tag ≠ 7.
    ///
    /// This is the cheap inline test the refcount ops branch on, so a plain
    /// integer program (e.g. recursive `fib`, whose `Unknown`-typed values are
    /// ints at runtime) skips the runtime call entirely instead of paying a
    /// function call that no-ops inside the callee.
    pub(super) fn is_heap(&self, w: IntValue<'ctx>) -> IntValue<'ctx> {
        let b = self.builder;
        let one = self.i64t().const_int(1, false);
        let seven = self.i64t().const_int(7, false);
        let low1 = b.build_and(w, one, "rc_low1").unwrap();
        let is_odd = b
            .build_int_compare(IntPredicate::NE, low1, self.i64t().const_zero(), "rc_odd")
            .unwrap();
        let low3 = b.build_and(w, seven, "rc_low3").unwrap();
        let not_imm = b.build_int_compare(IntPredicate::NE, low3, seven, "rc_nonimm").unwrap();
        b.build_and(is_odd, not_imm, "rc_isheap").unwrap()
    }

    /// Run `emit_call` only when `w` is a heap word; otherwise fall straight
    /// through. Replaces an unconditional refcount call with a predicted-not-taken
    /// inline branch for the common non-heap (int/bool/nil) case.
    pub(super) fn if_heap(&self, w: IntValue<'ctx>, emit_call: impl FnOnce()) {
        let func = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let heap_bb = self.ctx.append_basic_block(func, "rc_heap");
        let cont_bb = self.ctx.append_basic_block(func, "rc_cont");
        let cond = self.is_heap(w);
        self.builder.build_conditional_branch(cond, heap_bb, cont_bb).unwrap();
        self.builder.position_at_end(heap_bb);
        emit_call();
        self.builder.build_unconditional_branch(cont_bb).unwrap();
        self.builder.position_at_end(cont_bb);
    }

    /// Emit `jrt_incref(w)` — retain a reference. No-op on non-collection words.
    pub(super) fn incref(&self, w: IntValue<'ctx>) {
        if !self.refcount {
            return;
        }
        self.if_heap(w, || {
            let f = self.runtime_fn(
                "jrt_incref",
                self.ctx.void_type().fn_type(&[self.i64t().into()], false),
            );
            self.builder.build_call(f, &[w.into()], "").unwrap();
        });
    }

    /// Emit `jrt_decref(w)` — release a reference (frees at zero, cascading).
    pub(super) fn decref(&self, w: IntValue<'ctx>) {
        if !self.refcount {
            return;
        }
        self.if_heap(w, || {
            let f = self.runtime_fn(
                "jrt_decref",
                self.ctx.void_type().fn_type(&[self.i64t().into()], false),
            );
            self.builder.build_call(f, &[w.into()], "").unwrap();
        });
    }

    /// Retain a value that is a *borrowed* read of an existing reference (a
    /// `Move`/`GetLocal`/`GetGlobal`/`GetIndex`/`GetField` result): the destination
    /// slot becomes a new owner, so the count must rise. Producer/call results are
    /// already owned and must NOT be routed through here.
    pub(super) fn retain(&self, w: IntValue<'ctx>) {
        self.incref(w);
    }

    /// Before slot `i` is overwritten with `new`, release whatever reference it
    /// held (via `jrt_rc_replace`, which skips the release when `old == new` — the
    /// in-place array-mutation case). No-op unless refcounting is on.
    pub(super) fn rc_replace_slot(&self, i: usize, new: IntValue<'ctx>) {
        if !self.refcount {
            return;
        }
        let old =
            self.builder.build_load(self.i64t(), self.slots[i], "rcold").unwrap().into_int_value();
        // `jrt_rc_replace` decrefs `old` and increfs `new`; both no-op unless the
        // word is heap. Skip the call when *neither* is heap (the common case for
        // scalar slots) with an inline guard — `is_heap(old) || is_heap(new)`.
        let old_heap = self.is_heap(old);
        let new_heap = self.is_heap(new);
        let either = self.builder.build_or(old_heap, new_heap, "rc_either").unwrap();
        let func = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let heap_bb = self.ctx.append_basic_block(func, "rcrep_heap");
        let cont_bb = self.ctx.append_basic_block(func, "rcrep_cont");
        self.builder.build_conditional_branch(either, heap_bb, cont_bb).unwrap();
        self.builder.position_at_end(heap_bb);
        let f = self.runtime_fn(
            "jrt_rc_replace",
            self.ctx.void_type().fn_type(&[self.i64t().into(), self.i64t().into()], false),
        );
        self.builder.build_call(f, &[old.into(), new.into()], "").unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();
        self.builder.position_at_end(cont_bb);
    }

    /// Release every local slot's owned reference — the function's scope-exit
    /// cleanup, emitted immediately before each `return`. Parameter slots
    /// (`0..n_params`) are borrowed from the caller and left untouched.
    pub(super) fn emit_scope_exit(&self) {
        if !self.refcount {
            return;
        }
        for i in self.n_params..self.slots.len() {
            let v = self.load_idx(i);
            self.decref(v);
        }
    }
}
