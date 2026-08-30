//! Reference counting and scope exit.
//!
//! ## The invariant every `TAG_PTR` value has to hold up
//!
//! `jrt_incref` / `jrt_decref` (and the destructor's child cascade) dispatch on
//! the `ObjKind` byte at offset 8, so **anything that puts a new kind of value
//! in a `TAG_PTR` word must put an `ObjKind` there.** Today every producer
//! does: collections (Array/Dict/Struct) and futures carry a real `ObjHeader`,
//! grammar objects carry `ObjKind::Grammar`, and ordinary fn boxes
//! (`fn_box_word`) and native fn values (`emit_native_fn_value`) carry
//! `ObjKind::Fn` so the refcount ops recognise them and no-op. A prompt is not
//! a heap kind at all — `MakePrompt` stores the underlying string, and a
//! `TAG_STR` word is rejected by tag before any header is read.
//!
//! Get it wrong and the failure is silent: the last holdout was the native fn
//! value, whose offset 8 held the `env` pointer, and a heap address whose low
//! byte happened to be 2/3/4 would have sent `free_obj` off to reclaim it as an
//! Array/Dict/Struct.
//!
//! Codegen used to scan each program for a value it could not account for and
//! turn refcounting off for the whole program when it found one. Nothing fails
//! that scan anymore, so it is gone and these ops always emit.

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
    /// in-place array-mutation case).
    pub(super) fn rc_replace_slot(&self, i: usize, new: IntValue<'ctx>) {
        let old = self.slot_load(i, "rcold");
        // `jrt_rc_replace` releases `old` and does *not* retain `new`: a store
        // takes ownership. A producer's result therefore needs no retain, and a
        // borrowed read (`Move`, `GetLocal`, `GetField`, …) needs one, which is
        // what `retain` above is for. Skip the call when neither word is heap
        // (the common case for scalar slots) with an inline guard.
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

    /// Release every slot's owned reference — the function's scope-exit cleanup,
    /// emitted immediately before each `return`.
    ///
    /// Parameter slots are included. They used to be skipped, on the reasoning
    /// that the caller still owns the argument, and that held right up until the
    /// callee assigned to one: the overwrite released a reference this frame
    /// never took, so the caller's value was freed under it. The prologue now
    /// retains each parameter (see `lower_body`), and this releases it.
    pub(super) fn emit_scope_exit(&self) {
        for i in 0..self.slots.len() {
            let v = self.load_idx(i);
            self.decref(v);
        }
    }
}
