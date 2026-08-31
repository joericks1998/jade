//! Tagged-value ABI: boxing, unboxing, and register slot access.
//!
//! See this directory's README.

use super::*;

/// Materialize a struct field's declared default as a tagged word in the
/// startup prologue, where there is no `Lowerer` — only a context, module and
/// builder.
///
/// Ints, bools and nil are pure constants. A float or string default has to be
/// heap-allocated, so it is built by a runtime call emitted into the prologue;
/// that runs once before user code, alongside the rest of the registration.
pub(super) fn default_word_const<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    b: &inkwell::builder::Builder<'ctx>,
    v: &VmValue,
) -> Result<IntValue<'ctx>, String> {
    let i64_ty = context.i64_type();
    let ptr_ty = context.ptr_type(AddressSpace::default());
    Ok(match v {
        VmValue::Int(n) => i64_ty.const_int((n.wrapping_shl(1)) as u64, false),
        VmValue::Bool(x) => i64_ty.const_int(if *x { TRUE } else { FALSE }, false),
        VmValue::Nil => i64_ty.const_int(NIL, false),
        VmValue::Float(f) => {
            let bf = module.get_function("jrt_box_float").unwrap_or_else(|| {
                module.add_function(
                    "jrt_box_float",
                    i64_ty.fn_type(&[context.f64_type().into()], false),
                    None,
                )
            });
            b.build_call(bf, &[context.f64_type().const_float(*f).into()], "dfl")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_int_value()
        }
        VmValue::Str(sv) => {
            let dup = module.get_function("jrt_str_dup").unwrap_or_else(|| {
                module.add_function(
                    "jrt_str_dup",
                    ptr_ty.fn_type(&[ptr_ty.into(), context.i8_type().into()], false),
                    None,
                )
            });
            let g = b
                .build_global_string_ptr(sv, "dstr")
                .map_err(|e| e.to_string())?
                .as_pointer_value();
            let raw = b
                .build_call(
                    dup,
                    &[g.into(), context.i8_type().const_int(TRUSTED, false).into()],
                    "dsw",
                )
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_pointer_value();
            let iv = b.build_ptr_to_int(raw, i64_ty, "dsi").map_err(|e| e.to_string())?;
            b.build_or(iv, i64_ty.const_int(TAG_STR, false), "dstag").map_err(|e| e.to_string())?
        }
        // A fresh empty collection, allocated per instance. It cannot be a
        // constant: two structs sharing one array would see each other's
        // writes, and the VM builds a new `ArrayObj` for every literal.
        VmValue::Array(_) => {
            let f = module.get_function("jrt_karr_new").unwrap_or_else(|| {
                module.add_function("jrt_karr_new", ptr_ty.fn_type(&[], false), None)
            });
            let p = b
                .build_call(f, &[], "dfarr")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_pointer_value();
            let iv = b.build_ptr_to_int(p, i64_ty, "dfai").map_err(|e| e.to_string())?;
            b.build_or(iv, i64_ty.const_int(TAG_PTR, false), "dfatag").map_err(|e| e.to_string())?
        }
        VmValue::Dict(_) => {
            let f = module.get_function("jrt_kdict_new").unwrap_or_else(|| {
                module.add_function("jrt_kdict_new", ptr_ty.fn_type(&[], false), None)
            });
            let p = b
                .build_call(f, &[], "dfdict")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_pointer_value();
            let iv = b.build_ptr_to_int(p, i64_ty, "dfdi").map_err(|e| e.to_string())?;
            b.build_or(iv, i64_ty.const_int(TAG_PTR, false), "dfdtag").map_err(|e| e.to_string())?
        }
        other => return Err(format!("codegen: unsupported struct field default {other:?}")),
    })
}

impl<'a, 'ctx> Lowerer<'a, 'ctx> {
    pub(super) fn i64t(&self) -> inkwell::types::IntType<'ctx> {
        self.ctx.i64_type()
    }

    pub(super) fn f64t(&self) -> inkwell::types::FloatType<'ctx> {
        self.ctx.f64_type()
    }

    /// Get an already-declared runtime symbol, or declare it on first use.
    pub(super) fn runtime_fn(
        &self,
        name: &str,
        ty: inkwell::types::FunctionType<'ctx>,
    ) -> FunctionValue<'ctx> {
        self.module.get_function(name).unwrap_or_else(|| self.module.add_function(name, ty, None))
    }

    /// Box a native f64 into a tagged float word via `jrt_box_float` (a heap
    /// malloc + `JRT_TAG_FLOAT`; floats do not fit inline in the tagged ABI).
    pub(super) fn box_float(&self, d: FloatValue<'ctx>) -> IntValue<'ctx> {
        let f = self.runtime_fn("jrt_box_float", self.i64t().fn_type(&[self.f64t().into()], false));
        self.builder
            .build_call(f, &[d.into()], "boxf")
            .unwrap()
            .as_any_value_enum()
            .into_int_value()
    }

    /// Load a boxed float word back to a native f64 via `jrt_unbox_float`.
    ///
    /// Checks the tag first: the word is about to be dereferenced as a pointer
    /// to a double, and the static type that said "float" may have come from a
    /// function whose branches disagreed. See `jrt_require_float_val`.
    pub(super) fn unbox_float(&self, v: IntValue<'ctx>) -> FloatValue<'ctx> {
        let req = self.runtime_fn(
            "jrt_require_float_val",
            self.ctx.void_type().fn_type(&[self.i64t().into()], false),
        );
        self.builder.build_call(req, &[v.into()], "").unwrap();
        let f =
            self.runtime_fn("jrt_unbox_float", self.f64t().fn_type(&[self.i64t().into()], false));
        self.builder
            .build_call(f, &[v.into()], "unboxf")
            .unwrap()
            .as_any_value_enum()
            .into_float_value()
    }

    /// Unbox both operands of a binary float op.
    pub(super) fn float_operands(&self, l: Reg, r: Reg) -> (FloatValue<'ctx>, FloatValue<'ctx>) {
        (self.unbox_float(self.load(l)), self.unbox_float(self.load(r)))
    }

    pub(super) fn ptrt(&self) -> inkwell::types::PointerType<'ctx> {
        self.ctx.ptr_type(AddressSpace::default())
    }

    /// Strip the low-3-bit tag off a heap word and reinterpret as a data pointer.
    pub(super) fn untag_ptr(&self, v: IntValue<'ctx>) -> PointerValue<'ctx> {
        let masked =
            self.builder.build_and(v, self.i64t().const_int(!7u64, false), "pmask").unwrap();
        self.builder.build_int_to_ptr(masked, self.ptrt(), "asptr").unwrap()
    }

    /// Tag an 8-aligned data pointer as a heap string word (`ptr | TAG_STR`).
    pub(super) fn tag_str(&self, p: PointerValue<'ctx>) -> IntValue<'ctx> {
        let asint = self.builder.build_ptr_to_int(p, self.i64t(), "p2i").unwrap();
        self.builder.build_or(asint, self.i64t().const_int(TAG_STR, false), "tagstr").unwrap()
    }

    /// Tag a malloc'd (8-aligned) heap object pointer as a non-string heap word
    /// (`ptr | TAG_PTR`) — used for kind-tagged collections.
    pub(super) fn tag_ptr(&self, p: PointerValue<'ctx>) -> IntValue<'ctx> {
        let asint = self.builder.build_ptr_to_int(p, self.i64t(), "op2i").unwrap();
        self.builder.build_or(asint, self.i64t().const_int(TAG_PTR, false), "tagptr").unwrap()
    }

    /// Read one slot, marking the access volatile in a function that contains a
    /// `try`. Every slot read goes through here — see `Lowerer::volatile_slots`
    /// for why one non-volatile read would be enough to lose the whole thing.
    pub(super) fn slot_load(&self, i: usize, name: &str) -> IntValue<'ctx> {
        let v = self.builder.build_load(self.i64t(), self.slots[i], name).unwrap().into_int_value();
        if self.volatile_slots
            && let Some(inst) = v.as_instruction()
        {
            let _ = inst.set_volatile(true);
        }
        v
    }

    /// Write one slot, volatile on the same terms as [`Self::slot_load`]. Does
    /// not touch the refcount — callers pair it with `rc_replace_slot`.
    pub(super) fn slot_store(&self, i: usize, v: IntValue<'ctx>) {
        let st = self.builder.build_store(self.slots[i], v).unwrap();
        if self.volatile_slots {
            let _ = st.set_volatile(true);
        }
    }

    /// Load a register's tagged word.
    pub(super) fn load(&self, r: Reg) -> IntValue<'ctx> {
        self.slot_load(r as usize, "ld")
    }

    /// Store a tagged word into a register, releasing the reference the slot
    /// previously held (in refcount mode). Slots are nil-initialized in the entry
    /// block, so the first store to any slot releases nil (a no-op).
    pub(super) fn store(&self, r: Reg, v: IntValue<'ctx>) {
        self.rc_replace_slot(r as usize, v);
        self.slot_store(r as usize, v);
    }

    /// Load a slot by raw index (locals share the register array in the VM, so
    /// `GetLocal`/`SetLocal` index the same `slots`).
    pub(super) fn load_idx(&self, i: usize) -> IntValue<'ctx> {
        self.slot_load(i, "ldl")
    }

    pub(super) fn store_idx(&self, i: usize, v: IntValue<'ctx>) {
        self.rc_replace_slot(i, v);
        self.slot_store(i, v);
    }

    /// Store a value the destination is *borrowing* — a `Move`, a local read, an
    /// element read. The source still owns its reference, so the destination
    /// takes one of its own.
    ///
    /// The retain and the release of the slot's old value belong in one call,
    /// not two beside each other. `jrt_rc_replace` skips the release when the
    /// slot already holds the same object, which is right for a write-back that
    /// mutated in place and took no reference, and wrong here — the retain is
    /// then unbalanced. Reading the same local on every pass of a loop gained a
    /// reference per pass:
    ///
    /// ```jade
    /// while seen < 5 { let x = a[0]  seen = seen + 1 }
    /// ```
    ///
    /// leaked the array once per pass of the loop around that one. A single
    /// pass balanced by luck, because the *next* store to the slot released it,
    /// which is why it took a nested loop to show at all.
    pub(super) fn store_borrowed_idx(&self, i: usize, v: IntValue<'ctx>) {
        // The retain first, so releasing the old value cannot free the new one
        // when they are the same object.
        self.retain(v);
        self.rc_replace_slot_retained(i, v);
        self.slot_store(i, v);
    }

    /// [`Self::store_borrowed_idx`] addressed by register.
    pub(super) fn store_borrowed(&self, r: Reg, v: IntValue<'ctx>) {
        self.store_borrowed_idx(r as usize, v);
    }

    /// The module-level global cell for `name`, created (initialized to nil) on
    /// first reference. Globals are module-scoped — shared across the top-level
    /// chunk and every lowered `fn_def` — so they must be LLVM globals keyed by
    /// name, not function allocas.
    pub(super) fn global_slot(&self, name: &str) -> PointerValue<'ctx> {
        let gname = format!("jgl_{name}");
        if let Some(g) = self.module.get_global(&gname) {
            return g.as_pointer_value();
        }
        let g = self.module.add_global(self.i64t(), None, &gname);
        g.set_initializer(&self.i64t().const_int(NIL, false));
        g.set_linkage(inkwell::module::Linkage::Internal);
        g.as_pointer_value()
    }

    /// Untag an int word to its native i64 (arithmetic shift right by 1).
    pub(super) fn untag_int(&self, v: IntValue<'ctx>) -> IntValue<'ctx> {
        self.builder.build_right_shift(v, self.i64t().const_int(1, false), true, "utag").unwrap()
    }

    /// Tag a native i64 as an int word (shift left by 1; low bit 0).
    pub(super) fn tag_int(&self, v: IntValue<'ctx>) -> IntValue<'ctx> {
        self.builder.build_left_shift(v, self.i64t().const_int(1, false), "tag").unwrap()
    }

    /// Untag a bool word to an `i1` (bit 4 holds the value).
    pub(super) fn untag_bool(&self, v: IntValue<'ctx>) -> IntValue<'ctx> {
        let shifted = self
            .builder
            .build_right_shift(v, self.i64t().const_int(4, false), false, "bsh")
            .unwrap();
        let bit = self.builder.build_and(shifted, self.i64t().const_int(1, false), "band").unwrap();
        self.builder
            .build_int_compare(IntPredicate::NE, bit, self.i64t().const_zero(), "btrue")
            .unwrap()
    }

    /// The truth value of a word that is not necessarily a bool.
    ///
    /// `untag_bool` reads bit 4, which is the answer only when the word really
    /// is a bool. Anything else — a heap pointer, an int, nil — has whatever
    /// happens to sit in that bit, so `if bound_method { … }` took the false
    /// branch on a value `bool(...)` reported as true. Bools stay on the inline
    /// path, since the checker turns most conditions into one; everything else
    /// asks the runtime, which is the same function `bool(x)` calls.
    pub(super) fn truthy(&self, v: IntValue<'ctx>) -> IntValue<'ctx> {
        let b = self.builder;
        let i64_ty = self.i64t();
        // low4 == 0b1111 marks nil/bool immediates; bool is the 0xf case.
        let low4 = b.build_and(v, i64_ty.const_int(0xf, false), "tb_low4").unwrap();
        let is_bool = b
            .build_int_compare(IntPredicate::EQ, low4, i64_ty.const_int(0xf, false), "tb_isbool")
            .unwrap();
        let func = b.get_insert_block().unwrap().get_parent().unwrap();
        let fast_bb = self.ctx.append_basic_block(func, "tb_bool");
        let slow_bb = self.ctx.append_basic_block(func, "tb_any");
        let join_bb = self.ctx.append_basic_block(func, "tb_join");
        b.build_conditional_branch(is_bool, fast_bb, slow_bb).unwrap();

        b.position_at_end(fast_bb);
        let fast = self.untag_bool(v);
        b.build_unconditional_branch(join_bb).unwrap();
        let fast_end = b.get_insert_block().unwrap();

        b.position_at_end(slow_bb);
        let f = self.runtime_fn("jrt_bool_any", i64_ty.fn_type(&[i64_ty.into()], false));
        let w =
            b.build_call(f, &[v.into()], "tb_any").unwrap().as_any_value_enum().into_int_value();
        let slow = self.untag_bool(w);
        b.build_unconditional_branch(join_bb).unwrap();
        let slow_end = b.get_insert_block().unwrap();

        b.position_at_end(join_bb);
        let phi = b.build_phi(self.ctx.bool_type(), "tb").unwrap();
        phi.add_incoming(&[(&fast, fast_end), (&slow, slow_end)]);
        phi.as_basic_value().into_int_value()
    }

    /// Untag both operands of a binary int op.
    pub(super) fn int_operands(&self, l: Reg, r: Reg) -> (IntValue<'ctx>, IntValue<'ctx>) {
        (self.untag_int(self.load(l)), self.untag_int(self.load(r)))
    }

    /// Wrap an `i1` as a tagged bool word (`true`→0x1f, `false`→0x0f).
    pub(super) fn bool_word(&self, b: IntValue<'ctx>) -> IntValue<'ctx> {
        self.builder
            .build_select(
                b,
                self.i64t().const_int(TRUE, false),
                self.i64t().const_int(FALSE, false),
                "boolw",
            )
            .unwrap()
            .into_int_value()
    }

    /// Untag a bool word and zero-extend to i64 (for bool ordering: false < true).
    pub(super) fn zext_bool(&self, r: Reg) -> IntValue<'ctx> {
        let b = self.untag_bool(self.load(r));
        self.builder.build_int_z_extend(b, self.i64t(), "zb").unwrap()
    }

    /// Read register `r` as an f64, widening from int if `is_int` (mixed
    /// int/float comparisons).
    pub(super) fn as_float(&self, r: Reg, is_int: bool) -> FloatValue<'ctx> {
        if is_int {
            let i = self.untag_int(self.load(r));
            self.builder.build_signed_int_to_float(i, self.f64t(), "i2fc").unwrap()
        } else {
            self.unbox_float(self.load(r))
        }
    }

    /// Materialize a compiled default-parameter value (always a literal, per
    /// `emit_fn`) as a tagged word for a call-site argument fill. Mirrors the
    /// tagged ABI used by the constant-load opcodes.
    pub(super) fn default_word(&self, v: &VmValue) -> Result<IntValue<'ctx>, String> {
        Ok(match v {
            VmValue::Int(n) => self.i64t().const_int((n.wrapping_shl(1)) as u64, false),
            VmValue::Bool(b) => self.i64t().const_int(if *b { TRUE } else { FALSE }, false),
            VmValue::Nil => self.i64t().const_int(NIL, false),
            VmValue::Float(f) => self.box_float(self.f64t().const_float(*f)),
            VmValue::Str(s) => self.str_literal_word(s)?,
            // A fresh empty collection per instance, not a shared constant: two
            // structs holding one array would see each other's writes. Reached
            // by a struct field default under a `...base`, where the default is
            // no longer folded into the literal while it compiles.
            VmValue::Array(_) => {
                let f = self.runtime_fn("jrt_karr_new", self.ptrt().fn_type(&[], false));
                let p = self
                    .builder
                    .build_call(f, &[], "dfarr")
                    .map_err(|e| e.to_string())?
                    .as_any_value_enum()
                    .into_pointer_value();
                self.tag_ptr(p)
            }
            VmValue::Dict(_) => {
                let f = self.runtime_fn("jrt_kdict_new", self.ptrt().fn_type(&[], false));
                let p = self
                    .builder
                    .build_call(f, &[], "dfdict")
                    .map_err(|e| e.to_string())?
                    .as_any_value_enum()
                    .into_pointer_value();
                self.tag_ptr(p)
            }
            other => return Err(format!("codegen: unsupported default value {other:?}")),
        })
    }
}
