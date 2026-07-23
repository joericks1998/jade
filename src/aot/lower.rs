//! Bytecode `Instr` → LLVM IR lowering (brick 2 of the Chunk→LLVM backend).
//!
//! ## Model: tagged register slots
//!
//! `emit.rs` produces a register machine. We give every register an `alloca i64`
//! holding a **tagged value word** (the same tagged ABI the runtime uses, see
//! `jade-runtime`/`runtime.h`). Instructions load their operand slots, untag to a
//! native value, compute, re-tag, and store the result slot. This is simpler
//! than tracking a static type per register (the emitter reuses register slots
//! across types), and LLVM's `mem2reg` + `instcombine` promote the allocas to
//! SSA and fold the `untag(tag(x))` round-trips away, so the tag arithmetic is
//! mostly free after optimization. Boxed floats and calls still hit the runtime
//! (added in later bricks).
//!
//! This brick handles the no-exception, no-call, no-heap subset: constant loads,
//! `Move`, integer arithmetic, control flow, and `Return`. Unsupported opcodes
//! return an `Err` so the daemon can fall back to the legacy `expr.rs` path
//! while this backend grows (the migration never runs an incomplete lowering).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use inkwell::attributes::AttributeLoc;
use inkwell::basic_block::BasicBlock as LlvmBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::{AnyValue, BasicMetadataValueEnum, FloatValue, FunctionValue, IntValue, PointerValue};
use inkwell::{AddressSpace, FloatPredicate, IntPredicate};

use crate::bytecode::{Chunk, CompiledFn, FStrPart, Instr, Reg};
use crate::vm::VmValue;
use crate::frontend::ast::{BinOpKind, Expr, StructFieldDef, UnaryOpKind};

use super::cfg;

// Tagged-immediate bit patterns (mirror runtime.h / jade-runtime value.rs).
const NIL: u64 = 0x07;
const TRUE: u64 = 0x1f;
const FALSE: u64 = 0x0f;
// Low-3-bit heap tags (mirror runtime.h JRT_TAG_*).
const TAG_PTR: u64 = 1; // 0b001 non-string heap object (incl. a boxed fn value)
const TAG_STR: u64 = 5; // 0b101 heap string pointer
// Trust byte for literal (compile-time-known) strings.
const TRUSTED: u64 = 0;
// ObjKind::Fn (mirror jade-runtime heap.rs). A function-value box carries this in
// the ObjKind byte (header offset 8) so the refcount ops recognise it as a
// non-collection and no-op on it, while indirect_call still reads fn_ptr at
// offset 0. See fn_box_word.
const OBJKIND_FN: u64 = 5;
// ObjKind::BoundMethod (mirror jade-runtime heap.rs). Same shape as a function
// box plus the receiver at offset 16; `indirect_call` reads the kind byte to
// decide whether to prepend it as `self`. Built by jrt_bind_method_new, never
// by codegen directly.
const OBJKIND_BOUND_METHOD: u64 = 9;

/// Reserved global names that resolve to a runtime builtin (not a user value),
/// mirroring `crate::builtins::seed_globals`. A `Call` whose callee is
/// `GetGlobal(<one of these>)` and which this backend does not itself lower is a
/// builtin call we can't emit — the whole program falls back to `expr.rs`. (A
/// user function that shadows one of these names is bound via `SetGlobal`, so it
/// is tracked as a known function and takes precedence over this check.)
const RESERVED_BUILTINS: &[&str] = &[
    // core + native + type-constructor globals
    "write", "len", "input", "print", "stream", "route", "int", "float", "bool", "str", "func",
    "Grammar",
    // stdlib package globals (accessed via `use`; a bare call is invalid, but
    // reserving them keeps a stray Call from mis-lowering to an indirect call)
    "llm", "string", "math", "array", "dict", "fs", "time", "http", "uhttp", "sh", "json", "env",
    "path", "random",
];

/// Parse a native package reference `__native$<pkgid>$<fn>` (produced by import
/// namespacing) into `(pkgid, fn_name)`. `None` for ordinary names.
fn parse_native_ref(name: &str) -> Option<(u32, &str)> {
    let rest = name.strip_prefix("__native$")?;
    let (id, fname) = rest.split_once('$')?;
    Some((id.parse().ok()?, fname))
}

/// Stdlib module namespaces whose `module.method(...)` calls the Chunk backend can
/// recognize (a `GetField` whose base was `GetGlobal`'d from one of these names is
/// a module call, not a struct/primitive method).
fn is_stdlib_module(name: &str) -> bool {
    matches!(
        name,
        "math" | "json" | "llm" | "path" | "time" | "env" | "fs" | "random" | "http" | "sh"
            | "dict" | "array" | "string" | "Grammar"
    )
}

/// Whether the Chunk backend can lower `module.method` with `argc` explicit args.
/// Restricted to **layout-safe** methods — string/scalar I/O whose runtime symbols
/// don't produce or consume the legacy `JrtArrayHdr`/`JadeDict` collection layouts
/// (those need ObjHeader-aware runtime helpers, a later brick). Everything else
/// declines to the legacy path. Keep in lockstep with `emit_module_call`.
fn chunk_module_supported(module: &str, method: &str, argc: usize) -> bool {
    match (module, method) {
        ("math", "floor" | "ceil" | "abs" | "sqrt") => argc == 1,
        ("math", "min" | "max" | "pow") => argc == 2,
        ("path", "basename" | "ext" | "dirname" | "stem" | "abs") => argc == 1,
        ("path", "is_abs") => argc == 1,
        ("path", "join") => argc >= 1,
        ("fs", "read") => argc == 1 || argc == 2, // read(path) or read(path, trust)
        ("fs", "exists" | "delete" | "mkdir") => argc == 1,
        ("fs", "write" | "append") => argc == 2,
        ("fs", "list_dir") => argc == 1,
        ("sh", "exec" | "run" | "output") => argc == 1,
        ("random", "choice" | "shuffle") => argc == 1,
        ("env", "cwd") => argc == 0,
        ("env", "get") => argc == 1,
        ("env", "set") => argc == 2,
        ("env", "args") => argc == 0,
        ("time", "now" | "now_ms") => argc == 0,
        ("time", "sleep") => argc == 1,
        ("time", "local") => argc == 1,
        ("http", "get" | "delete" | "head") => argc == 1 || argc == 2,
        ("http", "post" | "put") => argc == 2 || argc == 3,
        ("array", "map" | "filter") => argc == 2,
        ("random", "int") => argc == 2,
        ("random", "seed") => argc == 1,
        ("random", "float") => argc == 0,
        ("llm", "count_tokens") => argc == 1,
        ("llm", "total_tokens") => argc == 0,
        ("llm", "model") => argc == 0,
        ("llm", "set_max_tokens" | "keep_anchors") => argc == 1,
        ("llm", "health") => argc == 0,
        ("dict", "merge") => argc == 2,
        ("json", "parse" | "stringify" | "stringify_pretty") => argc == 1,
        ("Grammar", "new") => (1..=3).contains(&argc), // pattern[, anchor[, stop]]
        _ => false,
    }
}

/// Whether `method` is a string-only primitive method the Chunk path lowers via
/// the shared `jrt_str_*` symbol (the receiver is unambiguously a string). These
/// names don't belong to dict/array, so no runtime kind dispatch is needed.
/// `contains` (also a dict method) and `split` (returns a collection) are excluded.
fn chunk_str_method_supported(method: &str, argc: usize) -> bool {
    match method {
        "trim" | "upper" | "lower" => argc == 0,
        "starts_with" | "ends_with" => argc == 1,
        "replace" => argc == 2,
        "split" => argc == 1,
        _ => false,
    }
}

/// Whether `method` is an array/dict primitive method unique to one collection
/// kind (so the receiver's kind is known by the method name — the frontend has
/// type-checked it). `contains` (str/array/dict) and `len` (all) are excluded.
fn chunk_val_method_supported(method: &str, argc: usize) -> bool {
    match method {
        "push" => argc == 1,                       // array
        "pop" | "sort" | "reverse" => argc == 0,   // array
        "keys" | "values" => argc == 0,            // dict
        "has" | "get" => argc == 1,                // dict
        "contains" => argc == 1,                   // str / array (runtime-dispatched)
        "len" => argc == 0,                        // str / array / dict (runtime-dispatched)
        _ => false,
    }
}

/// Per-function lowering helper: bundles the builder, the i64 slot type, and the
/// register `alloca`s so the instruction handlers stay terse.
struct Lowerer<'a, 'ctx> {
    ctx: &'ctx Context,
    module: &'a Module<'ctx>,
    builder: &'a Builder<'ctx>,
    slots: &'a [PointerValue<'ctx>],
    /// Reference-counting enabled for this program (collections-only; see
    /// `FnCtx::refcount`). When false every rc method below is a no-op, so the
    /// emitted IR is byte-identical to the pre-B4.2 backend.
    refcount: bool,
    /// Parameter count of the function being lowered. Parameter slots (`0..n_params`)
    /// hold references the *caller* owns (borrowed), so scope-exit release covers
    /// only the locals (`n_params..`).
    n_params: usize,
}

impl<'a, 'ctx> Lowerer<'a, 'ctx> {
    fn i64t(&self) -> inkwell::types::IntType<'ctx> {
        self.ctx.i64_type()
    }

    fn f64t(&self) -> inkwell::types::FloatType<'ctx> {
        self.ctx.f64_type()
    }

    /// Get an already-declared runtime symbol, or declare it on first use.
    fn runtime_fn(
        &self,
        name: &str,
        ty: inkwell::types::FunctionType<'ctx>,
    ) -> FunctionValue<'ctx> {
        self.module
            .get_function(name)
            .unwrap_or_else(|| self.module.add_function(name, ty, None))
    }

    /// Box a native f64 into a tagged float word via `jrt_box_float` (a heap
    /// malloc + `JRT_TAG_FLOAT`; floats do not fit inline in the tagged ABI).
    fn box_float(&self, d: FloatValue<'ctx>) -> IntValue<'ctx> {
        let f = self.runtime_fn("jrt_box_float", self.i64t().fn_type(&[self.f64t().into()], false));
        self.builder
            .build_call(f, &[d.into()], "boxf")
            .unwrap()
            .as_any_value_enum()
            .into_int_value()
    }

    /// Load a boxed float word back to a native f64 via `jrt_unbox_float`.
    fn unbox_float(&self, v: IntValue<'ctx>) -> FloatValue<'ctx> {
        let f = self.runtime_fn("jrt_unbox_float", self.f64t().fn_type(&[self.i64t().into()], false));
        self.builder
            .build_call(f, &[v.into()], "unboxf")
            .unwrap()
            .as_any_value_enum()
            .into_float_value()
    }

    /// Unbox both operands of a binary float op.
    fn float_operands(&self, l: Reg, r: Reg) -> (FloatValue<'ctx>, FloatValue<'ctx>) {
        (self.unbox_float(self.load(l)), self.unbox_float(self.load(r)))
    }

    fn ptrt(&self) -> inkwell::types::PointerType<'ctx> {
        self.ctx.ptr_type(AddressSpace::default())
    }

    /// Strip the low-3-bit tag off a heap word and reinterpret as a data pointer.
    fn untag_ptr(&self, v: IntValue<'ctx>) -> PointerValue<'ctx> {
        let masked = self
            .builder
            .build_and(v, self.i64t().const_int(!7u64, false), "pmask")
            .unwrap();
        self.builder.build_int_to_ptr(masked, self.ptrt(), "asptr").unwrap()
    }

    /// Tag an 8-aligned data pointer as a heap string word (`ptr | TAG_STR`).
    fn tag_str(&self, p: PointerValue<'ctx>) -> IntValue<'ctx> {
        let asint = self.builder.build_ptr_to_int(p, self.i64t(), "p2i").unwrap();
        self.builder
            .build_or(asint, self.i64t().const_int(TAG_STR, false), "tagstr")
            .unwrap()
    }

    /// Tag a malloc'd (8-aligned) heap object pointer as a non-string heap word
    /// (`ptr | TAG_PTR`) — used for kind-tagged collections.
    fn tag_ptr(&self, p: PointerValue<'ctx>) -> IntValue<'ctx> {
        let asint = self.builder.build_ptr_to_int(p, self.i64t(), "op2i").unwrap();
        self.builder
            .build_or(asint, self.i64t().const_int(TAG_PTR, false), "tagptr")
            .unwrap()
    }

    /// A plain NUL-terminated C string global (for compile-time struct type/field
    /// names passed to the runtime — not a tagged Jade string).
    fn cstr(&self, s: &str) -> PointerValue<'ctx> {
        self.builder.build_global_string_ptr(s, "cstr").unwrap().as_pointer_value()
    }

    /// Materialize a compile-time string literal as a TRUSTED tagged-string
    /// global and return its **data pointer** (8-aligned). Layout mirrors
    /// `expr.rs::emit_tagged_literal`: `[7 pad][trust][bytes…][nul]`, so the
    /// data pointer is `global+8` and the trust byte lives at `data[-1]`.
    fn str_literal_ptr(&self, s: &str) -> Result<PointerValue<'ctx>, String> {
        let i8_ty = self.ctx.i8_type();
        let i32_ty = self.ctx.i32_type();
        let bytes = s.as_bytes();
        let mut data: Vec<u8> = Vec::with_capacity(bytes.len() + 9);
        data.extend_from_slice(&[0u8; 7]);
        data.push(TRUSTED as u8);
        data.extend_from_slice(bytes);
        data.push(0);

        let arr_ty = i8_ty.array_type(data.len() as u32);
        let const_arr = self.ctx.const_string(&data, false);
        let global = self.module.add_global(arr_ty, None, "str_lit_t");
        global.set_initializer(&const_arr);
        global.set_linkage(inkwell::module::Linkage::Internal);
        global.set_constant(true);
        global.set_alignment(8);

        let zero = i32_ty.const_zero();
        let eight = i32_ty.const_int(8, false);
        unsafe {
            self.builder
                .build_in_bounds_gep(arr_ty, global.as_pointer_value(), &[zero, eight], "lit_data")
                .map_err(|e| e.to_string())
        }
    }

    /// Materialize a string literal as a tagged STRING **word** (ready to store
    /// in a slot), not just its data pointer.
    fn str_literal_word(&self, s: &str) -> Result<IntValue<'ctx>, String> {
        let ptr = self.str_literal_ptr(s)?;
        Ok(self.tag_str(ptr))
    }

    /// Raise `value` (a tagged word) as an untyped exception via the runtime's
    /// `jade_exc_throw_typed(value, NULL)`, then close the block with
    /// `unreachable` (throw is noreturn — it longjmps to the active handler or
    /// aborts). Callers must not emit anything after this in the same block.
    fn throw(&self, value: IntValue<'ctx>) -> Result<(), String> {
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

    /// Declare (once) `setjmp(ptr) -> i32` with the `returns_twice` attribute —
    /// without it LLVM may hoist code across the call and miscompile the second
    /// (longjmp) return.
    fn setjmp_fn(&self) -> FunctionValue<'ctx> {
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

    fn push_frame(&self, buf: PointerValue<'ctx>) {
        let f = self
            .runtime_fn("jade_exc_push_frame", self.ctx.void_type().fn_type(&[self.ptrt().into()], false));
        self.builder.build_call(f, &[buf.into()], "").unwrap();
    }

    fn pop_frame(&self) {
        let f = self.runtime_fn("jade_exc_pop", self.ctx.void_type().fn_type(&[], false));
        self.builder.build_call(f, &[], "").unwrap();
    }

    /// The currently-thrown value word (`jade_exc_value()`), read inside a
    /// handler's landing block.
    fn exc_value(&self) -> IntValue<'ctx> {
        let f = self.runtime_fn("jade_exc_value", self.i64t().fn_type(&[], false));
        self.builder
            .build_call(f, &[], "excv")
            .unwrap()
            .as_any_value_enum()
            .into_int_value()
    }

    /// Lower `print(val)`: the runtime's tag-dispatching `jrt_print_any(val,
    /// "\n")` (VM-faithful `value_to_display` + trailing newline). `print`
    /// evaluates to nil.
    fn print_value(&self, val: IntValue<'ctx>, dest: Reg) -> Result<(), String> {
        let f = self.runtime_fn(
            "jrt_print_any",
            self.ctx.void_type().fn_type(&[self.i64t().into(), self.ptrt().into()], false),
        );
        let nl = self
            .builder
            .build_global_string_ptr("\n", "jprint_nl")
            .map_err(|e| e.to_string())?
            .as_pointer_value();
        self.builder
            .build_call(f, &[val.into(), nl.into()], "")
            .map_err(|e| e.to_string())?;
        self.store(dest, self.i64t().const_int(NIL, false));
        Ok(())
    }

    /// Integer divide (`is_mod == false`) or modulo, with the VM's catchable
    /// zero-divisor behavior: LLVM `sdiv`/`srem` by 0 is UB (traps on arm64),
    /// so we branch on a zero divisor and `raise` a "division/modulo by zero"
    /// string instead. Leaves the builder on the non-zero ("ok") block and
    /// returns the tagged result word.
    fn int_div_mod(&self, l: Reg, r: Reg, is_mod: bool) -> Result<IntValue<'ctx>, String> {
        let (a, c) = self.int_operands(l, r);
        let func = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or("int_div_mod outside a function")?;
        let is_zero = self
            .builder
            .build_int_compare(IntPredicate::EQ, c, self.i64t().const_zero(), "divz")
            .map_err(|e| e.to_string())?;
        let throw_bb = self.ctx.append_basic_block(func, "divzero_throw");
        let ok_bb = self.ctx.append_basic_block(func, "divzero_ok");
        self.builder
            .build_conditional_branch(is_zero, throw_bb, ok_bb)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(throw_bb);
        let msg = self.str_literal_word(if is_mod { "modulo by zero" } else { "division by zero" })?;
        self.throw(msg)?;

        self.builder.position_at_end(ok_bb);
        let res = if is_mod {
            self.builder.build_int_signed_rem(a, c, "iremi").map_err(|e| e.to_string())?
        } else {
            self.builder.build_int_signed_div(a, c, "idivi").map_err(|e| e.to_string())?
        };
        Ok(self.tag_int(res))
    }

    /// Compute a checked integer result: do the arithmetic in i128, verify it
    /// fits the 63-bit tagged range, and raise "integer overflow" if not.
    ///
    /// `AddInt`/`SubInt`/`MulInt`/`NegInt` used to emit a bare LLVM add/sub/mul
    /// and tag whatever came out, so compiled integer arithmetic silently
    /// produced a wrong number on overflow while the VM — which uses
    /// `checked_add`/`checked_mul` — raised. Two distinct gaps: the result could
    /// exceed i64, and even an in-i64 result may not fit a tagged word, because
    /// one bit goes to the tag.
    ///
    /// i128 closes both at once: two 63-bit operands cannot overflow it under
    /// any of these operations, so one range check against the tagged bounds is
    /// exact. The widening costs a couple of instructions and buys agreement
    /// with the VM on every integer operation.
    fn checked_int_result(&self, wide: IntValue<'ctx>, what: &str) -> Result<IntValue<'ctx>, String> {
        let i128t = self.ctx.i128_type();
        let func = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or("checked_int_result outside a function")?;

        // The inclusive bounds of a tagged integer: -(1<<62) ..= (1<<62)-1.
        let max = i128t.const_int((1u64 << 62) - 1, false);
        let min = self
            .builder
            .build_int_neg(i128t.const_int(1u64 << 62, false), "imin")
            .map_err(|e| e.to_string())?;

        let too_big = self
            .builder
            .build_int_compare(IntPredicate::SGT, wide, max, "ovf_hi")
            .map_err(|e| e.to_string())?;
        let too_small = self
            .builder
            .build_int_compare(IntPredicate::SLT, wide, min, "ovf_lo")
            .map_err(|e| e.to_string())?;
        let overflowed =
            self.builder.build_or(too_big, too_small, "ovf").map_err(|e| e.to_string())?;

        let throw_bb = self.ctx.append_basic_block(func, "intovf_throw");
        let ok_bb = self.ctx.append_basic_block(func, "intovf_ok");
        self.builder
            .build_conditional_branch(overflowed, throw_bb, ok_bb)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(throw_bb);
        let msg = self.str_literal_word("integer overflow")?;
        self.throw(msg)?;

        self.builder.position_at_end(ok_bb);
        let narrowed = self
            .builder
            .build_int_truncate(wide, self.i64t(), what)
            .map_err(|e| e.to_string())?;
        Ok(self.tag_int(narrowed))
    }

    /// Sign-extend an untagged i64 operand to i128 for checked arithmetic.
    fn widen(&self, v: IntValue<'ctx>) -> Result<IntValue<'ctx>, String> {
        self.builder
            .build_int_s_extend(v, self.ctx.i128_type(), "w")
            .map_err(|e| e.to_string())
    }

    /// Concatenate two tagged-string **data pointers** via the shared runtime
    /// `jrt_str_concat` (trust = max of inputs); returns a new data pointer.
    fn concat_ptrs(&self, a: PointerValue<'ctx>, b: PointerValue<'ctx>) -> PointerValue<'ctx> {
        let f = self.runtime_fn(
            "jrt_str_concat",
            self.ptrt().fn_type(&[self.ptrt().into(), self.ptrt().into()], false),
        );
        self.builder
            .build_call(f, &[a.into(), b.into()], "concat")
            .unwrap()
            .as_any_value_enum()
            .into_pointer_value()
    }

    /// Concatenate two tagged-string words; returns a new tagged word.
    fn str_concat(&self, l: Reg, r: Reg) -> IntValue<'ctx> {
        let lp = self.untag_ptr(self.load(l));
        let rp = self.untag_ptr(self.load(r));
        self.tag_str(self.concat_ptrs(lp, rp))
    }

    /// Render a value word to a tagged-string **data pointer** via the runtime's
    /// `jrt_str_of_any` (VM-faithful for scalars/strings; preserves trust). Used
    /// to interpolate an f-string part.
    fn str_of_any(&self, r: Reg) -> PointerValue<'ctx> {
        let f = self.runtime_fn("jrt_str_of_any", self.ptrt().fn_type(&[self.i64t().into()], false));
        self.builder
            .build_call(f, &[self.load(r).into()], "strofany")
            .unwrap()
            .as_any_value_enum()
            .into_pointer_value()
    }

    /// Load a register's tagged word.
    fn load(&self, r: Reg) -> IntValue<'ctx> {
        self.builder
            .build_load(self.i64t(), self.slots[r as usize], "ld")
            .unwrap()
            .into_int_value()
    }

    /// Store a tagged word into a register, releasing the reference the slot
    /// previously held (in refcount mode). Slots are nil-initialized in the entry
    /// block, so the first store to any slot releases nil (a no-op).
    fn store(&self, r: Reg, v: IntValue<'ctx>) {
        self.rc_replace_slot(r as usize, v);
        self.builder.build_store(self.slots[r as usize], v).unwrap();
    }

    /// Load a slot by raw index (locals share the register array in the VM, so
    /// `GetLocal`/`SetLocal` index the same `slots`).
    fn load_idx(&self, i: usize) -> IntValue<'ctx> {
        self.builder
            .build_load(self.i64t(), self.slots[i], "ldl")
            .unwrap()
            .into_int_value()
    }

    fn store_idx(&self, i: usize, v: IntValue<'ctx>) {
        self.rc_replace_slot(i, v);
        self.builder.build_store(self.slots[i], v).unwrap();
    }

    // ── Reference counting (B4.2; all no-ops unless `self.refcount`) ──────────

    /// Emit `jrt_incref(w)` — retain a reference. No-op on non-collection words.
    fn incref(&self, w: IntValue<'ctx>) {
        if !self.refcount {
            return;
        }
        let f = self.runtime_fn("jrt_incref", self.ctx.void_type().fn_type(&[self.i64t().into()], false));
        self.builder.build_call(f, &[w.into()], "").unwrap();
    }

    /// Emit `jrt_decref(w)` — release a reference (frees at zero, cascading).
    fn decref(&self, w: IntValue<'ctx>) {
        if !self.refcount {
            return;
        }
        let f = self.runtime_fn("jrt_decref", self.ctx.void_type().fn_type(&[self.i64t().into()], false));
        self.builder.build_call(f, &[w.into()], "").unwrap();
    }

    /// Retain a value that is a *borrowed* read of an existing reference (a
    /// `Move`/`GetLocal`/`GetGlobal`/`GetIndex`/`GetField` result): the destination
    /// slot becomes a new owner, so the count must rise. Producer/call results are
    /// already owned and must NOT be routed through here.
    fn retain(&self, w: IntValue<'ctx>) {
        self.incref(w);
    }

    /// Before slot `i` is overwritten with `new`, release whatever reference it
    /// held (via `jrt_rc_replace`, which skips the release when `old == new` — the
    /// in-place array-mutation case). No-op unless refcounting is on.
    fn rc_replace_slot(&self, i: usize, new: IntValue<'ctx>) {
        if !self.refcount {
            return;
        }
        let old = self
            .builder
            .build_load(self.i64t(), self.slots[i], "rcold")
            .unwrap()
            .into_int_value();
        let f = self.runtime_fn(
            "jrt_rc_replace",
            self.ctx.void_type().fn_type(&[self.i64t().into(), self.i64t().into()], false),
        );
        self.builder.build_call(f, &[old.into(), new.into()], "").unwrap();
    }

    /// Release every local slot's owned reference — the function's scope-exit
    /// cleanup, emitted immediately before each `return`. Parameter slots
    /// (`0..n_params`) are borrowed from the caller and left untouched.
    fn emit_scope_exit(&self) {
        if !self.refcount {
            return;
        }
        for i in self.n_params..self.slots.len() {
            let v = self.load_idx(i);
            self.decref(v);
        }
    }

    /// A first-class function value for `jf_<uid>`: a `TAG_PTR`-tagged pointer to
    /// an 8-aligned internal global `{ ptr fn_ptr@0, i64 kind@8 }`. All fn values
    /// for a uid share one box global (allocation-free). `indirect_call` reads
    /// `fn_ptr` at offset 0 and calls through it; the `kind` word at offset 8 holds
    /// `ObjKind::Fn`, aligned with `ObjHeader.kind`, so the refcount ops
    /// (`jrt_incref`/`jrt_decref`) recognise the box as a non-collection and no-op
    /// on it — which is what lets a program that merely *defines* functions still
    /// be treated as collections-only for refcounting.
    fn fn_box_word(&self, uid: usize, f: FunctionValue<'ctx>) -> IntValue<'ctx> {
        let gname = format!("jf_box_{uid}");
        let g = self.module.get_global(&gname).unwrap_or_else(|| {
            let box_ty = self.ctx.struct_type(&[self.ptrt().into(), self.i64t().into()], false);
            let g = self.module.add_global(box_ty, None, &gname);
            let init = self.ctx.const_struct(
                &[
                    f.as_global_value().as_pointer_value().into(),
                    self.i64t().const_int(OBJKIND_FN, false).into(),
                ],
                false,
            );
            g.set_initializer(&init);
            g.set_constant(true);
            g.set_linkage(inkwell::module::Linkage::Internal);
            g.set_alignment(8);
            g
        });
        let asint = self
            .builder
            .build_ptr_to_int(g.as_pointer_value(), self.i64t(), "boxp2i")
            .unwrap();
        self.builder
            .build_or(asint, self.i64t().const_int(TAG_PTR, false), "boxtag")
            .unwrap()
    }

    /// Indirect call through a first-class function value: untag the callee box and
    /// load its `fn_ptr` (field 0). If `fn_ptr` is the `jrt_native_call` sentinel,
    /// the box is a native function value `{ sentinel, kind, env={handle,name} }` —
    /// dispatch through `jrt_native_call`. Otherwise it is an ordinary `jf_<uid>`
    /// box — call it directly with `args` (all tagged i64 words). The callee's arity
    /// equals `args.len()` (the frontend guarantees it).
    fn indirect_call(&self, callee: Reg, args: &[Reg]) -> Result<IntValue<'ctx>, String> {
        let e = |x: inkwell::builder::BuilderError| x.to_string();
        let b = self.builder;
        let i64_ty = self.i64t();
        let ptrt = self.ptrt();

        let box_ptr = self.untag_ptr(self.load(callee));
        let fn_ptr = b.build_load(ptrt, box_ptr, "fnld").map_err(e)?.into_pointer_value();

        // A bound method (`let f = obj.greet`) is a function value carrying the
        // receiver it will pass as `self`: {fn_ptr@0, kind@8, self@16}. It is
        // told apart by the ObjKind byte at offset 8 rather than by a sentinel
        // address at offset 0 (the older native-fn trick) — every TAG_PTR value
        // must carry that kind byte anyway, so it costs nothing.
        let kind_slot = unsafe {
            b.build_in_bounds_gep(i64_ty, box_ptr, &[i64_ty.const_int(1, false)], "kslot").map_err(e)?
        };
        let kind = b.build_load(i64_ty, kind_slot, "kind").map_err(e)?.into_int_value();
        let is_bound = b
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                kind,
                i64_ty.const_int(OBJKIND_BOUND_METHOD as u64, false),
                "isbm",
            )
            .map_err(e)?;
        let outer_fn = b.get_insert_block().unwrap().get_parent().unwrap();
        let bm_bb = self.ctx.append_basic_block(outer_fn, "icall_bound");
        let plain_bb = self.ctx.append_basic_block(outer_fn, "icall_plain");
        let bm_merge_bb = self.ctx.append_basic_block(outer_fn, "icall_bm_merge");
        b.build_conditional_branch(is_bound, bm_bb, plain_bb).map_err(e)?;

        // ── bound: load self from slot 2, prepend it, call with args+1 ──
        b.position_at_end(bm_bb);
        let self_slot = unsafe {
            b.build_in_bounds_gep(i64_ty, box_ptr, &[i64_ty.const_int(2, false)], "sslot").map_err(e)?
        };
        let self_word = b.build_load(i64_ty, self_slot, "selfw").map_err(e)?.into_int_value();
        let bm_arg_tys = vec![i64_ty.into(); args.len() + 1];
        let bm_fn_ty = i64_ty.fn_type(&bm_arg_tys, false);
        let mut bm_argv: Vec<BasicMetadataValueEnum> = Vec::with_capacity(args.len() + 1);
        bm_argv.push(self_word.into());
        for a in args {
            bm_argv.push(self.load(*a).into());
        }
        let bm_ret = b
            .build_indirect_call(bm_fn_ty, fn_ptr, &bm_argv, "bmcall")
            .map_err(e)?
            .as_any_value_enum()
            .into_int_value();
        b.build_unconditional_branch(bm_merge_bb).map_err(e)?;
        let bm_end = b.get_insert_block().unwrap();

        b.position_at_end(plain_bb);

        // Sentinel = the jrt_native_call address.
        let native_fn = self.runtime_fn(
            "jrt_native_call",
            i64_ty.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into(), i64_ty.into()], false),
        );
        let sentinel = native_fn.as_global_value().as_pointer_value();
        let fp_int = b.build_ptr_to_int(fn_ptr, i64_ty, "fpi").map_err(e)?;
        let sent_int = b.build_ptr_to_int(sentinel, i64_ty, "si").map_err(e)?;
        let is_native = b
            .build_int_compare(inkwell::IntPredicate::EQ, fp_int, sent_int, "isnat")
            .map_err(e)?;

        let cur_fn = b.get_insert_block().unwrap().get_parent().unwrap();
        let nat_bb = self.ctx.append_basic_block(cur_fn, "icall_native");
        let reg_bb = self.ctx.append_basic_block(cur_fn, "icall_reg");
        let merge_bb = self.ctx.append_basic_block(cur_fn, "icall_merge");
        b.build_conditional_branch(is_native, nat_bb, reg_bb).map_err(e)?;

        // ── native: read env {handle, name}, marshal args, jrt_native_call ──
        // env is at slot 2; slot 1 is the ObjKind word (see emit_native_fn_value).
        b.position_at_end(nat_bb);
        let env_slot = unsafe {
            b.build_in_bounds_gep(ptrt, box_ptr, &[i64_ty.const_int(2, false)], "envs").map_err(e)?
        };
        let env = b.build_load(ptrt, env_slot, "env").map_err(e)?.into_pointer_value();
        let handle = b.build_load(ptrt, env, "nh").map_err(e)?.into_pointer_value();
        let name_slot = unsafe {
            b.build_in_bounds_gep(ptrt, env, &[i64_ty.const_int(1, false)], "nns").map_err(e)?
        };
        let name = b.build_load(ptrt, name_slot, "nn").map_err(e)?.into_pointer_value();
        let argv = if args.is_empty() {
            ptrt.const_null()
        } else {
            let arr = b
                .build_array_alloca(i64_ty, i64_ty.const_int(args.len() as u64, false), "iargv")
                .map_err(e)?;
            for (i, a) in args.iter().enumerate() {
                let slot = unsafe {
                    b.build_in_bounds_gep(i64_ty, arr, &[i64_ty.const_int(i as u64, false)], "ia").map_err(e)?
                };
                b.build_store(slot, self.load(*a)).map_err(e)?;
            }
            arr
        };
        let nat_ret = b
            .build_call(
                native_fn,
                &[handle.into(), name.into(), argv.into(), i64_ty.const_int(args.len() as u64, false).into()],
                "natret",
            )
            .map_err(e)?
            .as_any_value_enum()
            .into_int_value();
        b.build_unconditional_branch(merge_bb).map_err(e)?;
        let nat_end = b.get_insert_block().unwrap();

        // ── regular: direct indirect call jf_ptr(args) ──
        b.position_at_end(reg_bb);
        let arg_tys = vec![i64_ty.into(); args.len()];
        let fn_ty = i64_ty.fn_type(&arg_tys, false);
        let cargv: Vec<BasicMetadataValueEnum> = args.iter().map(|a| self.load(*a).into()).collect();
        let reg_ret = b
            .build_indirect_call(fn_ty, fn_ptr, &cargv, "icall")
            .map_err(e)?
            .as_any_value_enum()
            .into_int_value();
        b.build_unconditional_branch(merge_bb).map_err(e)?;
        let reg_end = b.get_insert_block().unwrap();

        // ── merge ──
        b.position_at_end(merge_bb);
        let phi = b.build_phi(i64_ty, "icall_ret").map_err(e)?;
        phi.add_incoming(&[(&nat_ret, nat_end), (&reg_ret, reg_end)]);
        let plain_ret = phi.as_basic_value().into_int_value();
        b.build_unconditional_branch(bm_merge_bb).map_err(e)?;
        let plain_end = b.get_insert_block().unwrap();

        // ── merge the bound and plain paths ──
        b.position_at_end(bm_merge_bb);
        let outer_phi = b.build_phi(i64_ty, "icall_out").map_err(e)?;
        outer_phi.add_incoming(&[(&bm_ret, bm_end), (&plain_ret, plain_end)]);
        Ok(outer_phi.as_basic_value().into_int_value())
    }

    /// The module-level global cell for `name`, created (initialized to nil) on
    /// first reference. Globals are module-scoped — shared across the top-level
    /// chunk and every lowered `fn_def` — so they must be LLVM globals keyed by
    /// name, not function allocas.
    fn global_slot(&self, name: &str) -> PointerValue<'ctx> {
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
    fn untag_int(&self, v: IntValue<'ctx>) -> IntValue<'ctx> {
        self.builder
            .build_right_shift(v, self.i64t().const_int(1, false), true, "utag")
            .unwrap()
    }

    /// Tag a native i64 as an int word (shift left by 1; low bit 0).
    fn tag_int(&self, v: IntValue<'ctx>) -> IntValue<'ctx> {
        self.builder
            .build_left_shift(v, self.i64t().const_int(1, false), "tag")
            .unwrap()
    }

    /// Untag a bool word to an `i1` (bit 4 holds the value).
    fn untag_bool(&self, v: IntValue<'ctx>) -> IntValue<'ctx> {
        let shifted = self
            .builder
            .build_right_shift(v, self.i64t().const_int(4, false), false, "bsh")
            .unwrap();
        let bit = self
            .builder
            .build_and(shifted, self.i64t().const_int(1, false), "band")
            .unwrap();
        self.builder
            .build_int_compare(IntPredicate::NE, bit, self.i64t().const_zero(), "btrue")
            .unwrap()
    }

    /// Untag both operands of a binary int op.
    fn int_operands(&self, l: Reg, r: Reg) -> (IntValue<'ctx>, IntValue<'ctx>) {
        (self.untag_int(self.load(l)), self.untag_int(self.load(r)))
    }

    /// Wrap an `i1` as a tagged bool word (`true`→0x1f, `false`→0x0f).
    fn bool_word(&self, b: IntValue<'ctx>) -> IntValue<'ctx> {
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
    fn zext_bool(&self, r: Reg) -> IntValue<'ctx> {
        let b = self.untag_bool(self.load(r));
        self.builder.build_int_z_extend(b, self.i64t(), "zb").unwrap()
    }

    /// Read register `r` as an f64, widening from int if `is_int` (mixed
    /// int/float comparisons).
    fn as_float(&self, r: Reg, is_int: bool) -> FloatValue<'ctx> {
        if is_int {
            let i = self.untag_int(self.load(r));
            self.builder.build_signed_int_to_float(i, self.f64t(), "i2fc").unwrap()
        } else {
            self.unbox_float(self.load(r))
        }
    }

    fn int_cmp(&self, l: Reg, r: Reg, pred: IntPredicate) -> IntValue<'ctx> {
        let (a, c) = self.int_operands(l, r);
        let b = self.builder.build_int_compare(pred, a, c, "icmp").unwrap();
        self.bool_word(b)
    }

    fn float_cmp(&self, l: Reg, r: Reg, pred: FloatPredicate) -> IntValue<'ctx> {
        let (a, c) = self.float_operands(l, r);
        let b = self.builder.build_float_compare(pred, a, c, "fcmp").unwrap();
        self.bool_word(b)
    }

    fn bool_cmp(&self, l: Reg, r: Reg, pred: IntPredicate) -> IntValue<'ctx> {
        let (a, c) = (self.zext_bool(l), self.zext_bool(r));
        let b = self.builder.build_int_compare(pred, a, c, "bcmp").unwrap();
        self.bool_word(b)
    }

    /// Mixed int/float ordering: widen whichever side is an int, then `fcmp`.
    fn mixed_cmp(&self, l: Reg, l_int: bool, r: Reg, r_int: bool, pred: FloatPredicate) -> IntValue<'ctx> {
        let a = self.as_float(l, l_int);
        let c = self.as_float(r, r_int);
        let b = self.builder.build_float_compare(pred, a, c, "mcmp").unwrap();
        self.bool_word(b)
    }

    /// Native i64 bitwise/shift op on two untagged int operands, re-tagged.
    fn int_bitop(
        &self,
        l: Reg,
        r: Reg,
        f: impl FnOnce(IntValue<'ctx>, IntValue<'ctx>) -> Result<IntValue<'ctx>, String>,
    ) -> Result<IntValue<'ctx>, String> {
        let (a, c) = self.int_operands(l, r);
        Ok(self.tag_int(f(a, c)?))
    }

    // ── Dynamic (Unknown-operand) ops → tag-dispatching runtime (A7) ─────────
    // Operands are tagged words in slots, so — unlike the legacy static-SSA
    // backend — we pass the slot words straight to the `jrt_*_any` helpers (the
    // same decision core the VM runs via `dynop`). Arithmetic returns a tagged
    // word; comparisons return an i32 that we fold into a bool word.

    /// Dynamic binary arithmetic `jrt_<name>(i64, i64) -> i64` (tagged result).
    fn any2(&self, name: &str, l: Reg, r: Reg) -> IntValue<'ctx> {
        let f = self.runtime_fn(
            name,
            self.i64t().fn_type(&[self.i64t().into(), self.i64t().into()], false),
        );
        self.builder
            .build_call(f, &[self.load(l).into(), self.load(r).into()], "any2")
            .unwrap()
            .as_any_value_enum()
            .into_int_value()
    }

    /// `jrt_eq_any(i64, i64) -> i32` (1 when equal).
    fn eq_any(&self, l: Reg, r: Reg) -> IntValue<'ctx> {
        let f = self.runtime_fn(
            "jrt_eq_any",
            self.ctx.i32_type().fn_type(&[self.i64t().into(), self.i64t().into()], false),
        );
        self.builder
            .build_call(f, &[self.load(l).into(), self.load(r).into()], "eqany")
            .unwrap()
            .as_any_value_enum()
            .into_int_value()
    }

    /// `jrt_cmp_any(i64, i64) -> i32` (three-way: -1 / 0 / 1).
    fn cmp_any(&self, l: Reg, r: Reg) -> IntValue<'ctx> {
        let f = self.runtime_fn(
            "jrt_cmp_any",
            self.ctx.i32_type().fn_type(&[self.i64t().into(), self.i64t().into()], false),
        );
        self.builder
            .build_call(f, &[self.load(l).into(), self.load(r).into()], "cmpany")
            .unwrap()
            .as_any_value_enum()
            .into_int_value()
    }

    /// `jrt_neg_any(i64) -> i64` (tagged result).
    fn neg_any(&self, s: Reg) -> IntValue<'ctx> {
        let f = self.runtime_fn("jrt_neg_any", self.i64t().fn_type(&[self.i64t().into()], false));
        self.builder
            .build_call(f, &[self.load(s).into()], "negany")
            .unwrap()
            .as_any_value_enum()
            .into_int_value()
    }

    /// Fold an i32 (from `eq_any`/`cmp_any`) compared to zero into a bool word.
    fn i32cmp_word(&self, v: IntValue<'ctx>, pred: IntPredicate) -> IntValue<'ctx> {
        let z = self.ctx.i32_type().const_zero();
        let b = self.builder.build_int_compare(pred, v, z, "i32c").unwrap();
        self.bool_word(b)
    }

    /// Compare two tagged-string words via libc `strcmp` on their data pointers
    /// (mirrors the legacy typed-string path), folding into a bool word. Strings
    /// carry their own tag (`TAG_STR`), so this needs no per-object kind header.
    fn str_cmp(&self, l: Reg, r: Reg, pred: IntPredicate) -> IntValue<'ctx> {
        let lp = self.untag_ptr(self.load(l));
        let rp = self.untag_ptr(self.load(r));
        let f = self.runtime_fn(
            "strcmp",
            self.ctx.i32_type().fn_type(&[self.ptrt().into(), self.ptrt().into()], false),
        );
        let c = self
            .builder
            .build_call(f, &[lp.into(), rp.into()], "scmp")
            .unwrap()
            .as_any_value_enum()
            .into_int_value();
        self.i32cmp_word(c, pred)
    }

    /// Materialize a compiled default-parameter value (always a literal, per
    /// `emit_fn`) as a tagged word for a call-site argument fill. Mirrors the
    /// tagged ABI used by the constant-load opcodes.
    fn default_word(&self, v: &VmValue) -> Result<IntValue<'ctx>, String> {
        Ok(match v {
            VmValue::Int(n) => self.i64t().const_int((n.wrapping_shl(1)) as u64, false),
            VmValue::Bool(b) => self.i64t().const_int(if *b { TRUE } else { FALSE }, false),
            VmValue::Nil => self.i64t().const_int(NIL, false),
            VmValue::Float(f) => self.box_float(self.f64t().const_float(*f)),
            VmValue::Str(s) => self.str_literal_word(s)?,
            other => return Err(format!("lower.rs: unsupported default value {other:?}")),
        })
    }
}

/// Builtins the Chunk backend can lower directly (devirtualized from
/// `GetGlobal(name)` + `Call`). A name here is only trusted when the program
/// never `SetGlobal`s it (so the global still holds the builtin, not a user
/// value). Grows as more builtins are supported.
const LOWERABLE_BUILTINS: &[&str] = &["print", "str", "int", "float", "bool", "len"];

/// The single register an instruction writes, or `None` for pure
/// stores/control-flow. Used to invalidate builtin tracking when a register is
/// overwritten; it must name **every** register-writing opcode (missing one
/// would let a stale devirtualization survive an overwrite = miscompile).
fn dest_reg(instr: &Instr) -> Option<Reg> {
    use Instr::*;
    match instr {
        LoadInt(d, _) | LoadFloat(d, _) | LoadBool(d, _) | LoadStr(d, _) | LoadNil(d)
        | LoadFn(d, _) | MakeClosure(d, _) | GetLocal(d, _) | GetGlobal(d, _) => Some(*d),
        Move(d, _) | NegInt(d, _) | NegFloat(d, _) | IntToFloat(d, _) | BitNot(d, _)
        | Not(d, _) | MakePrompt(d, _) | UnaryOp(d, _, _) | GetTypeName(d, _)
        | Await(d, _) | PromptDeref(d, _, _, _) => Some(*d),
        AddInt(d, _, _) | SubInt(d, _, _) | MulInt(d, _, _) | DivInt(d, _, _) | ModInt(d, _, _)
        | AddFloat(d, _, _) | SubFloat(d, _, _) | MulFloat(d, _, _) | DivFloat(d, _, _)
        | ConcatStr(d, _, _) | BitAnd(d, _, _) | BitOr(d, _, _) | BitXor(d, _, _)
        | Shl(d, _, _) | Shr(d, _, _) | BinOp(d, _, _, _) | GetIndex(d, _, _)
        | GetField(d, _, _) => Some(*d),
        CmpEqInt(d, ..) | CmpNeInt(d, ..) | CmpLtInt(d, ..) | CmpGtInt(d, ..) | CmpLeInt(d, ..)
        | CmpGeInt(d, ..) | CmpEqFloat(d, ..) | CmpNeFloat(d, ..) | CmpLtFloat(d, ..)
        | CmpGtFloat(d, ..) | CmpLeFloat(d, ..) | CmpGeFloat(d, ..) | CmpLtIntFloat(d, ..)
        | CmpGtIntFloat(d, ..) | CmpLeIntFloat(d, ..) | CmpGeIntFloat(d, ..)
        | CmpLtFloatInt(d, ..) | CmpGtFloatInt(d, ..) | CmpLeFloatInt(d, ..)
        | CmpGeFloatInt(d, ..) | CmpEqBool(d, ..) | CmpNeBool(d, ..) | CmpLtBool(d, ..)
        | CmpGtBool(d, ..) | CmpLeBool(d, ..) | CmpGeBool(d, ..) | CmpEqStr(d, ..)
        | CmpNeStr(d, ..) | CmpLtStr(d, ..) | CmpGtStr(d, ..) | CmpLeStr(d, ..)
        | CmpGeStr(d, ..) | CmpEq(d, ..) | CmpNe(d, ..) | CmpLt(d, ..) | CmpGt(d, ..)
        | CmpLe(d, ..) | CmpGe(d, ..) => Some(*d),
        Call(d, _, _) | CallNamed(d, _, _) | Spawn(d, _, _) | Join(d, _) | MakeArray(d, _)
        | MakeDict(d, _) | MakeStruct(d, _, _) | BuildFStr(d, _) => Some(*d),
        // Handler binds its caught register (in the landing block).
        SetupHandler(r, _) => Some(*r),
        // Pure stores / control flow / no-reg-dest.
        SetGlobal(..) | SetLocal(..) | SetIndex(..) | SetField(..) | Jump(_)
        | JumpIfFalse(..) | JumpIfTrue(..) | Return(_) | Halt | Raise(_) | PopHandler
        | ImportFile(..) | ImportFrom(..) => None,
    }
}

/// A call the pre-scan devirtualized to a supported builtin.
struct BuiltinCall {
    name: &'static str,
    args: Vec<Reg>,
}

/// Resolve which `Call`s target a lowerable builtin. Tracks, forward over the
/// flat stream, which registers hold a builtin function value (bound by
/// `GetGlobal(builtin)` and never overwritten). Sound because the tracked
/// globals are immutable (the no-`SetGlobal` guard) and any write to a tracked
/// register clears it — so a resolution can never name the wrong callee; at
/// worst it conservatively declines and the Call falls back.
fn resolve_builtin_calls(code: &[Instr]) -> HashMap<usize, BuiltinCall> {
    let reassigned: std::collections::HashSet<&str> = code
        .iter()
        .filter_map(|i| match i {
            Instr::SetGlobal(n, _) => Some(n.as_str()),
            _ => None,
        })
        .collect();
    let mut reg_builtin: HashMap<Reg, &'static str> = HashMap::new();
    let mut out: HashMap<usize, BuiltinCall> = HashMap::new();
    for (i, instr) in code.iter().enumerate() {
        match instr {
            Instr::GetGlobal(d, name) => {
                match LOWERABLE_BUILTINS.iter().copied().find(|b| *b == name.as_str()) {
                    Some(b) if !reassigned.contains(name.as_str()) => {
                        reg_builtin.insert(*d, b);
                    }
                    _ => {
                        reg_builtin.remove(d);
                    }
                }
            }
            Instr::Call(d, callee, args) => {
                if let Some(&b) = reg_builtin.get(callee) {
                    // Only resolve arities this backend lowers; others fall back.
                    let ok = match b {
                        "print" | "str" | "int" | "float" | "bool" | "len" => args.len() == 1,
                        _ => false,
                    };
                    if ok {
                        out.insert(i, BuiltinCall { name: b, args: args.clone() });
                    }
                }
                reg_builtin.remove(d);
            }
            other => {
                if let Some(d) = dest_reg(other) {
                    reg_builtin.remove(&d);
                }
            }
        }
    }
    out
}

// ── User-function lowering (A6b) ───────────────────────────────────────────────
//
// The top-level chunk and every reachable `fn_def`/closure becomes its own LLVM
// function `jf_<uid>`. Function values are **first-class**: `LoadFn`/`MakeClosure`
// materialize a boxed function pointer (`fn_box_word`), so a function can be
// stored, passed as an argument, and returned. A `Call` whose callee is a
// statically-known function is **devirtualized** to a direct `call jf_<uid>`
// (filling omitted defaults); any other function value is called **indirectly**
// through its box. Closures capture only globals (the language forbids nested
// `fn`s and local capture — a closure body reads outer names as `GetGlobal`), so
// a closure is just a plain `jf_<uid>` with no environment. Only a call to a
// reserved builtin this backend doesn't lower (e.g. `len`) forces a fallback.

/// Whole-program function registry, shared across every chunk being lowered.
struct FnCtx<'ctx> {
    /// uid → the pre-declared `jf_<uid>` LLVM function (declared before any body
    /// is lowered, so recursion and mutual recursion resolve).
    funcs: Vec<FunctionValue<'ctx>>,
    /// uid → the compiled function (params/defaults/body).
    defs: Vec<Arc<CompiledFn>>,
    /// `Arc<CompiledFn>` identity → uid, for resolving a chunk-local `LoadFn`
    /// index (`chunk.fn_defs[idx]`) to its global uid.
    ptr2uid: HashMap<*const CompiledFn, usize>,
    /// Global variable name → uid, for a name that is *provably* a function: it
    /// is `SetGlobal`'d exactly once in the whole program, from a `LoadFn`. A
    /// decorated function (`@dec`) writes the global twice, so it is excluded —
    /// its identity changes at runtime, and the call must fall back.
    global_fns: HashMap<String, usize>,
    /// Struct type name → its optional fields' (name, scalar-literal default),
    /// used to fill fields a struct literal omits (the VM fills these at runtime).
    struct_defaults: HashMap<String, Vec<(String, VmValue)>>,
    /// Every declared struct type name.
    ///
    /// Used to recognise a *call* on a type name, which is an error worth
    /// naming: a struct type is not a function, and without this the call falls
    /// through to `Indirect` and jumps through a global cell codegen never
    /// assigns for a type name.
    ///
    /// The field lists are carried too, rather than just the names, because
    /// `struct_defaults` cannot answer "what fields does this type have?" — it
    /// keeps only optional fields with scalar-literal defaults.
    struct_field_names: HashMap<String, Vec<String>>,
    /// Extend-block method NAME → its candidate implementations `(uid, required,
    /// total)`, where `required`/`total` are the arg counts (excluding `self`)
    /// the method accepts (`required` = params without a default, `total` = all
    /// params). A call `obj.name(k args)` resolves to the *one* candidate whose
    /// range accepts `k` (`required <= k <= total`); if exactly one matches it
    /// devirtualizes to a direct `jf_<uid>` call, so same-named methods on
    /// different types disambiguate by arity (`put(a,b)` vs `put(a)`) with no
    /// runtime type dispatch. Two candidates accepting the same `k` (same name +
    /// arity on two types) are genuinely ambiguous → decline.
    method_candidates: HashMap<String, Vec<(usize, usize, usize)>>,
    /// Global names the program `SetGlobal`s (assigns) anywhere. A name here is a
    /// user variable, so `name.method(...)` is a value method, NOT a stdlib module
    /// call — even when `name` happens to be a reserved module name (`let sh = []`
    /// shadows `use std::sh`). Guards `module.method` recognition against shadowing.
    user_globals: std::collections::HashSet<String>,
    /// Whether the whole program is "collections-only" (no first-class functions,
    /// no async): if so, every `TAG_PTR` word is guaranteed to be an `ObjHeader`
    /// collection, so codegen emits reference-counting (incref/decref/scope-exit)
    /// and turns the runtime's `RC_ACTIVE` on via `jrt_rc_enable`. Otherwise no rc
    /// is emitted and heap objects leak (the pre-B4.2 behavior). See `gc.rs`.
    refcount: bool,
}

impl<'ctx> FnCtx<'ctx> {
    fn empty() -> Self {
        FnCtx {
            funcs: Vec::new(),
            defs: Vec::new(),
            ptr2uid: HashMap::new(),
            global_fns: HashMap::new(),
            struct_defaults: HashMap::new(),
            struct_field_names: HashMap::new(),
            method_candidates: HashMap::new(),
            user_globals: std::collections::HashSet::new(),
            refcount: false,
        }
    }

    /// uid of the function `fn_defs[idx]` refers to (by `Arc` identity).
    fn uid_of(&self, fn_defs: &[Arc<CompiledFn>], idx: usize) -> Option<usize> {
        fn_defs.get(idx).and_then(|f| self.ptr2uid.get(&Arc::as_ptr(f)).copied())
    }

    /// Resolve an extend-method call `obj.name(k args)` to a single `jf_<uid>`:
    /// the one candidate named `name` whose arg range accepts `k` (`required <= k
    /// <= total`, both excluding `self`). Returns `None` if no candidate matches
    /// (not a struct method) or more than one does (same name+arity on two types
    /// — genuinely ambiguous, needs runtime type dispatch).
    fn resolve_method(&self, name: &str, k: usize) -> Option<usize> {
        let cands = self.method_candidates.get(name)?;
        let mut hit = None;
        for &(uid, required, total) in cands {
            if required <= k && k <= total {
                if hit.is_some() {
                    return None; // ambiguous
                }
                hit = Some(uid);
            }
        }
        hit
    }
}

/// Assign a stable uid to every `CompiledFn` reachable from `top` (breadth-first
/// so parents precede children), returning the uid→def table and the identity map.
fn collect_fns(top: &Chunk) -> (Vec<Arc<CompiledFn>>, HashMap<*const CompiledFn, usize>) {
    let mut defs: Vec<Arc<CompiledFn>> = Vec::new();
    let mut ptr2uid: HashMap<*const CompiledFn, usize> = HashMap::new();
    let mut queue: VecDeque<Arc<CompiledFn>> = top.fn_defs.iter().cloned().collect();
    while let Some(f) = queue.pop_front() {
        let uid = defs.len();
        ptr2uid.insert(Arc::as_ptr(&f), uid);
        for c in &f.chunk.fn_defs {
            queue.push_back(c.clone());
        }
        defs.push(f);
    }
    (defs, ptr2uid)
}

/// Append every extend-block method body to `defs`/`ptr2uid` (assigning uids and
/// BFS-collecting each method's nested `fn_defs`), and return the method-name →
/// candidate-`(uid, required, total)` map (arg counts exclude `self`). A method
/// body is an ordinary `CompiledFn` whose first parameter is `self`, so once it
/// has a uid the normal forward-declare / lower / task-wrapper loops emit it like
/// any other function.
fn collect_method_fns(
    extend_methods: &HashMap<String, HashMap<String, Arc<CompiledFn>>>,
    defs: &mut Vec<Arc<CompiledFn>>,
    ptr2uid: &mut HashMap<*const CompiledFn, usize>,
) -> HashMap<String, Vec<(usize, usize, usize)>> {
    let mut candidates: HashMap<String, Vec<(usize, usize, usize)>> = HashMap::new();
    let mut queue: VecDeque<Arc<CompiledFn>> = VecDeque::new();
    // Deterministic order: sort by (type, method) so uids are stable across runs.
    let mut types: Vec<&String> = extend_methods.keys().collect();
    types.sort();
    for ty in types {
        let methods = &extend_methods[ty];
        let mut names: Vec<&String> = methods.keys().collect();
        names.sort();
        for name in names {
            let mfn = &methods[name];
            let uid = match ptr2uid.get(&Arc::as_ptr(mfn)) {
                Some(&u) => u,
                None => {
                    let u = defs.len();
                    ptr2uid.insert(Arc::as_ptr(mfn), u);
                    defs.push(mfn.clone());
                    queue.push_back(mfn.clone());
                    u
                }
            };
            // Arg counts excluding `self` (param 0): `total` = all trailing params,
            // `required` = those without a default.
            let total = mfn.params.len().saturating_sub(1);
            let required = (1..mfn.params.len())
                .filter(|&j| mfn.defaults.get(j).and_then(|d| d.as_ref()).is_none())
                .count();
            candidates.entry(name.clone()).or_default().push((uid, required, total));
        }
    }
    // BFS the method bodies' nested function literals.
    while let Some(f) = queue.pop_front() {
        for c in &f.chunk.fn_defs {
            if !ptr2uid.contains_key(&Arc::as_ptr(c)) {
                let u = defs.len();
                ptr2uid.insert(Arc::as_ptr(c), u);
                defs.push(c.clone());
                queue.push_back(c.clone());
            }
        }
    }
    candidates
}

/// Names that provably hold a function: bound once (whole-program) from a
/// `LoadFn` at top level. The single-assignment guard is checked across *every*
/// chunk, so a name a nested function later rebinds to a non-function is excluded.
fn build_global_fns(
    top: &Chunk,
    defs: &[Arc<CompiledFn>],
    ptr2uid: &HashMap<*const CompiledFn, usize>,
) -> HashMap<String, usize> {
    // Count SetGlobal writes to each name across the whole program.
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut count_in = |chunk: &Chunk| {
        for instr in &chunk.code {
            if let Instr::SetGlobal(n, _) = instr {
                *counts.entry(n.clone()).or_default() += 1;
            }
        }
    };
    count_in(top);
    for d in defs {
        count_in(&d.chunk);
    }

    // Forward-track which top-level register holds which function, and record
    // the name each such register is stored to.
    let mut reg_fn: HashMap<Reg, usize> = HashMap::new();
    let mut candidate: HashMap<String, usize> = HashMap::new();
    for instr in &top.code {
        match instr {
            Instr::LoadFn(d, idx) => {
                match ptr2uid.get(&Arc::as_ptr(&top.fn_defs[*idx])) {
                    Some(&uid) => { reg_fn.insert(*d, uid); }
                    None => { reg_fn.remove(d); }
                }
            }
            Instr::SetGlobal(name, src) => {
                if let Some(&uid) = reg_fn.get(src) {
                    candidate.insert(name.clone(), uid);
                }
            }
            other => {
                if let Some(d) = dest_reg(other) {
                    reg_fn.remove(&d);
                }
            }
        }
    }

    candidate
        .into_iter()
        .filter(|(name, _)| counts.get(name).copied() == Some(1))
        .collect()
}

/// How the backend lowers a `Call`. A callee whose function is statically known
/// becomes a `Direct` call to `jf_<uid>` (filling omitted trailing defaults); a
/// callee that is a runtime function value (a parameter, a variable, an escaped
/// closure) becomes an `Indirect` call through its boxed function pointer. A
/// builtin call this backend lowers itself (print/str/int/…) is left out of the
/// map (handled by `resolve_builtin_calls`); a call to a reserved builtin we do
/// *not* lower makes the whole program decline (`Err`) to the legacy path.
enum CallKind {
    Direct { uid: usize, args: Vec<Reg> },
    /// A keyword-argument call to a known function, pre-resolved to one slot per
    /// parameter: `Some(reg)` was supplied (positionally or by name), `None` is
    /// filled from the parameter's default at the call site.
    DirectNamed { uid: usize, arg_slots: Vec<Option<Reg>> },
    /// A struct method call `obj.name(args)` where `name` is a unique extend-block
    /// method → direct call to `jf_<uid>` with the receiver (`self_reg`) prepended
    /// as `self` (param 0) and omitted trailing defaults filled at the call site.
    MethodDirect { uid: usize, self_reg: Reg, args: Vec<Reg> },
    /// A genuinely-ambiguous struct method call `obj.method(args)` — two types
    /// define `method` with the same arity, so the target depends on `obj`'s
    /// runtime type. Looked up at runtime by (type-name, method) via
    /// `jrt_method_lookup` and called indirectly (`self` prepended). See
    /// `emit_dynamic_method`.
    MethodDynamic { recv: Reg, method: String, args: Vec<Reg> },
    /// `stream(?p)` / `stream(?p, mute_on=[g])` — streaming inference that
    /// prints tokens as they arrive and evaluates to the full response.
    ///
    /// `prompt` is the *un-dereferenced* prompt register: the producing
    /// `PromptDeref` is elided, because letting it run would infer twice (once
    /// for the deref, once for the stream) and print the response twice. That
    /// is the same hazard the non-streaming `?p` lowering documents, arrived at
    /// from the other direction.
    StreamCall { prompt: Reg, grammar: Option<Reg> },
    /// A stdlib module-namespace call `module.method(args)` (`fs.read`, `path.ext`,
    /// …) resolved statically by name to a runtime symbol. Only layout-safe methods
    /// (string/scalar I/O — no legacy-layout collections) are lowered; the rest
    /// decline. See `emit_module_call`.
    ModuleCall { module: String, method: String, args: Vec<Reg> },
    /// A native (C-ABI) package call `__native$<pkgid>$<fn>(args)` → dispatch
    /// through `jrt_native_call` against the `dlopen`'d package handle. Args and
    /// the result are already tagged words. See `emit_native_call`.
    NativeCall { pkgid: u32, fname: String, args: Vec<Reg> },
    /// A string primitive method `s.method(args)` (`trim`/`upper`/`starts_with`/…)
    /// → the shared `jrt_str_*` symbol. Strings have one representation across both
    /// paths, so these reuse the legacy string helpers directly. See
    /// `emit_str_method`. (Method names unique to strings; `contains`/`split` are
    /// excluded — ambiguous with dict / returns a collection.)
    PrimStrMethod { recv: Reg, method: String, args: Vec<Reg> },
    /// An array/dict primitive method `recv.method(args)` whose name is unique to
    /// one collection kind (`push`/`pop`/`sort`/`reverse` → array;
    /// `keys`/`values`/`has`/`get` → dict), so the receiver kind is known by name
    /// (frontend-checked). Lowered via the ObjHeader-aware `jrt_coll_*`/`jrt_karr_*`
    /// helpers. See `emit_val_method`. (`contains`/`len` are ambiguous → excluded.)
    PrimValMethod { recv: Reg, method: String, args: Vec<Reg> },
    Indirect,
    /// `Spawn` of a statically-known async function → `jade_spawn(jf_task_<uid>,
    /// args, n)`. Only exact-arity spawns of a known function are lowered.
    Spawn { uid: usize, args: Vec<Reg> },
}

/// Classify every `Call` in `code`. Function values are first-class (materialized
/// as boxed pointers), so nothing "escapes" — the only decline is a call to a
/// reserved builtin this backend doesn't lower (e.g. `len`), which must go to the
/// legacy path. Direct calls are a devirtualization optimization; every other
/// call (a runtime function value) lowers to an indirect call, sound because the
/// frontend guarantees a `Call`'s callee is callable and non-user-fn callables
/// (builtins/methods) arrive via `GetGlobal(reserved)`/`GetField` — the former
/// handled here, the latter an unsupported opcode that already forces fallback.
fn resolve_user_calls(
    code: &[Instr],
    fn_defs: &[Arc<CompiledFn>],
    fnctx: &FnCtx,
) -> Result<(HashMap<usize, CallKind>, std::collections::HashSet<usize>), String> {
    // reg → uid of a statically-known function (for direct-call devirtualization).
    let mut reg_fn: HashMap<Reg, usize> = HashMap::new();
    // local slot → uid (a function stored into a local).
    let mut slot_fn: HashMap<u32, usize> = HashMap::new();
    // reg → the global name it was last loaded from (to classify builtin callees).
    let mut reg_global: HashMap<Reg, String> = HashMap::new();
    // reg holding a `GetField` result → (receiver reg, field/method name, the
    // GetField's instruction index). Calling one is a method call: a unique struct
    // method devirtualizes (self = receiver), anything else declines.
    let mut reg_getfield: HashMap<Reg, (Reg, String, usize)> = HashMap::new();
    // reg holding a `module.method` GetField result → (module name, method, the
    // GetField's instruction index). The base was `GetGlobal`'d from a reserved
    // stdlib module name, so calling it is a module call, not a value method.
    let mut reg_getfield_module: HashMap<Reg, (String, String, usize)> = HashMap::new();
    let mut out: HashMap<usize, CallKind> = HashMap::new();
    // GetField instruction indices whose result is consumed *only* as the callee of
    // a devirtualized method call. Their field is a method (not a data field), so
    // lowering them would raise "undefined field" — the method dispatch replaces
    // them, so `lower_body` skips these opcodes entirely.
    //
    // Also carries `PromptDeref`s subsumed by a `stream()` call; the name is
    // historical, the set is just "instruction indices `lower_body` must skip".
    let mut skip_getfields: std::collections::HashSet<usize> = std::collections::HashSet::new();
    // reg holding an *unconstrained* `?p` result → (the prompt reg, the deref's
    // instruction index). Only `stream()` consumes this: it needs the prompt
    // itself, not an already-inferred response.
    let mut reg_promptderef: HashMap<Reg, (Reg, usize)> = HashMap::new();
    // reg holding an array built from a literal → its element regs, so
    // `mute_on=[g]` can be resolved to the single grammar it carries.
    let mut reg_array_lit: HashMap<Reg, Vec<Reg>> = HashMap::new();

    for (i, instr) in code.iter().enumerate() {
        match instr {
            Instr::GetField(d, obj, field) => {
                reg_fn.remove(d);
                // A field access whose base was loaded from a reserved stdlib
                // module global is a `module.method` access (resolved by name) —
                // UNLESS the program assigns that name (a user variable shadowing
                // the module, e.g. `let sh = []`), in which case it's a value method.
                let module = reg_global
                    .get(obj)
                    .filter(|n| is_stdlib_module(n) && !fnctx.user_globals.contains(n.as_str()))
                    .cloned();
                reg_global.remove(d);
                reg_getfield.remove(d);
                reg_getfield_module.remove(d);
                match module {
                    Some(m) => { reg_getfield_module.insert(*d, (m, field.clone(), i)); }
                    None => { reg_getfield.insert(*d, (*obj, field.clone(), i)); }
                }
                continue;
            }
            Instr::PromptDeref(d, prompt, output_type, grammar) => {
                reg_fn.remove(d);
                reg_global.remove(d);
                reg_getfield.remove(d);
                reg_getfield_module.remove(d);
                reg_array_lit.remove(d);
                reg_promptderef.remove(d);
                // Only an unconstrained deref can be folded into a stream: a
                // typed or grammar-constrained one has its own inference call
                // with different semantics.
                if output_type.is_none() && grammar.is_none() {
                    reg_promptderef.insert(*d, (*prompt, i));
                }
                continue;
            }
            Instr::MakeArray(d, elems) => {
                reg_fn.remove(d);
                reg_global.remove(d);
                reg_getfield.remove(d);
                reg_getfield_module.remove(d);
                reg_promptderef.remove(d);
                reg_array_lit.insert(*d, elems.clone());
                continue;
            }
            Instr::LoadFn(d, idx) | Instr::MakeClosure(d, idx) => {
                reg_global.remove(d);
                reg_getfield.remove(d);
                reg_getfield_module.remove(d);
                match fnctx.uid_of(fn_defs, *idx) {
                    Some(uid) => { reg_fn.insert(*d, uid); }
                    None => { reg_fn.remove(d); }
                }
            }
            Instr::Move(d, s) => {
                match reg_fn.get(s).copied() {
                    Some(u) => { reg_fn.insert(*d, u); }
                    None => { reg_fn.remove(d); }
                }
                reg_global.remove(d);
                // Propagate method-value-ness so `let m = obj.f; m()` still resolves.
                match reg_getfield.get(s).cloned() {
                    Some(v) => { reg_getfield.insert(*d, v); }
                    None => { reg_getfield.remove(d); }
                }
                match reg_getfield_module.get(s).cloned() {
                    Some(v) => { reg_getfield_module.insert(*d, v); }
                    None => { reg_getfield_module.remove(d); }
                }
            }
            Instr::GetGlobal(d, name) => {
                reg_getfield.remove(d);
                reg_getfield_module.remove(d);
                match fnctx.global_fns.get(name).copied() {
                    Some(u) => { reg_fn.insert(*d, u); }
                    None => { reg_fn.remove(d); }
                }
                reg_global.insert(*d, name.clone());
            }
            Instr::GetLocal(d, slot) => {
                match slot_fn.get(slot).copied() {
                    Some(u) => { reg_fn.insert(*d, u); }
                    None => { reg_fn.remove(d); }
                }
                reg_global.remove(d);
                reg_getfield.remove(d);
                reg_getfield_module.remove(d);
            }
            Instr::SetLocal(slot, src) => match reg_fn.get(src).copied() {
                Some(u) => { slot_fn.insert(*slot, u); }
                None => { slot_fn.remove(slot); }
            },
            Instr::SetGlobal(_, _) => {}
            // Spawn an async function: only a statically-known callee with an
            // exact-arity argument list is lowered (no defaults through spawn).
            Instr::Spawn(d, callee, args) => {
                if let Some(&uid) = reg_fn.get(callee) {
                    let cf = &fnctx.defs[uid];
                    if args.len() > cf.params.len() {
                        return Err("lower.rs: spawn passes more arguments than parameters".into());
                    }
                    for j in args.len()..cf.params.len() {
                        if cf.defaults.get(j).and_then(|x| x.as_ref()).is_none() {
                            return Err("lower.rs: spawn omits a required argument".into());
                        }
                    }
                    out.insert(i, CallKind::Spawn { uid, args: args.clone() });
                } else {
                    return Err("lower.rs: spawn of a non-static function".into());
                }
                reg_fn.remove(d);
                reg_global.remove(d);
                reg_getfield.remove(d);
                reg_getfield_module.remove(d);
            }
            Instr::Call(d, callee, args) => {
                if let Some((module, method, gf_idx)) = reg_getfield_module.get(callee).cloned() {
                    // A stdlib module call `module.method(args)`. Lower the
                    // layout-safe subset; anything else declines to the legacy path.
                    if chunk_module_supported(&module, &method, args.len()) {
                        out.insert(i, CallKind::ModuleCall { module, method, args: args.clone() });
                        skip_getfields.insert(gf_idx);
                    } else {
                        return Err(format!("lower.rs: unsupported module call {module}.{method}"));
                    }
                } else if let Some((self_reg, mname, gf_idx)) = reg_getfield.get(callee).cloned() {
                    // A method call `obj.mname(args)`. Devirtualize to the one
                    // extend-block method named `mname` whose arg range accepts this
                    // call's arg count (disambiguating same-named methods by arity);
                    // otherwise try primitive methods, else decline.
                    if let Some(uid) = fnctx.resolve_method(&mname, args.len()) {
                        out.insert(i, CallKind::MethodDirect { uid, self_reg, args: args.clone() });
                        // The producing GetField is a method lookup (would raise as a
                        // data-field access) and its result is now unused → skip it.
                        skip_getfields.insert(gf_idx);
                    } else if fnctx.method_candidates.contains_key(&mname) {
                        // A known extend method whose target is ambiguous by arity →
                        // dispatch on the receiver's runtime type.
                        out.insert(i, CallKind::MethodDynamic { recv: self_reg, method: mname, args: args.clone() });
                        skip_getfields.insert(gf_idx);
                    } else if chunk_str_method_supported(&mname, args.len()) {
                        out.insert(i, CallKind::PrimStrMethod { recv: self_reg, method: mname, args: args.clone() });
                        skip_getfields.insert(gf_idx);
                    } else if chunk_val_method_supported(&mname, args.len()) {
                        out.insert(i, CallKind::PrimValMethod { recv: self_reg, method: mname, args: args.clone() });
                        skip_getfields.insert(gf_idx);
                    } else {
                        return Err("lower.rs: method call (GetField result) is unsupported".into());
                    }
                } else {
                    let kind = if let Some(&uid) = reg_fn.get(callee) {
                        // Statically-known function → direct call (fill defaults).
                        let cf = &fnctx.defs[uid];
                        if args.len() > cf.params.len() {
                            return Err("lower.rs: call passes more arguments than parameters".into());
                        }
                        for j in args.len()..cf.params.len() {
                            if cf.defaults.get(j).and_then(|x| x.as_ref()).is_none() {
                                return Err("lower.rs: call omits a required argument".into());
                            }
                        }
                        Some(CallKind::Direct { uid, args: args.clone() })
                    } else if let Some(name) = reg_global.get(callee) {
                        // A named global callee. A native package reference dispatches
                        // through jrt_native_call; a builtin this backend lowers itself
                        // is left to `resolve_builtin_calls`; any other reserved builtin
                        // declines; otherwise it's a user variable holding a function.
                        if let Some((pkgid, fname)) = parse_native_ref(name) {
                            Some(CallKind::NativeCall { pkgid, fname: fname.to_string(), args: args.clone() })
                        } else {
                            let lowered = LOWERABLE_BUILTINS.contains(&name.as_str())
                                && matches!(name.as_str(), "print" | "str" | "int" | "float" | "bool" | "len")
                                && args.len() == 1;
                            if lowered {
                                None
                            } else if name == "stream" && args.len() == 1 {
                                // Checked before the reserved-builtin decline
                                // below: `stream` is reserved, and this is the
                                // one shape of it the backend can lower.
                                match reg_promptderef.get(&args[0]) {
                                    Some(&(prompt, deref_idx)) => {
                                        skip_getfields.insert(deref_idx);
                                        Some(CallKind::StreamCall { prompt, grammar: None })
                                    }
                                    // `stream(x)` where x is not a fresh `?p`.
                                    // The VM drains whatever TokenStream it is
                                    // handed; AOT has no such value to hold, so
                                    // this declines rather than guessing.
                                    None => return Err(
                                        "lower.rs: stream() requires a prompt dereference (`stream(?p)`)".into()
                                    ),
                                }
                            } else if RESERVED_BUILTINS.contains(&name.as_str()) {
                                return Err(format!("lower.rs: unsupported builtin call `{name}`"));
                            } else if fnctx.struct_field_names.contains_key(name) {
                                // A struct type is not callable — `City { .. }` is
                                // the one way to build one. This still has to be
                                // recognised rather than left to fall through: a
                                // type name is not a known function, so `Indirect`
                                // would load a fn pointer from a global cell codegen
                                // never assigns and jump through it.
                                return Err(format!(
                                    "lower.rs: `{name}` is a struct type, not a function — build one with `{name} {{ ... }}`"
                                ));
                            } else {
                                Some(CallKind::Indirect)
                            }
                        }
                    } else {
                        // A runtime function value (parameter / temporary) → indirect.
                        Some(CallKind::Indirect)
                    };
                    if let Some(k) = kind {
                        out.insert(i, k);
                    }
                }
                reg_fn.remove(d);
                reg_global.remove(d);
                reg_getfield.remove(d);
                reg_getfield_module.remove(d);
            }
            // Keyword-argument call. Only a direct call to a known function is
            // lowerable (named args need the callee's parameter names, which a
            // runtime function value doesn't carry) — anything else declines.
            Instr::CallNamed(d, callee, pairs) => {
                if reg_getfield.contains_key(callee) {
                    return Err("lower.rs: keyword method call (GetField result) is unsupported".into());
                }
                if let Some((module, method, gf_idx)) = reg_getfield_module.get(callee).cloned() {
                    // The one supported keyword module call: fs.read(path, trust=<bool>).
                    let resolved = if module == "fs" && method == "read" {
                        let (mut path, mut trust, mut ok) = (None, None, true);
                        for (name, reg) in pairs {
                            match name.as_deref() {
                                None if path.is_none() => path = Some(*reg),
                                Some("trust") => trust = Some(*reg),
                                _ => ok = false,
                            }
                        }
                        match (ok, path, trust) {
                            (true, Some(p), Some(t)) => Some(vec![p, t]),
                            _ => None,
                        }
                    } else {
                        None
                    };
                    match resolved {
                        Some(args) => {
                            out.insert(i, CallKind::ModuleCall { module, method, args });
                            skip_getfields.insert(gf_idx);
                        }
                        None => return Err("lower.rs: unsupported keyword module call".into()),
                    }
                    reg_fn.remove(d);
                    reg_global.remove(d);
                    reg_getfield.remove(d);
                    reg_getfield_module.remove(d);
                    continue;
                }
                if let Some(&uid) = reg_fn.get(callee) {
                    let cf = &fnctx.defs[uid];
                    let p = cf.params.len();
                    let mut arg_slots: Vec<Option<Reg>> = vec![None; p];
                    let mut pos = 0usize;
                    for (name, reg) in pairs {
                        let slot = match name {
                            None => {
                                let s = pos;
                                pos += 1;
                                s
                            }
                            Some(n) => cf
                                .params
                                .iter()
                                .position(|param| param == n)
                                .ok_or_else(|| format!("lower.rs: no parameter `{n}`"))?,
                        };
                        if slot >= p || arg_slots[slot].is_some() {
                            return Err("lower.rs: bad keyword-argument call".into());
                        }
                        arg_slots[slot] = Some(*reg);
                    }
                    for i in 0..p {
                        if arg_slots[i].is_none()
                            && cf.defaults.get(i).and_then(|x| x.as_ref()).is_none()
                        {
                            return Err("lower.rs: keyword call omits a required argument".into());
                        }
                    }
                    out.insert(i, CallKind::DirectNamed { uid, arg_slots });
                } else if let Some(name) = reg_global.get(callee) {
                    if name == "stream" {
                        let (mut prompt_reg, mut mute_reg, mut ok) = (None, None, true);
                        for (n, reg) in pairs {
                            match n.as_deref() {
                                None if prompt_reg.is_none() => prompt_reg = Some(*reg),
                                Some("mute_on") => mute_reg = Some(*reg),
                                _ => ok = false,
                            }
                        }
                        let deref = prompt_reg.and_then(|r| reg_promptderef.get(&r).copied());
                        let (Some((prompt, gf_idx)), true) = (deref, ok) else {
                            return Err(
                                "lower.rs: stream() requires a prompt dereference and an optional mute_on=".into()
                            );
                        };
                        // `mute_on` is a list, but the streaming entry takes one
                        // anchor and one stop. A single grammar maps exactly; more
                        // than one would need mute regions the C side cannot
                        // express, so decline rather than silently honour the
                        // first and drop the rest.
                        let grammar = match mute_reg {
                            None => None,
                            Some(r) => match reg_array_lit.get(&r).map(|v| v.as_slice()) {
                                Some([]) => None,
                                Some([g]) => Some(*g),
                                Some(_) => return Err(
                                    "lower.rs: stream() mute_on= supports one grammar".into()
                                ),
                                None => return Err(
                                    "lower.rs: stream() mute_on= must be a list literal".into()
                                ),
                            },
                        };
                        skip_getfields.insert(gf_idx);
                        out.insert(i, CallKind::StreamCall { prompt, grammar });
                        reg_fn.remove(d);
                        reg_global.remove(d);
                        reg_getfield.remove(d);
                        reg_getfield_module.remove(d);
                        continue;
                    }
                    if RESERVED_BUILTINS.contains(&name.as_str()) {
                        return Err(format!("lower.rs: unsupported builtin kwarg call `{name}`"));
                    }
                    return Err("lower.rs: indirect keyword-argument call".into());
                } else {
                    return Err("lower.rs: indirect keyword-argument call".into());
                }
                reg_fn.remove(d);
                reg_global.remove(d);
                reg_getfield.remove(d);
                reg_getfield_module.remove(d);
            }
            other => {
                if let Some(d) = dest_reg(other) {
                    reg_fn.remove(&d);
                    reg_global.remove(&d);
                    reg_getfield.remove(&d);
                }
            }
        }
    }
    Ok((out, skip_getfields))
}

/// Emit a runtime-dispatched struct method `recv.method(args)`: look up the
/// implementation by the receiver's runtime type name (`jrt_get_type_name` →
/// `jrt_method_lookup`), then indirect-call it with `self` (the receiver)
/// prepended. Used only for genuinely-ambiguous method names (same name+arity on
/// >1 type); exact-arity, so no default filling.
/// Lower `stream(?p)` / `stream(?p, mute_on=[g])`.
///
/// `prompt` is the prompt register, NOT a dereferenced response: the producing
/// `PromptDeref` is elided during resolution. Inferring at the deref *and* then
/// streaming would run inference twice and print the response twice — the same
/// double-output hazard the non-streaming `?p` lowering guards against, reached
/// from the other side.
///
/// Everything else — the GBNF, the mute anchors, the trailing newline — lives
/// behind `jrt_prompt_stream_obj` in the shared runtime, so the streaming and
/// non-streaming paths cannot disagree about what a grammar means.
/// The LLM session variables, which the VM maintains as globals it rewrites
/// after every inference: `__model__` (the model the daemon reported),
/// `__tokens__` (cumulative tokens billed this run) and `__max_retries__` (the
/// typed-deref retry budget).
///
/// A compiled program has no such writer, so these read back as nil — the same
/// silent-wrong-answer shape as the old `City(d)` miscompile. Each becomes a
/// runtime query against the same state the inference entries maintain.
///
/// Returns `None` for any other name, so ordinary globals are untouched.
/// Materialize a struct field's declared default as a tagged word in the
/// startup prologue, where there is no `Lowerer` — only a context, module and
/// builder.
///
/// Ints, bools and nil are pure constants. A float or string default has to be
/// heap-allocated, so it is built by a runtime call emitted into the prologue;
/// that runs once before user code, alongside the rest of the registration.
fn default_word_const<'ctx>(
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
                .build_call(dup, &[g.into(), context.i8_type().const_int(TRUSTED, false).into()], "dsw")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_pointer_value();
            let iv = b.build_ptr_to_int(raw, i64_ty, "dsi").map_err(|e| e.to_string())?;
            b.build_or(iv, i64_ty.const_int(TAG_STR, false), "dstag")
                .map_err(|e| e.to_string())?
        }
        other => return Err(format!("lower.rs: unsupported struct field default {other:?}")),
    })
}

fn emit_stream_call<'ctx>(
    low: &Lowerer<'_, 'ctx>,
    dest: Reg,
    prompt: Reg,
    grammar: Option<Reg>,
) -> Result<(), String> {
    let b = low.builder;
    let ptrt = low.ptrt();
    let model_fn = low.runtime_fn("jrt_get_model", ptrt.fn_type(&[], false));
    let model = b
        .build_call(model_fn, &[], "model")
        .map_err(|e| e.to_string())?
        .as_any_value_enum()
        .into_pointer_value();
    let prompt_ptr = low.untag_ptr(low.load(prompt));
    let gobj = match grammar {
        Some(g) => low.untag_ptr(low.load(g)),
        None => ptrt.const_null(),
    };
    let f = low.runtime_fn(
        "jrt_prompt_stream_obj",
        ptrt.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into()], false),
    );
    let raw = b
        .build_call(f, &[prompt_ptr.into(), model.into(), gobj.into()], "streamed")
        .map_err(|e| e.to_string())?
        .as_any_value_enum()
        .into_pointer_value();
    low.store(dest, low.tag_str(raw));
    Ok(())
}

fn emit_dynamic_method<'ctx>(
    low: &Lowerer<'_, 'ctx>,
    recv: Reg,
    method: &str,
    args: &[Reg],
) -> Result<IntValue<'ctx>, String> {
    let b = low.builder;
    let i64_ty = low.i64t();
    let ptrt = low.ptrt();
    let err = |e: inkwell::builder::BuilderError| e.to_string();

    // type_word = tag_str(jrt_get_type_name(recv))
    let gtn = low.runtime_fn("jrt_get_type_name", ptrt.fn_type(&[i64_ty.into()], false));
    let tn = b
        .build_call(gtn, &[low.load(recv).into()], "tname")
        .map_err(err)?
        .as_any_value_enum()
        .into_pointer_value();
    let type_word = low.tag_str(tn);
    // fnptr = jrt_method_lookup(type_word, "method")
    let lookup = low.runtime_fn("jrt_method_lookup", ptrt.fn_type(&[i64_ty.into(), ptrt.into()], false));
    let fnptr = b
        .build_call(lookup, &[type_word.into(), low.cstr(method).into()], "mfn")
        .map_err(err)?
        .as_any_value_enum()
        .into_pointer_value();
    // Indirect call: fnptr(self, args...) — one i64 param per (self + args).
    let arity = args.len() + 1;
    let arg_tys = vec![i64_ty.into(); arity];
    let fn_ty = i64_ty.fn_type(&arg_tys, false);
    let mut argv: Vec<BasicMetadataValueEnum> = Vec::with_capacity(arity);
    argv.push(low.load(recv).into());
    for a in args {
        argv.push(low.load(*a).into());
    }
    Ok(b
        .build_indirect_call(fn_ty, fnptr, &argv, "dmcall")
        .map_err(err)?
        .as_any_value_enum()
        .into_int_value())
}

/// Load a native package's `dlopen` handle from its `native_pkg$<pkgid>` global.
/// `compile()` creates + fills this in `main`'s prologue for the real module; it
/// is created lazily (nil) if missing so the throwaway probe module also lowers.
fn native_pkg_handle<'ctx>(low: &Lowerer<'_, 'ctx>, pkgid: u32) -> Result<PointerValue<'ctx>, String> {
    let gname = format!("native_pkg${pkgid}");
    let g = low.module.get_global(&gname).unwrap_or_else(|| {
        let g = low.module.add_global(low.ptrt(), None, &gname);
        g.set_initializer(&low.ptrt().const_null());
        g.set_linkage(inkwell::module::Linkage::Internal);
        g
    });
    low.builder
        .build_load(low.ptrt(), g.as_pointer_value(), "nhandle")
        .map_err(|e| e.to_string())
        .map(|v| v.into_pointer_value())
}

/// Emit a direct native (C-ABI) call `__native$<pkgid>$<fname>(args)`: marshal the
/// (already-tagged) args into a stack array and dispatch through `jrt_native_call`.
/// The result is a tagged word (native output strings are TAINTED); it is used
/// directly (no reinterpret — the Chunk path is uniformly tagged). Can raise.
fn emit_native_call<'ctx>(
    low: &Lowerer<'_, 'ctx>,
    pkgid: u32,
    fname: &str,
    args: &[Reg],
) -> Result<IntValue<'ctx>, String> {
    let b = low.builder;
    let i64_ty = low.i64t();
    let ptrt = low.ptrt();
    let err = |e: inkwell::builder::BuilderError| e.to_string();

    let handle = native_pkg_handle(low, pkgid)?;
    let argv = if args.is_empty() {
        ptrt.const_null()
    } else {
        let arr = b
            .build_array_alloca(i64_ty, i64_ty.const_int(args.len() as u64, false), "nargv")
            .map_err(err)?;
        for (i, r) in args.iter().enumerate() {
            let slot = unsafe {
                b.build_in_bounds_gep(i64_ty, arr, &[i64_ty.const_int(i as u64, false)], "na")
                    .map_err(err)?
            };
            b.build_store(slot, low.load(*r)).map_err(err)?;
        }
        arr
    };
    let name_ptr = b.build_global_string_ptr(fname, "nfname").map_err(err)?.as_pointer_value();
    let call_fn = low.runtime_fn(
        "jrt_native_call",
        i64_ty.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into(), i64_ty.into()], false),
    );
    Ok(b
        .build_call(
            call_fn,
            &[handle.into(), name_ptr.into(), argv.into(), i64_ty.const_int(args.len() as u64, false).into()],
            "ncall",
        )
        .map_err(err)?
        .as_any_value_enum()
        .into_int_value())
}

/// Materialize a first-class native function value: a heap
/// `{ fn_ptr@0, kind@8, env@16 }` where `fn_ptr` is the `jrt_native_call`
/// address used purely as a sentinel (a real `jf_<uid>` box never holds this
/// symbol), and `env = { handle, name }`. `indirect_call` recognizes the
/// sentinel and routes through `jrt_native_call`. Returned as a `TAG_PTR` word.
///
/// The `kind` word at offset 8 holds `ObjKind::Fn`, aligned with
/// `ObjHeader.kind`, exactly as `fn_box_word` does for ordinary functions. It is
/// load-bearing, not decorative: without it, offset 8 held the `env` *pointer*,
/// so `jrt_decref` reading an `ObjKind` there would interpret a heap address's
/// low byte as a kind — and a byte that happened to be 2/3/4 would send
/// `free_obj` off to `Box::from_raw` the value as an Array/Dict/Struct. That
/// hazard is the reason native refs used to veto refcounting for the whole
/// program.
///
/// The third slot used to hold `name`, which nothing ever read (`env` already
/// carries it), so the kind word costs no extra space.
fn emit_native_fn_value<'ctx>(
    low: &Lowerer<'_, 'ctx>,
    pkgid: u32,
    fname: &str,
) -> Result<IntValue<'ctx>, String> {
    let b = low.builder;
    let i64_ty = low.i64t();
    let ptrt = low.ptrt();
    let err = |e: inkwell::builder::BuilderError| e.to_string();
    let malloc = low.runtime_fn("malloc", ptrt.fn_type(&[i64_ty.into()], false));
    let alloc = |n: u64, name: &str| -> Result<PointerValue<'ctx>, String> {
        Ok(b.build_call(malloc, &[i64_ty.const_int(n, false).into()], name)
            .map_err(err)?
            .as_any_value_enum()
            .into_pointer_value())
    };
    let store_ptr = |base: PointerValue<'ctx>, idx: u64, val: BasicMetadataValueEnum<'ctx>| -> Result<(), String> {
        let slot = unsafe {
            b.build_in_bounds_gep(ptrt, base, &[i64_ty.const_int(idx, false)], "fslot").map_err(err)?
        };
        let v: inkwell::values::BasicValueEnum = match val {
            BasicMetadataValueEnum::PointerValue(p) => p.into(),
            _ => return Err("native fn value: expected pointer".into()),
        };
        b.build_store(slot, v).map_err(err)?;
        Ok(())
    };

    let handle = native_pkg_handle(low, pkgid)?;
    let name_ptr = b.build_global_string_ptr(fname, "nfname").map_err(err)?.as_pointer_value();
    // env = { handle, name }
    let env = alloc(16, "native_env")?;
    store_ptr(env, 0, handle.into())?;
    store_ptr(env, 1, name_ptr.into())?;
    // fn value = { sentinel, kind, env }
    let sentinel = low
        .runtime_fn(
            "jrt_native_call",
            i64_ty.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into(), i64_ty.into()], false),
        )
        .as_global_value()
        .as_pointer_value();
    let fnval = alloc(24, "native_fn_val")?;
    store_ptr(fnval, 0, sentinel.into())?;
    // kind word at offset 8 — see the doc comment.
    let kind_slot = unsafe {
        b.build_in_bounds_gep(i64_ty, fnval, &[i64_ty.const_int(1, false)], "nkind").map_err(err)?
    };
    b.build_store(kind_slot, i64_ty.const_int(OBJKIND_FN, false)).map_err(err)?;
    store_ptr(fnval, 2, env.into())?;
    Ok(low.tag_ptr(fnval))
}

/// Emit a string primitive method `recv.method(args)` via the shared `jrt_str_*`
/// symbol (the receiver and any args are strings; results are tagged strings or
/// bool words). Only methods `chunk_str_method_supported` accepts reach here.
fn emit_str_method<'ctx>(
    low: &Lowerer<'_, 'ctx>,
    recv: Reg,
    method: &str,
    args: &[Reg],
) -> Result<IntValue<'ctx>, String> {
    let b = low.builder;
    let ptrt = low.ptrt();
    let i32_ty = low.ctx.i32_type();
    let err = |e: inkwell::builder::BuilderError| e.to_string();
    let sp = |r: Reg| low.untag_ptr(low.load(r));

    match method {
        "trim" | "upper" | "lower" => {
            let f = low.runtime_fn(&format!("jrt_str_{method}"), ptrt.fn_type(&[ptrt.into()], false));
            let r = b
                .build_call(f, &[sp(recv).into()], "strm")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_str(r))
        }
        "replace" => {
            let f = low.runtime_fn(
                "jrt_str_replace",
                ptrt.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into()], false),
            );
            let r = b
                .build_call(f, &[sp(recv).into(), sp(args[0]).into(), sp(args[1]).into()], "strm")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_str(r))
        }
        "starts_with" | "ends_with" => {
            let f = low.runtime_fn(
                &format!("jrt_str_{method}"),
                i32_ty.fn_type(&[ptrt.into(), ptrt.into()], false),
            );
            let r = b
                .build_call(f, &[sp(recv).into(), sp(args[0]).into()], "strm")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value();
            let bit = b
                .build_int_compare(inkwell::IntPredicate::NE, r, i32_ty.const_zero(), "b")
                .map_err(err)?;
            Ok(low.bool_word(bit))
        }
        "split" => {
            // (s, sep) -> new array of substrings (tagged ptr).
            let f = low.runtime_fn("jrt_coll_str_split", ptrt.fn_type(&[ptrt.into(), ptrt.into()], false));
            let p = b
                .build_call(f, &[sp(recv).into(), sp(args[0]).into()], "split")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_ptr(p))
        }
        _ => Err(format!("lower.rs: emit_str_method: unhandled {method}")),
    }
}

/// Emit an array/dict primitive method `recv.method(args)` via the ObjHeader-aware
/// `jrt_karr_*`/`jrt_coll_*` helpers. The receiver kind is implied by the method
/// name (`chunk_val_method_supported`). Array push/pop/sort/reverse mutate in
/// place (push/sort/reverse → nil, pop → element); dict keys/values build a new
/// array, has → bool, get → value-or-nil.
fn emit_val_method<'ctx>(
    low: &Lowerer<'_, 'ctx>,
    recv: Reg,
    method: &str,
    args: &[Reg],
) -> Result<IntValue<'ctx>, String> {
    let b = low.builder;
    let i64_ty = low.i64t();
    let ptrt = low.ptrt();
    let i32_ty = low.ctx.i32_type();
    let void_ty = low.ctx.void_type();
    let err = |e: inkwell::builder::BuilderError| e.to_string();
    let recv_p = low.untag_ptr(low.load(recv));
    let nil = i64_ty.const_int(NIL, false);

    match method {
        "push" => {
            let f = low.runtime_fn("jrt_karr_push", void_ty.fn_type(&[ptrt.into(), i64_ty.into()], false));
            b.build_call(f, &[recv_p.into(), low.load(args[0]).into()], "").map_err(err)?;
            Ok(nil)
        }
        "pop" => {
            let f = low.runtime_fn("jrt_coll_array_pop", i64_ty.fn_type(&[ptrt.into()], false));
            Ok(b.build_call(f, &[recv_p.into()], "pop").map_err(err)?.as_any_value_enum().into_int_value())
        }
        "sort" | "reverse" => {
            let cname = if method == "sort" { "jrt_coll_array_sort" } else { "jrt_coll_array_reverse" };
            let f = low.runtime_fn(cname, void_ty.fn_type(&[ptrt.into()], false));
            b.build_call(f, &[recv_p.into()], "").map_err(err)?;
            Ok(nil)
        }
        "keys" | "values" => {
            let cname = if method == "keys" { "jrt_coll_dict_keys" } else { "jrt_coll_dict_values" };
            let f = low.runtime_fn(cname, ptrt.fn_type(&[ptrt.into()], false));
            let p = b.build_call(f, &[recv_p.into()], "kv").map_err(err)?.as_any_value_enum().into_pointer_value();
            Ok(low.tag_ptr(p))
        }
        "len" => {
            // `recv.len()` == `len(recv)`: jrt_len_chunk tag-dispatches str
            // (byte length) / collection (ObjHeader.len) at runtime → tagged int.
            let f = low.runtime_fn("jrt_len_chunk", i64_ty.fn_type(&[i64_ty.into()], false));
            let n = b
                .build_call(f, &[low.load(recv).into()], "len")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value();
            Ok(low.tag_int(n))
        }
        "contains" => {
            // `haystack.contains(needle)` == `needle in haystack`: jrt_in_any
            // dispatches str (substring) / array (element eq) at runtime.
            let f = low.runtime_fn("jrt_in_any", i32_ty.fn_type(&[i64_ty.into(), i64_ty.into()], false));
            let r = b
                .build_call(f, &[low.load(args[0]).into(), low.load(recv).into()], "cont")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value();
            let bit = b
                .build_int_compare(inkwell::IntPredicate::NE, r, i32_ty.const_zero(), "c")
                .map_err(err)?;
            Ok(low.bool_word(bit))
        }
        "has" | "get" => {
            // jrt_coll_dict_get(dict, key_cstr, *out) -> found?  (key arg is a
            // tagged string → its data pointer).
            let f = low.runtime_fn(
                "jrt_coll_dict_get",
                i32_ty.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into()], false),
            );
            let out = b.build_alloca(i64_ty, "dget").map_err(err)?;
            let key_p = low.untag_ptr(low.load(args[0]));
            let found = b
                .build_call(f, &[recv_p.into(), key_p.into(), out.into()], "has")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value();
            let bit = b
                .build_int_compare(inkwell::IntPredicate::NE, found, i32_ty.const_zero(), "f")
                .map_err(err)?;
            if method == "has" {
                Ok(low.bool_word(bit))
            } else {
                // get: found ? *out : nil.
                let val = b.build_load(i64_ty, out, "getval").map_err(err)?.into_int_value();
                Ok(b.build_select(bit, val, nil, "getornil").map_err(err)?.into_int_value())
            }
        }
        _ => Err(format!("lower.rs: emit_val_method: unhandled {method}")),
    }
}

/// Emit a layout-safe stdlib `module.method(args)` call, returning the tagged
/// result word. Only methods `chunk_module_supported` accepts reach here; each
/// reuses the same runtime `jrt_*` symbol the legacy path calls. String returns
/// are null-checked → tagged nil (covers `path.ext`/`env.get`); scalar returns
/// tag directly. Collection-producing/consuming methods are excluded (they use
/// the legacy `JrtArrayHdr`/`JadeDict` layout, not the Chunk path's ObjHeader).
fn emit_module_call<'ctx>(
    low: &Lowerer<'_, 'ctx>,
    module: &str,
    method: &str,
    args: &[Reg],
) -> Result<IntValue<'ctx>, String> {
    let b = low.builder;
    let i64_ty = low.i64t();
    let ptrt = low.ptrt();
    let i32_ty = low.ctx.i32_type();
    let void_ty = low.ctx.void_type();
    let f64_ty = low.f64t();
    let err = |e: inkwell::builder::BuilderError| e.to_string();

    // Untag arg `k` as a data pointer (string/collection char*/void*).
    let strp = |k: usize| low.untag_ptr(low.load(args[k]));
    let nil = i64_ty.const_int(NIL, false);

    // A returned char* → tagged str word, or nil when NULL.
    let tag_str_or_nil = |p: PointerValue<'ctx>| -> Result<IntValue<'ctx>, String> {
        let asint = b.build_ptr_to_int(p, i64_ty, "sp2i").map_err(err)?;
        let is_null = b
            .build_int_compare(inkwell::IntPredicate::EQ, asint, i64_ty.const_zero(), "isnull")
            .map_err(err)?;
        let tagged = b.build_or(asint, i64_ty.const_int(TAG_STR, false), "tagstr").map_err(err)?;
        Ok(b.build_select(is_null, nil, tagged, "strornil").map_err(err)?.into_int_value())
    };

    // Call a `(ptr, ptr, …) -> ptr` string function over `strp(0..n)`.
    let str_fn = |name: &str, n: usize| -> Result<PointerValue<'ctx>, String> {
        let params: Vec<inkwell::types::BasicMetadataTypeEnum> = vec![ptrt.into(); n];
        let f = low.runtime_fn(name, ptrt.fn_type(&params, false));
        let argv: Vec<BasicMetadataValueEnum> = (0..n).map(|k| strp(k).into()).collect();
        Ok(b.build_call(f, &argv, "modcall").map_err(err)?.as_any_value_enum().into_pointer_value())
    };
    // A `(ptr, …) -> void` sink → nil word.
    let void_ptr_fn = |name: &str, n: usize| -> Result<IntValue<'ctx>, String> {
        let params: Vec<inkwell::types::BasicMetadataTypeEnum> = vec![ptrt.into(); n];
        let f = low.runtime_fn(name, void_ty.fn_type(&params, false));
        let argv: Vec<BasicMetadataValueEnum> = (0..n).map(|k| strp(k).into()).collect();
        b.build_call(f, &argv, "").map_err(err)?;
        Ok(nil)
    };
    // A `(ptr) -> i32` predicate → bool word.
    let bool_ptr_fn = |name: &str| -> Result<IntValue<'ctx>, String> {
        let f = low.runtime_fn(name, i32_ty.fn_type(&[ptrt.into()], false));
        let r = b.build_call(f, &[strp(0).into()], "").map_err(err)?.as_any_value_enum().into_int_value();
        let bit = b
            .build_int_compare(inkwell::IntPredicate::NE, r, i32_ty.const_zero(), "b")
            .map_err(err)?;
        Ok(low.bool_word(bit))
    };
    // A `() / (ptr) -> i64` scalar → tagged int.
    let int_fn = |name: &str, n_ptr: usize| -> Result<IntValue<'ctx>, String> {
        let params: Vec<inkwell::types::BasicMetadataTypeEnum> = vec![ptrt.into(); n_ptr];
        let f = low.runtime_fn(name, i64_ty.fn_type(&params, false));
        let argv: Vec<BasicMetadataValueEnum> = (0..n_ptr).map(|k| strp(k).into()).collect();
        let r = b.build_call(f, &argv, "").map_err(err)?.as_any_value_enum().into_int_value();
        Ok(low.tag_int(r))
    };

    // math.* take/return tagged value words directly (int-or-float dispatch is in
    // the runtime helper), so no arg/return coercion is needed.
    let math_fn = |name: &str, n: usize| -> Result<IntValue<'ctx>, String> {
        let params: Vec<inkwell::types::BasicMetadataTypeEnum> = vec![i64_ty.into(); n];
        let f = low.runtime_fn(name, i64_ty.fn_type(&params, false));
        let argv: Vec<BasicMetadataValueEnum> = (0..n).map(|k| low.load(args[k]).into()).collect();
        Ok(b.build_call(f, &argv, "math").map_err(err)?.as_any_value_enum().into_int_value())
    };

    match (module, method) {
        // abs/pow are overflow-checked, so they go through C forwarders that
        // raise "integer overflow"; the rest cannot fail and call Rust directly.
        ("math", "abs") => math_fn("jade_math_abs", 1),
        ("math", "pow") => math_fn("jade_math_pow", 2),
        ("math", "floor" | "ceil" | "sqrt") => math_fn(&format!("jrt_math_{method}"), 1),
        ("math", "min" | "max") => math_fn(&format!("jrt_math_{method}"), 2),
        ("path", "basename" | "ext" | "dirname" | "stem" | "abs") => {
            tag_str_or_nil(str_fn(&format!("jrt_path_{method}"), 1)?)
        }
        ("path", "is_abs") => bool_ptr_fn("jrt_path_is_abs"),
        ("path", "join") => {
            // Variadic left-fold through the 2-arg jrt_path_join primitive.
            let f = low.runtime_fn("jrt_path_join", ptrt.fn_type(&[ptrt.into(), ptrt.into()], false));
            let mut acc = strp(0);
            for k in 1..args.len() {
                acc = b
                    .build_call(f, &[acc.into(), strp(k).into()], "join")
                    .map_err(err)?
                    .as_any_value_enum()
                    .into_pointer_value();
            }
            Ok(low.tag_str(acc))
        }
        ("fs", "read") => {
            // jrt_fs_read(path, i32 trust) -> str (TAINTED unless trust, raises on
            // error). `fs.read(path, trust=<bool>)` passes the bool's bit4 as trust.
            let f = low.runtime_fn("jrt_fs_read", ptrt.fn_type(&[ptrt.into(), i32_ty.into()], false));
            let trust = if args.len() == 2 {
                let w = low.load(args[1]);
                let sh = b.build_right_shift(w, i64_ty.const_int(4, false), false, "tsh").map_err(err)?;
                let bit = b.build_and(sh, i64_ty.const_int(1, false), "tbit").map_err(err)?;
                b.build_int_truncate(bit, i32_ty, "t32").map_err(err)?
            } else {
                i32_ty.const_zero()
            };
            let r = b
                .build_call(f, &[strp(0).into(), trust.into()], "read")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_str(r))
        }
        ("fs", "exists") => bool_ptr_fn("jrt_fs_exists"),
        ("fs", "write") => void_ptr_fn("jrt_fs_write", 2),
        ("fs", "append") => void_ptr_fn("jrt_fs_append", 2),
        ("fs", "delete") => void_ptr_fn("jrt_fs_delete", 1),
        ("fs", "mkdir") => void_ptr_fn("jrt_fs_mkdir", 1),
        ("sh", "exec") => Ok(low.tag_str(str_fn("jrt_sh_exec", 1)?)),
        ("sh", "run") => int_fn("jrt_sh_run", 1),
        ("sh", "output") => {
            let f = low.runtime_fn("jrt_coll_sh_output", ptrt.fn_type(&[ptrt.into()], false));
            let p = b.build_call(f, &[strp(0).into()], "shout").map_err(err)?.as_any_value_enum().into_pointer_value();
            Ok(low.tag_ptr(p))
        }
        ("fs", "list_dir") => {
            // raises on I/O error; returns an already-tagged array pointer word.
            let f = low.runtime_fn("jrt_fs_list_dir_chunk", i64_ty.fn_type(&[ptrt.into()], false));
            Ok(b.build_call(f, &[strp(0).into()], "ld").map_err(err)?.as_any_value_enum().into_int_value())
        }
        ("random", "choice") => {
            // (arr word) -> element word (already tagged).
            let f = low.runtime_fn("jrt_random_choice_chunk", i64_ty.fn_type(&[i64_ty.into()], false));
            Ok(b.build_call(f, &[low.load(args[0]).into()], "choice").map_err(err)?.as_any_value_enum().into_int_value())
        }
        ("random", "shuffle") => {
            let f = low.runtime_fn("jrt_random_shuffle_chunk", void_ty.fn_type(&[i64_ty.into()], false));
            b.build_call(f, &[low.load(args[0]).into()], "").map_err(err)?;
            Ok(nil)
        }
        ("env", "cwd") => Ok(low.tag_str(str_fn("jrt_env_cwd", 0)?)),
        ("env", "get") => tag_str_or_nil(str_fn("jrt_env_get", 1)?),
        ("env", "set") => void_ptr_fn("jrt_env_set", 2),
        ("time", "now") => int_fn("jrt_time_now", 0),
        ("time", "now_ms") => int_fn("jrt_time_now_ms", 0),
        ("time", "sleep") => {
            // (float seconds) -> nil. Unbox the boxed-float arg to a native f64.
            let unbox = low.runtime_fn("jrt_unbox_float", f64_ty.fn_type(&[i64_ty.into()], false));
            let d = b.build_call(unbox, &[low.load(args[0]).into()], "sec").map_err(err)?.as_any_value_enum().into_float_value();
            let f = low.runtime_fn("jrt_time_sleep", void_ty.fn_type(&[f64_ty.into()], false));
            b.build_call(f, &[d.into()], "").map_err(err)?;
            Ok(nil)
        }
        ("array", "map" | "filter") => {
            // (arr word, fn word) -> new array word. Both args are tagged words.
            let cname = if method == "map" { "jrt_coll_array_map" } else { "jrt_coll_array_filter" };
            let f = low.runtime_fn(cname, i64_ty.fn_type(&[i64_ty.into(), i64_ty.into()], false));
            Ok(b
                .build_call(f, &[low.load(args[0]).into(), low.load(args[1]).into()], "mapf")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value())
        }
        ("random", "int") => {
            // Raw (untagged) i64 bounds; raises if lo > hi.
            let f = low.runtime_fn("jrt_random_int", i64_ty.fn_type(&[i64_ty.into(), i64_ty.into()], false));
            let lo = low.untag_int(low.load(args[0]));
            let hi = low.untag_int(low.load(args[1]));
            let r = b.build_call(f, &[lo.into(), hi.into()], "").map_err(err)?.as_any_value_enum().into_int_value();
            Ok(low.tag_int(r))
        }
        ("random", "seed") => {
            let f = low.runtime_fn("jrt_random_seed", void_ty.fn_type(&[i64_ty.into()], false));
            b.build_call(f, &[low.untag_int(low.load(args[0])).into()], "").map_err(err)?;
            Ok(nil)
        }
        ("random", "float") => {
            let f = low.runtime_fn("jrt_random_float", f64_ty.fn_type(&[], false));
            let d = b.build_call(f, &[], "").map_err(err)?.as_any_value_enum().into_float_value();
            let boxf = low.runtime_fn("jrt_box_float", i64_ty.fn_type(&[f64_ty.into()], false));
            Ok(b.build_call(boxf, &[d.into()], "boxf").map_err(err)?.as_any_value_enum().into_int_value())
        }
        ("llm", "count_tokens") => int_fn("jrt_count_tokens", 1),
        ("llm", "total_tokens") => int_fn("jrt_total_tokens", 0),
        ("llm", "model") => Ok(low.tag_str(str_fn("jrt_get_model", 0)?)),
        ("llm", "keep_anchors") => {
            // (bool word) -> void. Extract bit4 (the bool payload) as an i32.
            let w = low.load(args[0]);
            let sh = b.build_right_shift(w, i64_ty.const_int(4, false), false, "ksh").map_err(err)?;
            let bit = b.build_and(sh, i64_ty.const_int(1, false), "kbit").map_err(err)?;
            let on = b.build_int_truncate(bit, i32_ty, "k32").map_err(err)?;
            let f = low.runtime_fn("jrt_llm_keep_anchors", void_ty.fn_type(&[i32_ty.into()], false));
            b.build_call(f, &[on.into()], "").map_err(err)?;
            Ok(nil)
        }
        ("llm", "set_max_tokens") => {
            // (int word) -> void. Untag to a raw i64.
            let n = low.untag_int(low.load(args[0]));
            let f = low.runtime_fn("jrt_llm_set_max_tokens", void_ty.fn_type(&[i64_ty.into()], false));
            b.build_call(f, &[n.into()], "").map_err(err)?;
            Ok(nil)
        }
        // These return already-tagged ObjHeader value words (dict/array or nil).
        ("llm", "health") => {
            let f = low.runtime_fn("jrt_llm_health", i64_ty.fn_type(&[], false));
            Ok(b.build_call(f, &[], "health").map_err(err)?.as_any_value_enum().into_int_value())
        }
        ("env", "args") => {
            // () -> already-tagged array word (TRUSTED strings).
            let f = low.runtime_fn("jrt_env_args", i64_ty.fn_type(&[], false));
            Ok(b.build_call(f, &[], "args").map_err(err)?.as_any_value_enum().into_int_value())
        }
        ("time", "local") => Ok(low.tag_str(str_fn("jrt_time_local", 1)?)),
        ("http", "get" | "delete" | "head") => {
            // (url, [headers]) -> already-tagged { status, body } dict word.
            let f = low.runtime_fn(&format!("jrt_http_{method}"), i64_ty.fn_type(&[ptrt.into(), ptrt.into()], false));
            let headers = if args.len() >= 2 { strp(1) } else { ptrt.const_null() };
            Ok(b.build_call(f, &[strp(0).into(), headers.into()], "http").map_err(err)?.as_any_value_enum().into_int_value())
        }
        ("http", "post" | "put") => {
            // (url, body, [headers]) -> tagged { status, body } dict word.
            let f = low.runtime_fn(&format!("jrt_http_{method}"), i64_ty.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into()], false));
            let headers = if args.len() >= 3 { strp(2) } else { ptrt.const_null() };
            Ok(b.build_call(f, &[strp(0).into(), strp(1).into(), headers.into()], "http").map_err(err)?.as_any_value_enum().into_int_value())
        }
        ("dict", "merge") => {
            // (d1, d2) -> new dict word (tagged ptr).
            let f = low.runtime_fn("jrt_coll_dict_merge", ptrt.fn_type(&[ptrt.into(), ptrt.into()], false));
            let p = b
                .build_call(f, &[strp(0).into(), strp(1).into()], "merge")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_ptr(p))
        }
        ("json", "parse") => {
            // (str) -> value word (already tagged: dict/array/scalar).
            let f = low.runtime_fn("jrt_json_parse_chunk", i64_ty.fn_type(&[ptrt.into()], false));
            Ok(b.build_call(f, &[strp(0).into()], "jparse").map_err(err)?.as_any_value_enum().into_int_value())
        }
        ("json", "stringify" | "stringify_pretty") => {
            // (value word, i32 pretty) -> tagged string.
            let f = low.runtime_fn("jrt_json_stringify_chunk", ptrt.fn_type(&[i64_ty.into(), i32_ty.into()], false));
            let pretty = i32_ty.const_int(u64::from(method == "stringify_pretty"), false);
            let p = b
                .build_call(f, &[low.load(args[0]).into(), pretty.into()], "jstr")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_str(p))
        }
        ("Grammar", "new") => {
            // Grammar.new(pattern[, anchor[, stop]]) -> a Grammar object word.
            // Omitted optional args pass NULL (→ None in the runtime).
            let f = low.runtime_fn(
                "jrt_grammar_new",
                ptrt.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into()], false),
            );
            let anchor = if args.len() >= 2 { strp(1) } else { ptrt.const_null() };
            let stop = if args.len() >= 3 { strp(2) } else { ptrt.const_null() };
            let g = b
                .build_call(f, &[strp(0).into(), anchor.into(), stop.into()], "grammar")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_ptr(g))
        }
        _ => Err(format!("lower.rs: emit_module_call: unhandled {module}.{method}")),
    }
}

/// A struct field default the backend can materialize (scalar literals only;
/// mirrors the VM's `eval_literal_default` for the representable subset).
fn eval_scalar_default(e: &Expr) -> Option<VmValue> {
    match e {
        Expr::Str { value, .. } => Some(VmValue::Str(value.clone().into())),
        Expr::Integer { value, .. } => Some(VmValue::Int(*value)),
        Expr::Float { value, .. } => Some(VmValue::Float(*value)),
        Expr::Bool { value, .. } => Some(VmValue::Bool(*value)),
        Expr::Identifier { name, .. } if name == "nil" || name == "None" || name == "null" => {
            Some(VmValue::Nil)
        }
        _ => None,
    }
}

/// Struct type name → its optional fields' (name, scalar default). Non-scalar or
/// non-literal defaults (e.g. `let xs = []`) are skipped — a literal that omits
/// such a field would leave it unset, but those appear on method-bearing structs
/// (declined). Mirrors what the VM fills at `MakeStruct` time.
fn build_struct_defaults(
    struct_defs: &HashMap<String, Vec<StructFieldDef>>,
) -> HashMap<String, Vec<(String, VmValue)>> {
    let mut out = HashMap::new();
    for (tn, fields) in struct_defs {
        let mut ds = Vec::new();
        for f in fields {
            if let StructFieldDef::Let { name, default } = f {
                if let Some(v) = eval_scalar_default(default) {
                    ds.push((name.clone(), v));
                }
            }
        }
        if !ds.is_empty() {
            out.insert(tn.clone(), ds);
        }
    }
    out
}

/// Lower a whole program: every reachable `fn_def` becomes a `jf_<uid>` function
/// and the top-level chunk becomes `jade_toplevel() -> i64`. Returns the
/// top-level function (for `main` to call), or `Err` on any opcode/construct the
/// backend can't lower yet (the daemon then falls back to `expr.rs`).

/// Whether the whole program is **refcount-safe**: every heap value it can put
/// in a `TAG_PTR` word is header-carrying, so `jrt_incref`/`jrt_decref` (and the
/// destructor's child cascade) can dispatch on the `ObjKind` byte at offset 8
/// and never touch a header-less allocation.
///
/// This is now unconditionally true, and the scan is gone. Every `TAG_PTR`
/// producer accounts for offset 8:
///
///  * collections (Array/Dict/Struct) and futures carry a real `ObjHeader`;
///  * grammar objects carry `ObjKind::Grammar` (`grammarf.rs`);
///  * ordinary fn boxes (`fn_box_word`) and native fn values
///    (`emit_native_fn_value`) carry `ObjKind::Fn` there, so the refcount ops
///    recognise them and no-op;
///  * a prompt is not a heap kind at all — `MakePrompt` stores the underlying
///    string, and a `TAG_STR` word is rejected by tag before any header is read.
///
/// It is kept as a named predicate rather than inlined as `true` so that the
/// invariant has somewhere to live: **anything that introduces a new `TAG_PTR`
/// value must put an `ObjKind` at offset 8, or re-introduce a veto here.** The
/// last holdout was the native fn value, whose offset 8 held the `env` pointer —
/// a heap address whose low byte, if it happened to be 2/3/4, would have sent
/// `free_obj` off to reclaim it as an Array/Dict/Struct.
fn program_collections_only(_top: &Chunk, _defs: &[Arc<CompiledFn>]) -> bool {
    true
}

/// A lowered program: its top-level entry plus the named functions it defines.
pub struct LoweredProgram<'ctx> {
    /// `jade_toplevel() -> i64`, which `main` (or `jade_pkg_init`) calls.
    pub toplevel: FunctionValue<'ctx>,
    /// Global name → the `jf_<uid>` it holds. `jade build --lib` needs this to
    /// export a function under the name the Jade source gave it; nothing else
    /// recovers a source-level name once lowering has mangled everything to uids.
    pub global_fns: HashMap<String, FunctionValue<'ctx>>,
}

pub fn lower_program<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    top: &Chunk,
    top_n_slots: u32,
    struct_defs: &HashMap<String, Vec<StructFieldDef>>,
    extend_methods: &HashMap<String, HashMap<String, Arc<CompiledFn>>>,
) -> Result<LoweredProgram<'ctx>, String> {
    let (mut defs, mut ptr2uid) = collect_fns(top);

    // Extend-block methods are ordinary compiled functions (`self` is param 0),
    // but they live in the `extend_methods` side table, not in `top`'s reachable
    // tree — so append them as functions with their own uids (BFS their nested
    // fn_defs too), and record which method NAMES are unique across all types.
    // A unique name devirtualizes to a direct call; an ambiguous name (same
    // method on >1 type) needs runtime type dispatch and is left out, so calls to
    // it decline (`resolve_user_calls`).
    let method_candidates = collect_method_fns(extend_methods, &mut defs, &mut ptr2uid);

    // Native (C-ABI FFI) references — `__native$<pkgid>$<fn>` globals produced by
    // import namespacing — are lowered directly: a `Call` on one dispatches through
    // `jrt_native_call` (against the package `dlopen`'d in main's prologue), and a
    // native ref used as a value materializes a `jade_fn_t` sentinel value that the
    // indirect-call path recognizes. See `parse_native_ref`, `emit_native_call`,
    // and `emit_native_fn_value` / `indirect_call`.

    let global_fns = build_global_fns(top, &defs, &ptr2uid);
    let i64_ty = context.i64_type();

    // Forward-declare every function first, so bodies can call each other.
    let mut funcs = Vec::with_capacity(defs.len());
    for (uid, cf) in defs.iter().enumerate() {
        let ptys: Vec<inkwell::types::BasicMetadataTypeEnum> =
            vec![i64_ty.into(); cf.params.len()];
        funcs.push(module.add_function(&format!("jf_{uid}"), i64_ty.fn_type(&ptys, false), None));
    }
    // Async task-entry wrappers: `jf_task_<uid>(i64* args, i32 n) -> i64` unpacks
    // the argument array and calls `jf_<uid>` — the `jade_task_fn` ABI expected by
    // `jade_spawn`. Emitted for every function (unused ones are DCE'd); each only
    // references the already-declared `jf_<uid>`.
    {
        let ptr_ty = context.ptr_type(AddressSpace::default());
        let i32_ty = context.i32_type();
        let builder = context.create_builder();
        for (uid, cf) in defs.iter().enumerate() {
            let wrapper = module.add_function(
                &format!("jf_task_{uid}"),
                i64_ty.fn_type(&[ptr_ty.into(), i32_ty.into()], false),
                Some(inkwell::module::Linkage::Internal),
            );
            let entry = context.append_basic_block(wrapper, "entry");
            builder.position_at_end(entry);
            let args_ptr = wrapper.get_nth_param(0).unwrap().into_pointer_value();
            let n = cf.params.len();
            let mut argv: Vec<BasicMetadataValueEnum> = Vec::with_capacity(n);
            for i in 0..n {
                let slot = unsafe {
                    builder
                        .build_in_bounds_gep(i64_ty, args_ptr, &[i64_ty.const_int(i as u64, false)], "argslot")
                        .map_err(|e| e.to_string())?
                };
                let v = builder.build_load(i64_ty, slot, "arg").map_err(|e| e.to_string())?.into_int_value();
                argv.push(v.into());
            }
            let r = builder
                .build_call(funcs[uid], &argv, "taskret")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_int_value();
            builder.build_return(Some(&r)).map_err(|e| e.to_string())?;
        }
    }

    let struct_defaults = build_struct_defaults(struct_defs);
    let struct_field_names: HashMap<String, Vec<String>> = struct_defs
        .iter()
        .map(|(tn, fields)| (tn.clone(), fields.iter().map(|f| f.name().to_string()).collect()))
        .collect();
    // Every global the program assigns (SetGlobal) anywhere — a user variable that
    // shadows any reserved module name (so `name.method()` is not a module call).
    let mut user_globals = std::collections::HashSet::new();
    let mut collect_setglobals = |chunk: &Chunk| {
        for instr in &chunk.code {
            if let Instr::SetGlobal(n, _) = instr {
                user_globals.insert(n.clone());
            }
        }
    };
    collect_setglobals(top);
    for d in &defs {
        collect_setglobals(&d.chunk);
    }
    let refcount = program_collections_only(top, &defs);
    let fnctx = FnCtx { funcs, defs, ptr2uid, global_fns, struct_defaults, struct_field_names, method_candidates, user_globals, refcount };

    for uid in 0..fnctx.defs.len() {
        let cf = fnctx.defs[uid].clone();
        lower_body(
            context,
            module,
            fnctx.funcs[uid],
            &cf.chunk.code,
            &cf.chunk.fn_defs,
            cf.n_slots,
            cf.params.len(),
            &fnctx,
        )?;
    }

    let top_fn = module.add_function("jade_toplevel", i64_ty.fn_type(&[], false), None);
    lower_body(context, module, top_fn, &top.code, &top.fn_defs, top_n_slots, 0, &fnctx)?;

    // Turn on runtime reference counting for a collections-only program, once, at
    // the very start of `jade_toplevel` (before any collection is allocated). This
    // flips `RC_ACTIVE` so the runtime builders retain inserted/copy-shared
    // elements; codegen has already emitted the matching incref/decref/scope-exit
    // under the same `fnctx.refcount` decision. See gc.rs / program_collections_only.
    if fnctx.refcount {
        let en = module.get_function("jrt_rc_enable").unwrap_or_else(|| {
            module.add_function("jrt_rc_enable", context.void_type().fn_type(&[], false), None)
        });
        let entry = top_fn.get_first_basic_block().ok_or("lower.rs: jade_toplevel has no entry block")?;
        let eb = context.create_builder();
        match entry.get_first_instruction() {
            Some(first) => eb.position_before(&first),
            None => eb.position_at_end(entry),
        }
        eb.build_call(en, &[], "").map_err(|e| e.to_string())?;
    }

    // Populate the runtime method registry (for dynamic dispatch of
    // ambiguous-arity extend methods) at the very start of `jade_toplevel`, which
    // `main` calls before any user code. Registering every extend method is
    // harmless; only ambiguous names are ever looked up.
    let regs: Vec<(&String, &String, usize)> = {
        let mut v = Vec::new();
        for (ty, methods) in extend_methods {
            for (m, mfn) in methods {
                if let Some(&uid) = fnctx.ptr2uid.get(&Arc::as_ptr(mfn)) {
                    v.push((ty, m, uid));
                }
            }
        }
        v.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1))); // deterministic order
        v
    };
    if !regs.is_empty() {
        let ptr_ty = context.ptr_type(AddressSpace::default());
        let void_ty = context.void_type();
        let reg_fn = module.get_function("jrt_method_register").unwrap_or_else(|| {
            module.add_function(
                "jrt_method_register",
                void_ty.fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false),
                None,
            )
        });
        let entry = top_fn.get_first_basic_block().ok_or("lower.rs: jade_toplevel has no entry block")?;
        let rb = context.create_builder();
        match entry.get_first_instruction() {
            Some(first) => rb.position_before(&first),
            None => rb.position_at_end(entry),
        }
        for (ty, m, uid) in regs {
            let tcstr = rb.build_global_string_ptr(ty, "mtype").map_err(|e| e.to_string())?.as_pointer_value();
            let mcstr = rb.build_global_string_ptr(m, "mname").map_err(|e| e.to_string())?.as_pointer_value();
            let fnptr = fnctx.funcs[uid].as_global_value().as_pointer_value();
            rb.build_call(reg_fn, &[tcstr.into(), mcstr.into(), fnptr.into()], "")
                .map_err(|e| e.to_string())?;
        }
    }

    // Emit the type -> field table a struct-typed prompt deref coerces against,
    // in declaration order, in the same startup prologue as the method
    // registry. A field with a compile-time default is marked as such, since
    // "omitted optional" and "omitted required" behave differently: one is
    // filled, the other re-prompts.
    if !fnctx.struct_field_names.is_empty() {
        let ptr_ty = context.ptr_type(AddressSpace::default());
        let i64_ty2 = context.i64_type();
        let i32_ty2 = context.i32_type();
        let void_ty = context.void_type();
        let reg_field = module.get_function("jrt_struct_field").unwrap_or_else(|| {
            module.add_function(
                "jrt_struct_field",
                void_ty.fn_type(
                    &[ptr_ty.into(), ptr_ty.into(), i64_ty2.into(), i32_ty2.into()],
                    false,
                ),
                None,
            )
        });
        let entry = top_fn.get_first_basic_block().ok_or("lower.rs: jade_toplevel has no entry block")?;
        let rb = context.create_builder();
        match entry.get_first_instruction() {
            Some(first) => rb.position_before(&first),
            None => rb.position_at_end(entry),
        }
        let mut types: Vec<(&String, &Vec<String>)> = fnctx.struct_field_names.iter().collect();
        types.sort_by(|a, b| a.0.cmp(b.0)); // deterministic IR
        for (tn, fields) in types {
            let tcstr = rb.build_global_string_ptr(tn, "sftype").map_err(|e| e.to_string())?.as_pointer_value();
            let defaults = fnctx.struct_defaults.get(tn);
            for f in fields {
                let fcstr = rb.build_global_string_ptr(f, "sffield").map_err(|e| e.to_string())?.as_pointer_value();
                let dv = defaults.and_then(|ds| ds.iter().find(|(n, _)| n == f)).map(|(_, v)| v);
                let (word, has) = match dv {
                    Some(v) => (default_word_const(context, module, &rb, v)?, 1u64),
                    None => (i64_ty2.const_int(NIL, false), 0u64),
                };
                rb.build_call(
                    reg_field,
                    &[tcstr.into(), fcstr.into(), word.into(), i32_ty2.const_int(has, false).into()],
                    "",
                )
                .map_err(|e| e.to_string())?;
            }
        }
    }

    // Recover source-level names for the lowered functions, so `--lib` can
    // export `add` rather than `jf_7`.
    let named = fnctx
        .global_fns
        .iter()
        .filter_map(|(name, uid)| fnctx.funcs.get(*uid).map(|f| (name.clone(), *f)))
        .collect();

    Ok(LoweredProgram { toplevel: top_fn, global_fns: named })
}

/// Lower a single `Chunk` body into a new LLVM function `name() -> i64` (a
/// tagged value word). Thin wrapper over [`lower_body`] with no functions in
/// scope — used by the unit tests and any caller lowering an isolated body.
pub fn lower_chunk<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    name: &str,
    code: &[Instr],
    n_slots: u32,
) -> Result<FunctionValue<'ctx>, String> {
    let function = module.add_function(name, context.i64_type().fn_type(&[], false), None);
    lower_body(context, module, function, code, &[], n_slots, 0, &FnCtx::empty())?;
    Ok(function)
}

/// Lower `code` into an already-declared `function`. `n_params` incoming i64
/// parameters are copied into slots `0..n_params` (the VM frame convention);
/// `fn_defs` are this chunk's nested function literals (for `LoadFn`).
fn lower_body<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    function: FunctionValue<'ctx>,
    code: &[Instr],
    fn_defs: &[Arc<CompiledFn>],
    n_slots: u32,
    n_params: usize,
    fnctx: &FnCtx<'ctx>,
) -> Result<(), String> {
    let i64_ty = context.i64_type();
    let builder = context.create_builder();

    // Entry block: one alloca per register slot (at least enough for the params).
    let entry = context.append_basic_block(function, "entry");
    builder.position_at_end(entry);
    let n = (n_slots as usize).max(n_params);
    let mut slots = Vec::with_capacity(n);
    for i in 0..n {
        slots.push(
            builder
                .build_alloca(i64_ty, &format!("r{i}"))
                .map_err(|e| e.to_string())?,
        );
    }
    // In refcount mode, nil-initialize every slot so the release-old-value logic
    // in `store`/`store_idx` (and scope-exit `decref`) never reads uninitialized
    // stack: a first store releases nil (a no-op), and an unwritten slot decref's
    // nil at scope exit. Done before the param copies so params overwrite the nil.
    if fnctx.refcount {
        let nil = i64_ty.const_int(NIL, false);
        for s in &slots {
            builder.build_store(*s, nil).map_err(|e| e.to_string())?;
        }
    }
    // Copy incoming parameters into slots 0..n_params (params are the first
    // locals; see `emit_fn`). Callers fill any omitted defaults, so every
    // parameter slot receives an argument.
    for i in 0..n_params {
        let p = function
            .get_nth_param(i as u32)
            .ok_or("lower_body: missing parameter")?
            .into_int_value();
        builder.build_store(slots[i], p).map_err(|e| e.to_string())?;
    }
    // One jmp_buf per SetupHandler, allocated *in the entry block* so a handler
    // inside a loop reuses one stable buffer instead of growing the stack each
    // iteration. 256 bytes is conservative for every x86_64/arm64 jmp_buf.
    let buf_ty = context.i8_type().array_type(256);
    let mut handler_bufs: HashMap<usize, PointerValue> = HashMap::new();
    for (idx, instr) in code.iter().enumerate() {
        if matches!(instr, Instr::SetupHandler(..)) {
            let buf = builder
                .build_alloca(buf_ty, &format!("exc_buf{idx}"))
                .map_err(|e| e.to_string())?;
            handler_bufs.insert(idx, buf);
        }
    }

    // One LLVM block per reconstructed basic block; entry branches to the first.
    let graph = cfg::build(code);
    if graph.blocks.is_empty() {
        builder
            .build_return(Some(&i64_ty.const_int(NIL, false)))
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    let llblocks: Vec<LlvmBlock> = (0..graph.blocks.len())
        .map(|bi| context.append_basic_block(function, &format!("bb{bi}")))
        .collect();
    builder
        .build_unconditional_branch(llblocks[0])
        .map_err(|e| e.to_string())?;

    let low = Lowerer {
        ctx: context,
        module,
        builder: &builder,
        slots: &slots,
        refcount: fnctx.refcount,
        n_params,
    };
    let call_builtins = resolve_builtin_calls(code);
    let (user_calls, skip_getfields) = resolve_user_calls(code, fn_defs, fnctx)?;

    for (bi, block) in graph.blocks.iter().enumerate() {
        builder.position_at_end(llblocks[bi]);
        let mut terminated = false;
        for idx in block.start..block.end {
            // Skip a GetField whose only use is a devirtualized method call (its
            // field is a method, so lowering it would raise "undefined field").
            if skip_getfields.contains(&idx) {
                continue;
            }
            terminated = lower_instr(
                &low,
                &code[idx],
                idx,
                &llblocks,
                &graph,
                &handler_bufs,
                &call_builtins,
                &user_calls,
                fn_defs,
                fnctx,
            )?;
        }
        // A block whose last instruction wasn't a terminator falls through.
        if !terminated {
            match block.succs.first() {
                Some(&succ) => {
                    builder
                        .build_unconditional_branch(llblocks[succ])
                        .map_err(|e| e.to_string())?;
                }
                None => {
                    // Implicit `return nil` (function ran off the end): release
                    // the local slots first, matching an explicit `Return`.
                    low.emit_scope_exit();
                    builder
                        .build_return(Some(&i64_ty.const_int(NIL, false)))
                        .map_err(|e| e.to_string())?;
                }
            }
        }
    }

    Ok(())
}

/// Lower one instruction. Returns `Ok(true)` if it emitted a block terminator
/// (`Return`/`Jump`/conditional jump), `Ok(false)` otherwise.
fn lower_instr<'ctx>(
    low: &Lowerer<'_, 'ctx>,
    instr: &Instr,
    idx: usize,
    llblocks: &[LlvmBlock<'ctx>],
    graph: &cfg::Cfg,
    handler_bufs: &HashMap<usize, PointerValue<'ctx>>,
    call_builtins: &HashMap<usize, BuiltinCall>,
    user_calls: &HashMap<usize, CallKind>,
    fn_defs: &[Arc<CompiledFn>],
    fnctx: &FnCtx<'ctx>,
) -> Result<bool, String> {
    use Instr::*;
    let b = low.builder;
    let i64_ty = low.i64t();
    // PC-relative jump target (see cfg / bytecode.rs::patch_jump).
    let target = |off: i32| -> usize { (idx as i64 + 1 + off as i64) as usize };
    let block_of = |instr_idx: usize| -> LlvmBlock<'ctx> { llblocks[graph.block_at[&instr_idx]] };

    match instr {
        // ── Constant loads ────────────────────────────────────────────────
        LoadInt(d, v) => {
            // Tagged int = v << 1. One bit goes to the tag, so a word holds 63
            // bits, not 64 — and this used to just truncate, so
            // `print(9223372036854775807)` compiled to `-1`: a wrong answer, not
            // an error. The magnitude is known here, so refuse it at build time
            // rather than emitting something that silently computes garbage.
            const INT_MAX: i64 = (1 << 62) - 1;
            const INT_MIN: i64 = -(1 << 62);
            if *v > INT_MAX || *v < INT_MIN {
                return Err(format!(
                    "integer overflow: {v} does not fit a Jade integer \
                     (the compiled representation holds {INT_MIN}..={INT_MAX})"
                ));
            }
            let tagged = (*v as i64).wrapping_shl(1) as u64;
            low.store(*d, i64_ty.const_int(tagged, false));
            Ok(false)
        }
        LoadBool(d, v) => {
            low.store(*d, i64_ty.const_int(if *v { TRUE } else { FALSE }, false));
            Ok(false)
        }
        LoadNil(d) => {
            low.store(*d, i64_ty.const_int(NIL, false));
            Ok(false)
        }

        // ── Move ──────────────────────────────────────────────────────────
        Move(d, s) => {
            let v = low.load(*s);
            low.retain(v); // borrowed alias → the dest slot becomes a new owner
            low.store(*d, v);
            Ok(false)
        }

        // ── Variable access ───────────────────────────────────────────────
        // Locals share the register slot array (see VM GetLocal/SetLocal), so
        // these are moves between slot indices.
        GetLocal(d, slot) => {
            let v = low.load_idx(*slot as usize);
            low.retain(v); // borrowed alias
            low.store(*d, v);
            Ok(false)
        }
        SetLocal(slot, s) => {
            let v = low.load(*s);
            low.retain(v); // borrowed alias into the target local
            low.store_idx(*slot as usize, v);
            Ok(false)
        }
        // Module-scoped globals: load/store the named LLVM global cell.
        GetGlobal(d, name) => {
            // A native package reference used as a value materializes a first-class
            // native function value (a jade_fn sentinel); an ordinary global loads
            // its cell. (When immediately called, the `Call` devirtualizes to a
            // NativeCall and this materialized value is dead-code-eliminated.)
            let v = if let Some((pkgid, fname)) = parse_native_ref(name) {
                emit_native_fn_value(low, pkgid, fname)?
            } else {
                let g = low.global_slot(name);
                b.build_load(i64_ty, g, "gld").map_err(|e| e.to_string())?.into_int_value()
            };
            low.retain(v); // borrowed from the global cell (native path is a fn value, but refcount is off then)
            low.store(*d, v);
            Ok(false)
        }
        SetGlobal(name, s) => {
            let v = low.load(*s);
            let g = low.global_slot(name);
            // The global cell becomes a new owner: retain the value, and release
            // whatever the cell held before (globals are never scope-exit-released,
            // so their final value intentionally lives until process end).
            low.retain(v);
            if low.refcount {
                let old = b.build_load(i64_ty, g, "gold").map_err(|e| e.to_string())?.into_int_value();
                let f = low.runtime_fn(
                    "jrt_rc_replace",
                    low.ctx.void_type().fn_type(&[i64_ty.into(), i64_ty.into()], false),
                );
                b.build_call(f, &[old.into(), v.into()], "").map_err(|e| e.to_string())?;
            }
            b.build_store(g, v).map_err(|e| e.to_string())?;
            Ok(false)
        }

        // ── Integer arithmetic (native op on untagged, then re-tag) ───────
        // Overflow-checked, matching the VM's checked_add/sub/mul. See
        // `checked_int_result` for why the arithmetic widens to i128 first.
        AddInt(d, l, r) => {
            let (a, c) = low.int_operands(*l, *r);
            let s = b.build_int_add(low.widen(a)?, low.widen(c)?, "addi").map_err(|e| e.to_string())?;
            let res = low.checked_int_result(s, "addi")?;
            low.store(*d, res);
            Ok(false)
        }
        SubInt(d, l, r) => {
            let (a, c) = low.int_operands(*l, *r);
            let s = b.build_int_sub(low.widen(a)?, low.widen(c)?, "subi").map_err(|e| e.to_string())?;
            let res = low.checked_int_result(s, "subi")?;
            low.store(*d, res);
            Ok(false)
        }
        MulInt(d, l, r) => {
            let (a, c) = low.int_operands(*l, *r);
            let s = b.build_int_mul(low.widen(a)?, low.widen(c)?, "muli").map_err(|e| e.to_string())?;
            let res = low.checked_int_result(s, "muli")?;
            low.store(*d, res);
            Ok(false)
        }
        NegInt(d, s) => {
            let a = low.untag_int(low.load(*s));
            let n = b.build_int_neg(low.widen(a)?, "negi").map_err(|e| e.to_string())?;
            let res = low.checked_int_result(n, "negi")?;
            low.store(*d, res);
            Ok(false)
        }
        DivInt(d, l, r) => {
            let res = low.int_div_mod(*l, *r, false)?;
            low.store(*d, res);
            Ok(false)
        }
        ModInt(d, l, r) => {
            let res = low.int_div_mod(*l, *r, true)?;
            low.store(*d, res);
            Ok(false)
        }

        // ── Float loads / arithmetic (unbox, native op, re-box) ───────────
        LoadFloat(d, v) => {
            let c = i64_ty.const_int(v.to_bits(), false);
            let f = b
                .build_bit_cast(c, low.f64t(), "fbits")
                .map_err(|e| e.to_string())?
                .into_float_value();
            low.store(*d, low.box_float(f));
            Ok(false)
        }
        AddFloat(d, l, r) => {
            let (a, c) = low.float_operands(*l, *r);
            let s = b.build_float_add(a, c, "addf").map_err(|e| e.to_string())?;
            low.store(*d, low.box_float(s));
            Ok(false)
        }
        SubFloat(d, l, r) => {
            let (a, c) = low.float_operands(*l, *r);
            let s = b.build_float_sub(a, c, "subf").map_err(|e| e.to_string())?;
            low.store(*d, low.box_float(s));
            Ok(false)
        }
        MulFloat(d, l, r) => {
            let (a, c) = low.float_operands(*l, *r);
            let s = b.build_float_mul(a, c, "mulf").map_err(|e| e.to_string())?;
            low.store(*d, low.box_float(s));
            Ok(false)
        }
        DivFloat(d, l, r) => {
            let (a, c) = low.float_operands(*l, *r);
            let s = b.build_float_div(a, c, "divf").map_err(|e| e.to_string())?;
            low.store(*d, low.box_float(s));
            Ok(false)
        }
        NegFloat(d, s) => {
            let a = low.unbox_float(low.load(*s));
            let n = b.build_float_neg(a, "negf").map_err(|e| e.to_string())?;
            low.store(*d, low.box_float(n));
            Ok(false)
        }
        IntToFloat(d, s) => {
            let i = low.untag_int(low.load(*s));
            let f = b
                .build_signed_int_to_float(i, low.f64t(), "i2f")
                .map_err(|e| e.to_string())?;
            low.store(*d, low.box_float(f));
            Ok(false)
        }

        // ── Typed comparisons → bool word (native icmp/fcmp) ──────────────
        CmpEqInt(d, l, r) => { low.store(*d, low.int_cmp(*l, *r, IntPredicate::EQ)); Ok(false) }
        CmpNeInt(d, l, r) => { low.store(*d, low.int_cmp(*l, *r, IntPredicate::NE)); Ok(false) }
        CmpLtInt(d, l, r) => { low.store(*d, low.int_cmp(*l, *r, IntPredicate::SLT)); Ok(false) }
        CmpGtInt(d, l, r) => { low.store(*d, low.int_cmp(*l, *r, IntPredicate::SGT)); Ok(false) }
        CmpLeInt(d, l, r) => { low.store(*d, low.int_cmp(*l, *r, IntPredicate::SLE)); Ok(false) }
        CmpGeInt(d, l, r) => { low.store(*d, low.int_cmp(*l, *r, IntPredicate::SGE)); Ok(false) }

        CmpEqFloat(d, l, r) => { low.store(*d, low.float_cmp(*l, *r, FloatPredicate::OEQ)); Ok(false) }
        CmpNeFloat(d, l, r) => { low.store(*d, low.float_cmp(*l, *r, FloatPredicate::UNE)); Ok(false) }
        CmpLtFloat(d, l, r) => { low.store(*d, low.float_cmp(*l, *r, FloatPredicate::OLT)); Ok(false) }
        CmpGtFloat(d, l, r) => { low.store(*d, low.float_cmp(*l, *r, FloatPredicate::OGT)); Ok(false) }
        CmpLeFloat(d, l, r) => { low.store(*d, low.float_cmp(*l, *r, FloatPredicate::OLE)); Ok(false) }
        CmpGeFloat(d, l, r) => { low.store(*d, low.float_cmp(*l, *r, FloatPredicate::OGE)); Ok(false) }

        CmpEqBool(d, l, r) => { low.store(*d, low.bool_cmp(*l, *r, IntPredicate::EQ)); Ok(false) }
        CmpNeBool(d, l, r) => { low.store(*d, low.bool_cmp(*l, *r, IntPredicate::NE)); Ok(false) }
        CmpLtBool(d, l, r) => { low.store(*d, low.bool_cmp(*l, *r, IntPredicate::ULT)); Ok(false) }
        CmpGtBool(d, l, r) => { low.store(*d, low.bool_cmp(*l, *r, IntPredicate::UGT)); Ok(false) }
        CmpLeBool(d, l, r) => { low.store(*d, low.bool_cmp(*l, *r, IntPredicate::ULE)); Ok(false) }
        CmpGeBool(d, l, r) => { low.store(*d, low.bool_cmp(*l, *r, IntPredicate::UGE)); Ok(false) }

        CmpLtIntFloat(d, l, r) => { low.store(*d, low.mixed_cmp(*l, true, *r, false, FloatPredicate::OLT)); Ok(false) }
        CmpGtIntFloat(d, l, r) => { low.store(*d, low.mixed_cmp(*l, true, *r, false, FloatPredicate::OGT)); Ok(false) }
        CmpLeIntFloat(d, l, r) => { low.store(*d, low.mixed_cmp(*l, true, *r, false, FloatPredicate::OLE)); Ok(false) }
        CmpGeIntFloat(d, l, r) => { low.store(*d, low.mixed_cmp(*l, true, *r, false, FloatPredicate::OGE)); Ok(false) }
        CmpLtFloatInt(d, l, r) => { low.store(*d, low.mixed_cmp(*l, false, *r, true, FloatPredicate::OLT)); Ok(false) }
        CmpGtFloatInt(d, l, r) => { low.store(*d, low.mixed_cmp(*l, false, *r, true, FloatPredicate::OGT)); Ok(false) }
        CmpLeFloatInt(d, l, r) => { low.store(*d, low.mixed_cmp(*l, false, *r, true, FloatPredicate::OLE)); Ok(false) }
        CmpGeFloatInt(d, l, r) => { low.store(*d, low.mixed_cmp(*l, false, *r, true, FloatPredicate::OGE)); Ok(false) }

        // ── Logical / bitwise (integers) ──────────────────────────────────
        Not(d, s) => {
            let b1 = low.untag_bool(low.load(*s));
            let n = b.build_not(b1, "lnot").map_err(|e| e.to_string())?;
            low.store(*d, low.bool_word(n));
            Ok(false)
        }
        BitAnd(d, l, r) => { low.store(*d, low.int_bitop(*l, *r, |a, c| b.build_and(a, c, "band").map_err(|e| e.to_string()))?); Ok(false) }
        BitOr(d, l, r)  => { low.store(*d, low.int_bitop(*l, *r, |a, c| b.build_or(a, c, "bor").map_err(|e| e.to_string()))?); Ok(false) }
        BitXor(d, l, r) => { low.store(*d, low.int_bitop(*l, *r, |a, c| b.build_xor(a, c, "bxor").map_err(|e| e.to_string()))?); Ok(false) }
        Shl(d, l, r)    => { low.store(*d, low.int_bitop(*l, *r, |a, c| b.build_left_shift(a, c, "shl").map_err(|e| e.to_string()))?); Ok(false) }
        Shr(d, l, r)    => { low.store(*d, low.int_bitop(*l, *r, |a, c| b.build_right_shift(a, c, true, "shr").map_err(|e| e.to_string()))?); Ok(false) }
        BitNot(d, s) => {
            let a = low.untag_int(low.load(*s));
            let n = b.build_not(a, "bnot").map_err(|e| e.to_string())?;
            low.store(*d, low.tag_int(n));
            Ok(false)
        }

        // ── Dynamic ops (Unknown-typed operands) → jrt_*_any (A7) ─────────
        // Emitted when the type-inferrer can't specialize (notably every
        // function-parameter use, since Jade params are untyped). Routes to the
        // same tag-dispatching decision core the VM runs.
        BinOp(d, op, l, r) => {
            use BinOpKind::*;
            match op {
                Add => low.store(*d, low.any2("jrt_add_any", *l, *r)),
                Sub => low.store(*d, low.any2("jrt_sub_any", *l, *r)),
                Mul => low.store(*d, low.any2("jrt_mul_any", *l, *r)),
                Div => low.store(*d, low.any2("jrt_div_any", *l, *r)),
                Mod => low.store(*d, low.any2("jrt_mod_any", *l, *r)),
                // Bitwise/shift are int-only: untag, native op, re-tag.
                BitAnd => low.store(*d, low.int_bitop(*l, *r, |a, c| b.build_and(a, c, "band").map_err(|e| e.to_string()))?),
                BitOr  => low.store(*d, low.int_bitop(*l, *r, |a, c| b.build_or(a, c, "bor").map_err(|e| e.to_string()))?),
                BitXor => low.store(*d, low.int_bitop(*l, *r, |a, c| b.build_xor(a, c, "bxor").map_err(|e| e.to_string()))?),
                Shl    => low.store(*d, low.int_bitop(*l, *r, |a, c| b.build_left_shift(a, c, "shl").map_err(|e| e.to_string()))?),
                Shr    => low.store(*d, low.int_bitop(*l, *r, |a, c| b.build_right_shift(a, c, true, "shr").map_err(|e| e.to_string()))?),
                // `x in y` / `x not in y` — runtime containment (substring / array
                // element / dict key), producing a bool word.
                In | NotIn => {
                    let f = low.runtime_fn(
                        "jrt_in_any",
                        low.ctx.i32_type().fn_type(&[i64_ty.into(), i64_ty.into()], false),
                    );
                    let present = b
                        .build_call(f, &[low.load(*l).into(), low.load(*r).into()], "inany")
                        .map_err(|e| e.to_string())?
                        .as_any_value_enum()
                        .into_int_value();
                    let pred = if matches!(op, In) { IntPredicate::NE } else { IntPredicate::EQ };
                    low.store(*d, low.i32cmp_word(present, pred));
                }
                // And/Or are emitted as short-circuit jumps, never a BinOp opcode.
                _ => return Err(format!("lower.rs: unsupported dynamic BinOp {op:?}")),
            }
            Ok(false)
        }
        UnaryOp(d, op, s) => {
            match op {
                UnaryOpKind::Neg => low.store(*d, low.neg_any(*s)),
                UnaryOpKind::Not => {
                    // Untag the (tagged) bool operand, invert, re-wrap as a word.
                    let b1 = low.untag_bool(low.load(*s));
                    let n = b.build_not(b1, "lnotd").map_err(|e| e.to_string())?;
                    low.store(*d, low.bool_word(n));
                }
                UnaryOpKind::BitNot => {
                    let a = low.untag_int(low.load(*s));
                    let n = b.build_not(a, "bnotd").map_err(|e| e.to_string())?;
                    low.store(*d, low.tag_int(n));
                }
            }
            Ok(false)
        }
        // Dynamic equality/ordering → bool word (mirror expr.rs emit_binop_any).
        CmpEq(d, l, r) => { let e = low.eq_any(*l, *r); low.store(*d, low.i32cmp_word(e, IntPredicate::NE)); Ok(false) }
        CmpNe(d, l, r) => { let e = low.eq_any(*l, *r); low.store(*d, low.i32cmp_word(e, IntPredicate::EQ)); Ok(false) }
        CmpLt(d, l, r) => { let c = low.cmp_any(*l, *r); low.store(*d, low.i32cmp_word(c, IntPredicate::SLT)); Ok(false) }
        CmpGt(d, l, r) => { let c = low.cmp_any(*l, *r); low.store(*d, low.i32cmp_word(c, IntPredicate::SGT)); Ok(false) }
        CmpLe(d, l, r) => { let c = low.cmp_any(*l, *r); low.store(*d, low.i32cmp_word(c, IntPredicate::SLE)); Ok(false) }
        CmpGe(d, l, r) => { let c = low.cmp_any(*l, *r); low.store(*d, low.i32cmp_word(c, IntPredicate::SGE)); Ok(false) }

        // ── Strings (pre-tagged literal globals; runtime concat/compare) ──
        LoadStr(d, s) => {
            let ptr = low.str_literal_ptr(s)?;
            low.store(*d, low.tag_str(ptr));
            Ok(false)
        }
        ConcatStr(d, l, r) => {
            low.store(*d, low.str_concat(*l, *r));
            Ok(false)
        }
        CmpEqStr(d, l, r) => { low.store(*d, low.str_cmp(*l, *r, IntPredicate::EQ)); Ok(false) }
        CmpNeStr(d, l, r) => { low.store(*d, low.str_cmp(*l, *r, IntPredicate::NE)); Ok(false) }
        CmpLtStr(d, l, r) => { low.store(*d, low.str_cmp(*l, *r, IntPredicate::SLT)); Ok(false) }
        CmpGtStr(d, l, r) => { low.store(*d, low.str_cmp(*l, *r, IntPredicate::SGT)); Ok(false) }
        CmpLeStr(d, l, r) => { low.store(*d, low.str_cmp(*l, *r, IntPredicate::SLE)); Ok(false) }
        CmpGeStr(d, l, r) => { low.store(*d, low.str_cmp(*l, *r, IntPredicate::SGE)); Ok(false) }

        // ── f-strings ─────────────────────────────────────────────────────
        // Fold the parts left-to-right with `jrt_str_concat` (trust = max):
        // each part is a tagged-string data pointer — a compile-time literal
        // global, or `jrt_str_of_any(value)` for an interpolated register. An
        // empty template yields the empty string.
        BuildFStr(d, parts) => {
            let mut acc: Option<PointerValue> = None;
            for part in parts {
                let p_ptr = match part {
                    FStrPart::Literal(s) => low.str_literal_ptr(s)?,
                    FStrPart::Reg(r) => low.str_of_any(*r),
                };
                acc = Some(match acc {
                    None => p_ptr,
                    Some(prev) => low.concat_ptrs(prev, p_ptr),
                });
            }
            let ptr = match acc {
                Some(p) => p,
                None => low.str_literal_ptr("")?,
            };
            low.store(*d, low.tag_str(ptr));
            Ok(false)
        }

        // ── Collections: kind-tagged arrays (A8) ──────────────────────────
        // MakeArray: allocate a kind-tagged array, push each element (a tagged
        // word), tag the pointer as a heap object. GetIndex/SetIndex dispatch on
        // the object's tag/kind in the runtime (string/array; dict is a later
        // sub-brick). len/print/str/f-strings are already collection-aware via
        // jrt_len_unknown / jrt_render_any.
        MakeArray(d, regs) => {
            let new_f = low.runtime_fn("jrt_karr_new", low.ptrt().fn_type(&[], false));
            let arr = b
                .build_call(new_f, &[], "karr")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_pointer_value();
            let push_f = low.runtime_fn(
                "jrt_karr_push",
                low.ctx.void_type().fn_type(&[low.ptrt().into(), i64_ty.into()], false),
            );
            for r in regs {
                let v = low.load(*r);
                b.build_call(push_f, &[arr.into(), v.into()], "").map_err(|e| e.to_string())?;
            }
            low.store(*d, low.tag_ptr(arr));
            Ok(false)
        }
        GetIndex(d, obj, idx) => {
            let f = low.runtime_fn(
                "jrt_val_index",
                i64_ty.fn_type(&[i64_ty.into(), i64_ty.into()], false),
            );
            let r = b
                .build_call(f, &[low.load(*obj).into(), low.load(*idx).into()], "index")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_int_value();
            low.retain(r); // borrowed element (still owned by the container) → retain
            low.store(*d, r);
            Ok(false)
        }
        MakeDict(d, pairs) => {
            let new_f = low.runtime_fn("jrt_kdict_new", low.ptrt().fn_type(&[], false));
            let dict = b
                .build_call(new_f, &[], "kdict")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_pointer_value();
            let set_f = low.runtime_fn(
                "jrt_kdict_set",
                low.ctx.void_type().fn_type(&[low.ptrt().into(), i64_ty.into(), i64_ty.into()], false),
            );
            for (kr, vr) in pairs {
                let k = low.load(*kr);
                let v = low.load(*vr);
                b.build_call(set_f, &[dict.into(), k.into(), v.into()], "")
                    .map_err(|e| e.to_string())?;
            }
            low.store(*d, low.tag_ptr(dict));
            Ok(false)
        }
        // SetIndex returns the (possibly new) container word — an array is
        // mutated in place (same pointer), a dict is copied (value semantics) —
        // so we store it back into the object register. The emitter's following
        // write-back (`SetIndex` then a store of `obj`) then rebinds the variable.
        SetIndex(obj, idx, val) => {
            let f = low.runtime_fn(
                "jrt_val_set_index",
                i64_ty.fn_type(&[i64_ty.into(), i64_ty.into(), i64_ty.into()], false),
            );
            let new_word = b
                .build_call(
                    f,
                    &[low.load(*obj).into(), low.load(*idx).into(), low.load(*val).into()],
                    "setidx",
                )
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_int_value();
            low.store(*obj, new_word);
            Ok(false)
        }

        // ── Structs (data fields only; methods decline in resolve_user_calls) ──
        // MakeStruct: kind-tagged struct carrying the type name + explicit fields,
        // then fill any omitted optional field from its scalar default (the VM
        // fills these at runtime). GetField/SetField are data-field access on a
        // struct (a missing field / non-struct raises).
        MakeStruct(d, type_name, field_specs) => {
            if field_specs.iter().any(|(_, _, is_prompt)| *is_prompt) {
                return Err("lower.rs: prompt struct fields are unsupported".into());
            }
            let new_f = low.runtime_fn("jrt_kstruct_new", low.ptrt().fn_type(&[low.ptrt().into()], false));
            let tn = low.cstr(type_name);
            let s = b
                .build_call(new_f, &[tn.into()], "kstruct")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_pointer_value();
            let set_f = low.runtime_fn(
                "jrt_kstruct_set",
                low.ctx.void_type().fn_type(&[low.ptrt().into(), low.ptrt().into(), i64_ty.into()], false),
            );
            for (fname, freg, _) in field_specs {
                let v = low.load(*freg);
                b.build_call(set_f, &[s.into(), low.cstr(fname).into(), v.into()], "")
                    .map_err(|e| e.to_string())?;
            }
            // Fill omitted optional fields from their scalar defaults.
            if let Some(defaults) = fnctx.struct_defaults.get(type_name) {
                for (fname, dv) in defaults {
                    if field_specs.iter().all(|(n, _, _)| n != fname) {
                        let w = low.default_word(dv)?;
                        b.build_call(set_f, &[s.into(), low.cstr(fname).into(), w.into()], "")
                            .map_err(|e| e.to_string())?;
                    }
                }
            }
            low.store(*d, low.tag_ptr(s));
            Ok(false)
        }
        GetField(d, obj, field) => {
            let f = low.runtime_fn(
                "jrt_get_field",
                i64_ty.fn_type(&[i64_ty.into(), low.ptrt().into()], false),
            );
            let r = b
                .build_call(f, &[low.load(*obj).into(), low.cstr(field).into()], "getfield")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_int_value();
            low.retain(r); // borrowed field value (still owned by the struct) → retain
            low.store(*d, r);
            Ok(false)
        }
        SetField(obj, field, val) => {
            let f = low.runtime_fn(
                "jrt_set_field",
                low.ctx.void_type().fn_type(&[i64_ty.into(), low.ptrt().into(), i64_ty.into()], false),
            );
            b.build_call(
                f,
                &[low.load(*obj).into(), low.cstr(field).into(), low.load(*val).into()],
                "",
            )
            .map_err(|e| e.to_string())?;
            Ok(false)
        }
        // GetTypeName (typed `catch <Type>`): the caught value's struct type name
        // as a tagged string (empty for a non-struct), compared against the
        // expected name with CmpEqStr. Now lowerable via the JK_STRUCT type tag.
        GetTypeName(d, src) => {
            let f = low.runtime_fn("jrt_get_type_name", low.ptrt().fn_type(&[i64_ty.into()], false));
            let p = b
                .build_call(f, &[low.load(*src).into()], "typename")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_pointer_value();
            low.store(*d, low.tag_str(p));
            Ok(false)
        }

        // ── LLM prompts ───────────────────────────────────────────────────
        // A prompt value is its underlying string, deferred until PromptDeref.
        // (No distinct heap kind: the prompt only ever flows to PromptDeref.)
        MakePrompt(d, text) => {
            low.store(*d, low.load(*text));
            Ok(false)
        }
        // Run inference on the prompt string, dispatching like the VM:
        //   grammar (`|> <Grammar value>`) → jrt_prompt_grammar_obj (reads the
        //     runtime Grammar object, converts pattern→GBNF, constrains sampling);
        //   typed (`|> int/float/bool/str`) → jrt_prompt_typed (retries until the
        //     response parses) → coerce via jrt_{int,float,bool}_any;
        //   unconstrained (`?p`) → jrt_prompt (returns the full text, no streaming).
        // OUTPUT PARITY: the VM's unconstrained `?p` is a *lazy* TokenStream — no
        // output until it is consumed, at which point `print(r)` streams it once
        // and adds the newline. If the AOT streamed eagerly at the deref *and*
        // then printed the returned text, `let r = ?p; print(r)` would emit the
        // response twice. Using the non-streaming jrt_prompt (like the VM's typed
        // path) makes `print(r)` the single output site → byte-identical stdout
        // (the VM streams tokens live, the AOT prints them in one go — same bytes).
        // The jrt_prompt* helpers already return a tagged, trust-propagated string
        // (a trusted prompt yields a trusted response), so the result is tag_str'd
        // directly — no re-dup, no forced taint. model via jrt_get_model.
        PromptDeref(d, prompt_reg, output_type, grammar_reg) => {
            let ptrt = low.ptrt();
            let i32_ty = low.ctx.i32_type();
            let e = |x: inkwell::builder::BuilderError| x.to_string();

            let prompt_ptr = low.untag_ptr(low.load(*prompt_reg));
            let model_fn = low.runtime_fn("jrt_get_model", ptrt.fn_type(&[], false));
            let model = b.build_call(model_fn, &[], "model").map_err(e)?.as_any_value_enum().into_pointer_value();

            let raw = if let Some(gr) = grammar_reg {
                // Grammar-constrained: pass the Grammar object pointer.
                let gobj = low.untag_ptr(low.load(*gr));
                let f = low.runtime_fn(
                    "jrt_prompt_grammar_obj",
                    ptrt.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into()], false),
                );
                b.build_call(f, &[prompt_ptr.into(), model.into(), gobj.into()], "promptg")
                    .map_err(e)?
                    .as_any_value_enum()
                    .into_pointer_value()
            } else if let Some(t) = output_type.as_deref().filter(|t| fnctx.struct_field_names.contains_key(*t)) {
                // A struct output type coerces the reply into a struct. This
                // returns a tagged *word*, not a string, so it bypasses the
                // tag_str + coerce tail below entirely. Without it the C
                // validator waved the raw reply through and the deref produced
                // a string, which then failed on any field access.
                let i32_ty = low.ctx.i32_type();
                let f = low.runtime_fn(
                    "jrt_prompt_struct",
                    i64_ty.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into(), i32_ty.into()], false),
                );
                let tname = low.cstr(t);
                let retries = i32_ty.const_int(crate::vm::TYPED_DEREF_RETRIES as u64, false);
                let w = b
                    .build_call(f, &[prompt_ptr.into(), model.into(), tname.into(), retries.into()], "prompts")
                    .map_err(e)?
                    .as_any_value_enum()
                    .into_int_value();
                low.store(*d, w);
                return Ok(false);
            } else if let Some(t) = output_type.as_deref() {
                // ..._checked, not the bare jrt_prompt_typed: that returns NULL
                // when the retries run out, and tagging NULL as a string
                // segfaulted the program where the VM reported a clean error.
                let f = low.runtime_fn(
                    "jrt_prompt_typed_checked",
                    ptrt.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into(), i32_ty.into()], false),
                );
                let tname = low.cstr(t);
                // Fixed retry budget, shared with the VM (crate::vm::TYPED_DEREF_RETRIES)
                // so both engines give up after the same number of attempts. It used
                // to be a config-injected runtime value read via jrt_max_retries.
                let retries = i32_ty.const_int(crate::vm::TYPED_DEREF_RETRIES as u64, false);
                b.build_call(
                    f,
                    &[prompt_ptr.into(), model.into(), tname.into(), retries.into()],
                    "promptt",
                )
                .map_err(e)?
                .as_any_value_enum()
                .into_pointer_value()
            } else {
                let f = low.runtime_fn("jrt_prompt", ptrt.fn_type(&[ptrt.into(), ptrt.into()], false));
                b.build_call(f, &[prompt_ptr.into(), model.into()], "prompt")
                    .map_err(e)?
                    .as_any_value_enum()
                    .into_pointer_value()
            };

            let str_word = low.tag_str(raw);

            // Coerce the typed variants (retry guaranteed a parseable response).
            let result = match output_type.as_deref() {
                Some(ty @ ("int" | "float" | "bool")) => {
                    let name = match ty {
                        "int" => "jrt_int_any",
                        "float" => "jrt_float_any",
                        _ => "jrt_bool_any",
                    };
                    let f = low.runtime_fn(name, i64_ty.fn_type(&[i64_ty.into()], false));
                    b.build_call(f, &[str_word.into()], "pconv")
                        .map_err(e)?
                        .as_any_value_enum()
                        .into_int_value()
                }
                _ => str_word, // None or "str"
            };
            low.store(*d, result);
            Ok(false)
        }

        // ── Async: spawn / await / join ───────────────────────────────────
        // Spawn a known async function: pack args into a stack array and call
        // jade_spawn(jf_task_<uid>, args, n). The future is stored as a raw
        // pointer word (futures only flow to await/join, never generic ops).
        Spawn(dest, _callee, _args) => match user_calls.get(&idx) {
            Some(CallKind::Spawn { uid, args }) => {
                // Pack `params.len()` slots: provided args, then omitted trailing
                // defaults (the task wrapper unpacks exactly that many).
                let cf = &fnctx.defs[*uid];
                let n = cf.params.len();
                let count = i64_ty.const_int(n.max(1) as u64, false);
                let arr = b.build_array_alloca(i64_ty, count, "spawn_args").map_err(|e| e.to_string())?;
                let store_slot = |slot_i: usize, val: IntValue| -> Result<(), String> {
                    let slot = unsafe {
                        b.build_in_bounds_gep(i64_ty, arr, &[i64_ty.const_int(slot_i as u64, false)], "sa")
                            .map_err(|e| e.to_string())?
                    };
                    b.build_store(slot, val).map_err(|e| e.to_string())?;
                    Ok(())
                };
                for (i, r) in args.iter().enumerate() {
                    store_slot(i, low.load(*r))?;
                }
                for j in args.len()..n {
                    let dv = cf.defaults[j].as_ref().ok_or("lower.rs: missing spawn default")?;
                    store_slot(j, low.default_word(dv)?)?;
                }
                let task = low
                    .module
                    .get_function(&format!("jf_task_{uid}"))
                    .ok_or("lower.rs: missing task wrapper")?;
                let spawn_f = low.runtime_fn(
                    "jade_spawn",
                    low.ptrt().fn_type(&[low.ptrt().into(), low.ptrt().into(), low.ctx.i32_type().into()], false),
                );
                let fut = b
                    .build_call(
                        spawn_f,
                        &[
                            task.as_global_value().as_pointer_value().into(),
                            arr.into(),
                            low.ctx.i32_type().const_int(n as u64, false).into(),
                        ],
                        "spawn",
                    )
                    .map_err(|e| e.to_string())?
                    .as_any_value_enum()
                    .into_pointer_value();
                // A future is an ordinary tagged value, not a bare pointer.
                // Storing it untagged made it indistinguishable from an Int to
                // every dynamic op: `print(f)` rendered the pointer as a huge
                // integer, and `await 5` happily int_to_ptr'd an integer and
                // segfaulted. It now carries TAG_PTR and ObjKind::Future, so
                // the renderer and the await guard can both recognise it.
                low.store(*dest, low.tag_ptr(fut));
                Ok(false)
            }
            _ => Err(format!("lower.rs: unsupported spawn at {idx}")),
        },
        Await(dest, fut) => {
            // Pass the tagged word through: the runtime checks the tag and the
            // ObjKind before touching the pointer, so awaiting a non-future
            // raises instead of dereferencing an integer.
            let await_f = low.runtime_fn("jade_await_word", i64_ty.fn_type(&[i64_ty.into()], false));
            let r = b
                .build_call(await_f, &[low.load(*fut).into()], "await")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_int_value();
            low.store(*dest, r);
            Ok(false)
        }
        Join(dest, futs) => {
            let n = futs.len();
            let cnt = i64_ty.const_int(n.max(1) as u64, false);
            let futarr = b.build_array_alloca(i64_ty, cnt, "join_futs").map_err(|e| e.to_string())?;
            for (i, r) in futs.iter().enumerate() {
                let slot = unsafe {
                    b.build_in_bounds_gep(i64_ty, futarr, &[i64_ty.const_int(i as u64, false)], "jfs")
                        .map_err(|e| e.to_string())?
                };
                b.build_store(slot, low.load(*r)).map_err(|e| e.to_string())?;
            }
            let resarr = b.build_array_alloca(i64_ty, cnt, "join_res").map_err(|e| e.to_string())?;
            let join_f = low.runtime_fn(
                "jade_join_words",
                low.ctx.void_type().fn_type(&[low.ptrt().into(), low.ctx.i32_type().into(), low.ptrt().into()], false),
            );
            b.build_call(
                join_f,
                &[futarr.into(), low.ctx.i32_type().const_int(n as u64, false).into(), resarr.into()],
                "",
            )
            .map_err(|e| e.to_string())?;
            // Collect results into a kind-tagged array.
            let new_f = low.runtime_fn("jrt_karr_new", low.ptrt().fn_type(&[], false));
            let arr = b
                .build_call(new_f, &[], "jarr")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_pointer_value();
            let push_f = low.runtime_fn(
                "jrt_karr_push",
                low.ctx.void_type().fn_type(&[low.ptrt().into(), i64_ty.into()], false),
            );
            for i in 0..n {
                let slot = unsafe {
                    b.build_in_bounds_gep(i64_ty, resarr, &[i64_ty.const_int(i as u64, false)], "jrs")
                        .map_err(|e| e.to_string())?
                };
                let v = b.build_load(i64_ty, slot, "jr").map_err(|e| e.to_string())?.into_int_value();
                b.build_call(push_f, &[arr.into(), v.into()], "").map_err(|e| e.to_string())?;
            }
            low.store(*dest, low.tag_ptr(arr));
            Ok(false)
        }

        // ── Exceptions (raise side; catch side is a later brick) ──────────
        // Raise longjmps to the active handler (set up by SetupHandler) or, if
        // none, aborts with an explicit message — matching the VM's uncaught
        // path. It never returns, so it terminates the block.
        Raise(val) => {
            let v = low.load(*val);
            low.throw(v)?;
            Ok(true)
        }
        // Register a handler frame and split on `setjmp`: 0 → try body
        // (fallthrough, idx+1); non-zero (a longjmp arrived) → a landing block
        // that stores the caught value into `caught_reg` and enters the handler
        // block (idx+1+offset). cfg records the handler as a leader-but-not-
        // normal-successor, so the only edge into it is this landing.
        SetupHandler(caught_reg, off) => {
            let buf = handler_bufs
                .get(&idx)
                .copied()
                .ok_or("SetupHandler: no jmp_buf pre-allocated")?;
            low.push_frame(buf);
            let sj = low.setjmp_fn();
            let r = b
                .build_call(sj, &[buf.into()], "setjmp_r")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_int_value();
            let is_throw = b
                .build_int_compare(
                    IntPredicate::NE,
                    r,
                    low.ctx.i32_type().const_zero(),
                    "is_throw",
                )
                .map_err(|e| e.to_string())?;
            let func = b.get_insert_block().and_then(|bb| bb.get_parent()).unwrap();
            let landing = low.ctx.append_basic_block(func, "exc_landing");
            b.build_conditional_branch(is_throw, landing, block_of(idx + 1))
                .map_err(|e| e.to_string())?;
            // Landing: bind the caught value, then enter the handler body.
            b.position_at_end(landing);
            let caught = low.exc_value();
            low.store(*caught_reg, caught);
            b.build_unconditional_branch(block_of(target(*off)))
                .map_err(|e| e.to_string())?;
            Ok(true)
        }
        PopHandler => {
            low.pop_frame();
            Ok(false)
        }

        // ── Control flow ──────────────────────────────────────────────────
        Jump(off) => {
            b.build_unconditional_branch(block_of(target(*off)))
                .map_err(|e| e.to_string())?;
            Ok(true)
        }
        JumpIfFalse(r, off) => {
            let cond = low.untag_bool(low.load(*r));
            // Jump to target when false; fall through (idx+1) when true.
            b.build_conditional_branch(cond, block_of(idx + 1), block_of(target(*off)))
                .map_err(|e| e.to_string())?;
            Ok(true)
        }
        JumpIfTrue(r, off) => {
            let cond = low.untag_bool(low.load(*r));
            b.build_conditional_branch(cond, block_of(target(*off)), block_of(idx + 1))
                .map_err(|e| e.to_string())?;
            Ok(true)
        }
        Return(opt) => {
            let v = match opt {
                Some(r) => low.load(*r),
                None => i64_ty.const_int(NIL, false),
            };
            // Transfer the returned reference to the caller: retain it, then the
            // scope-exit release (which decrefs the source slot) nets an ownership
            // move rather than a free of a value the caller now holds.
            low.incref(v);
            low.emit_scope_exit();
            b.build_return(Some(&v)).map_err(|e| e.to_string())?;
            Ok(true)
        }
        // Program terminator (the VM breaks its dispatch loop). A lowered
        // chunk-function ends by returning nil.
        Halt => {
            low.emit_scope_exit();
            b.build_return(Some(&i64_ty.const_int(NIL, false)))
                .map_err(|e| e.to_string())?;
            Ok(true)
        }

        // ── Function values (first-class: boxed function pointers) ────────
        // Materialize `jf_<uid>` as a callable value (used by escapes / indirect
        // calls; a devirtualized direct call ignores it). A closure is just a
        // plain function here — it captures only globals, read via GetGlobal.
        LoadFn(d, idx) | MakeClosure(d, idx) => {
            match fnctx.uid_of(fn_defs, *idx) {
                Some(uid) => low.store(*d, low.fn_box_word(uid, fnctx.funcs[uid])),
                None => return Err(format!("lower.rs: unknown fn_def index {idx}")),
            }
            Ok(false)
        }

        // ── Calls ─────────────────────────────────────────────────────────
        // Direct call to a known function (devirtualized), an indirect call
        // through a runtime function value, or a devirtualized builtin. A call to
        // an unlowered reserved builtin already declined in `resolve_user_calls`.
        Call(dest, callee, args) => {
            match user_calls.get(&idx) {
                Some(CallKind::Direct { uid, args: dargs }) => {
                    let f = fnctx.funcs[*uid];
                    let cf = &fnctx.defs[*uid];
                    let mut argv: Vec<BasicMetadataValueEnum> = Vec::with_capacity(cf.params.len());
                    for a in dargs {
                        argv.push(low.load(*a).into());
                    }
                    for j in dargs.len()..cf.params.len() {
                        let dv = cf.defaults[j]
                            .as_ref()
                            .ok_or("lower.rs: missing default at call site")?;
                        argv.push(low.default_word(dv)?.into());
                    }
                    let ret = b
                        .build_call(f, &argv, "callret")
                        .map_err(|e| e.to_string())?
                        .as_any_value_enum()
                        .into_int_value();
                    low.store(*dest, ret);
                    return Ok(false);
                }
                Some(CallKind::StreamCall { prompt, grammar }) => {
                    emit_stream_call(low, *dest, *prompt, *grammar)?;
                    return Ok(false);
                }
                // A unique struct method → direct call with the receiver as `self`
                // (param 0), the explicit args next, then omitted trailing defaults.
                Some(CallKind::MethodDirect { uid, self_reg, args: margs }) => {
                    let f = fnctx.funcs[*uid];
                    let cf = &fnctx.defs[*uid];
                    let mut argv: Vec<BasicMetadataValueEnum> = Vec::with_capacity(cf.params.len());
                    argv.push(low.load(*self_reg).into());
                    for a in margs {
                        argv.push(low.load(*a).into());
                    }
                    for j in (1 + margs.len())..cf.params.len() {
                        let dv = cf.defaults[j]
                            .as_ref()
                            .ok_or("lower.rs: missing method default at call site")?;
                        argv.push(low.default_word(dv)?.into());
                    }
                    let ret = b
                        .build_call(f, &argv, "mcallret")
                        .map_err(|e| e.to_string())?
                        .as_any_value_enum()
                        .into_int_value();
                    low.store(*dest, ret);
                    return Ok(false);
                }
                Some(CallKind::ModuleCall { module, method, args: margs }) => {
                    let ret = emit_module_call(low, module, method, margs)?;
                    low.store(*dest, ret);
                    return Ok(false);
                }
                Some(CallKind::NativeCall { pkgid, fname, args: margs }) => {
                    let ret = emit_native_call(low, *pkgid, fname, margs)?;
                    low.store(*dest, ret);
                    return Ok(false);
                }
                Some(CallKind::PrimStrMethod { recv, method, args: margs }) => {
                    let ret = emit_str_method(low, *recv, method, margs)?;
                    low.store(*dest, ret);
                    return Ok(false);
                }
                Some(CallKind::PrimValMethod { recv, method, args: margs }) => {
                    let ret = emit_val_method(low, *recv, method, margs)?;
                    low.store(*dest, ret);
                    return Ok(false);
                }
                Some(CallKind::MethodDynamic { recv, method, args: margs }) => {
                    let ret = emit_dynamic_method(low, *recv, method, margs)?;
                    low.store(*dest, ret);
                    return Ok(false);
                }
                Some(CallKind::Indirect) => {
                    let ret = low.indirect_call(*callee, args)?;
                    low.store(*dest, ret);
                    return Ok(false);
                }
                // A plain Call is never resolved as DirectNamed/Spawn.
                Some(CallKind::DirectNamed { .. }) | Some(CallKind::Spawn { .. }) | None => {}
            }
            match call_builtins.get(&idx) {
                Some(bc) if bc.name == "print" => {
                    let arg = low.load(bc.args[0]);
                    low.print_value(arg, *dest)?;
                    Ok(false)
                }
                // str(x) → tagged string via jrt_str_of_any (VM-faithful
                // value_to_display for scalars/strings; an object arg renders the
                // runtime's placeholder, but such programs construct the object
                // and fall back before reaching here).
                Some(bc) if bc.name == "str" => {
                    let p = low.str_of_any(bc.args[0]);
                    low.store(*dest, low.tag_str(p));
                    Ok(false)
                }
                // int(x)/float(x)/bool(x) → tag-dispatching runtime conversions
                // (VM-faithful; int/float raise a catchable error on bad input).
                Some(bc) if matches!(bc.name, "int" | "float" | "bool") => {
                    let fname = match bc.name {
                        "int" => "jrt_int_any",
                        "float" => "jrt_float_any",
                        _ => "jrt_bool_any",
                    };
                    let f = low.runtime_fn(fname, i64_ty.fn_type(&[i64_ty.into()], false));
                    let arg = low.load(bc.args[0]);
                    let r = b
                        .build_call(f, &[arg.into()], "conv")
                        .map_err(|e| e.to_string())?
                        .as_any_value_enum()
                        .into_int_value();
                    low.store(*dest, r);
                    Ok(false)
                }
                // len(x) → jrt_len_chunk (tag-dispatched: strlen for a string, the
                // shared ObjHeader.len for a kind-tagged collection) → tag as int.
                // Collections now lower on the Chunk path (MakeArray/MakeDict), so
                // len() over them must read the header count — jrt_len_unknown reads
                // the legacy offset-8 length and would return the kind byte here.
                Some(bc) if bc.name == "len" => {
                    let f = low.runtime_fn("jrt_len_chunk", i64_ty.fn_type(&[i64_ty.into()], false));
                    let arg = low.load(bc.args[0]);
                    let count = b
                        .build_call(f, &[arg.into()], "len")
                        .map_err(|e| e.to_string())?
                        .as_any_value_enum()
                        .into_int_value();
                    low.store(*dest, low.tag_int(count));
                    Ok(false)
                }
                _ => Err(format!("lower.rs: unsupported call at {idx}")),
            }
        }

        // ── Keyword-argument calls (direct to a known function only) ──────
        CallNamed(dest, _callee, _pairs) => match user_calls.get(&idx) {
            Some(CallKind::DirectNamed { uid, arg_slots }) => {
                let f = fnctx.funcs[*uid];
                let cf = &fnctx.defs[*uid];
                let mut argv: Vec<BasicMetadataValueEnum> = Vec::with_capacity(arg_slots.len());
                for (i, slot) in arg_slots.iter().enumerate() {
                    let w = match slot {
                        Some(r) => low.load(*r),
                        None => {
                            let dv = cf.defaults[i]
                                .as_ref()
                                .ok_or("lower.rs: missing default in keyword call")?;
                            low.default_word(dv)?
                        }
                    };
                    argv.push(w.into());
                }
                let ret = b
                    .build_call(f, &argv, "callret")
                    .map_err(|e| e.to_string())?
                    .as_any_value_enum()
                    .into_int_value();
                low.store(*dest, ret);
                Ok(false)
            }
            Some(CallKind::StreamCall { prompt, grammar }) => {
                emit_stream_call(low, *dest, *prompt, *grammar)?;
                Ok(false)
            }
            // A keyword module call pre-resolved to a module call (fs.read trust).
            Some(CallKind::ModuleCall { module, method, args: margs }) => {
                let ret = emit_module_call(low, module, method, margs)?;
                low.store(*dest, ret);
                Ok(false)
            }
            _ => Err(format!("lower.rs: unsupported keyword call at {idx}")),
        },

        // Everything else is added in later bricks; until then the daemon falls
        // back to the legacy lowering for programs that use it.
        other => Err(format!("lower.rs: unsupported opcode {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::Instr::*;

    fn ir_of(code: &[Instr], n_slots: u32) -> String {
        let context = Context::create();
        let module = context.create_module("t");
        lower_chunk(&context, &module, "f", code, n_slots).expect("lowering failed");
        // Verify catches malformed IR (unterminated blocks, type errors) — the
        // real correctness gate for a lowering brick before it's wired up.
        module.verify().expect("module failed LLVM verification");
        module.print_to_string().to_string()
    }

    #[test]
    fn arithmetic_lowers_to_add_and_ret() {
        // r2 = r0 + r1 ; return r2   with r0=2, r1=3
        let ir = ir_of(
            &[LoadInt(0, 2), LoadInt(1, 3), AddInt(2, 0, 1), Return(Some(2))],
            3,
        );
        assert!(ir.contains("alloca i64"), "slots allocated:\n{ir}");
        assert!(ir.contains(" add "), "native add emitted:\n{ir}");
        assert!(ir.contains("ret i64"), "returns a value word:\n{ir}");
    }

    #[test]
    fn conditional_lowers_to_condbr() {
        // if r0 { return 1 } else { return 2 }
        let ir = ir_of(
            &[
                LoadBool(0, true),
                JumpIfFalse(0, 2), // → idx 4
                LoadInt(1, 1),
                Return(Some(1)),
                LoadInt(1, 2),
                Return(Some(1)),
            ],
            2,
        );
        assert!(ir.contains("br i1"), "conditional branch emitted:\n{ir}");
        // Two distinct return sites.
        assert_eq!(ir.matches("ret i64").count(), 2, "two returns:\n{ir}");
    }

    #[test]
    fn every_block_is_terminated() {
        // Backward loop: must still produce valid (terminated) blocks.
        let ir = ir_of(&[LoadInt(0, 0), Jump(-1)], 1);
        // A well-formed module verifies; unterminated blocks would fail printing
        // to be verifiable, so a non-empty IR with a branch is our smoke signal.
        assert!(ir.contains("br label"));
    }

    #[test]
    fn float_arithmetic_boxes_and_unboxes() {
        // r2 = r0 + r1 (floats) ; return r2
        let ir = ir_of(
            &[LoadFloat(0, 2.5), LoadFloat(1, 1.5), AddFloat(2, 0, 1), Return(Some(2))],
            3,
        );
        assert!(ir.contains("jrt_box_float"), "boxes floats:\n{ir}");
        assert!(ir.contains("jrt_unbox_float"), "unboxes operands:\n{ir}");
        assert!(ir.contains("fadd"), "native fadd emitted:\n{ir}");
        // The runtime symbols are declared exactly once each.
        assert_eq!(ir.matches("declare i64 @jrt_box_float").count(), 1, "one box decl:\n{ir}");
        assert_eq!(ir.matches("declare double @jrt_unbox_float").count(), 1, "one unbox decl:\n{ir}");
    }

    #[test]
    fn int_to_float_widens_then_boxes() {
        // r1 = float(r0) ; return r1   with r0 = 3
        let ir = ir_of(&[LoadInt(0, 3), IntToFloat(1, 0), Return(Some(1))], 2);
        assert!(ir.contains("sitofp"), "signed int→float conversion:\n{ir}");
        assert!(ir.contains("jrt_box_float"), "result boxed:\n{ir}");
    }

    #[test]
    fn string_literal_and_concat() {
        // r2 = "ab" + "cd" ; return r2
        let ir = ir_of(
            &[
                LoadStr(0, "ab".to_string()),
                LoadStr(1, "cd".to_string()),
                ConcatStr(2, 0, 1),
                Return(Some(2)),
            ],
            3,
        );
        // Two pre-tagged, 8-aligned internal literal globals.
        assert_eq!(ir.matches("str_lit_t").count() >= 2, true, "two literal globals:\n{ir}");
        assert!(ir.contains("align 8"), "literal globals 8-aligned:\n{ir}");
        // The literal payload keeps a 7-byte pad + trust header before the bytes.
        assert!(ir.contains("jrt_str_concat"), "concat via runtime:\n{ir}");
        // Words carry the STRING tag: an `or …, 5` after ptrtoint.
        assert!(ir.contains("ptrtoint"), "pointer tagged into a word:\n{ir}");
    }

    #[test]
    fn int_div_guards_zero_divisor_with_a_raise() {
        // r2 = r0 / r1 ; return r2
        let ir = ir_of(
            &[LoadInt(0, 6), LoadInt(1, 2), DivInt(2, 0, 1), Return(Some(2))],
            3,
        );
        assert!(ir.contains("sdiv"), "native signed div:\n{ir}");
        assert!(ir.contains("divzero_throw"), "a throw block guards the divisor:\n{ir}");
        assert!(ir.contains("jade_exc_throw_typed"), "raises on zero divisor:\n{ir}");
        assert!(ir.contains("unreachable"), "throw path is noreturn:\n{ir}");
    }

    #[test]
    fn mod_uses_srem() {
        let ir = ir_of(
            &[LoadInt(0, 7), LoadInt(1, 3), ModInt(2, 0, 1), Return(Some(2))],
            3,
        );
        assert!(ir.contains("srem"), "native signed remainder:\n{ir}");
        assert!(ir.contains("divzero_throw"), "modulo also guards zero:\n{ir}");
    }

    #[test]
    fn raise_throws_and_terminates() {
        // raise "boom"
        let ir = ir_of(&[LoadStr(0, "boom".to_string()), Raise(0)], 1);
        assert!(ir.contains("jade_exc_throw_typed"), "raises the value:\n{ir}");
        assert!(ir.contains("unreachable"), "raise terminates its block:\n{ir}");
    }

    #[test]
    fn try_catch_lowers_to_setjmp_frame() {
        // 0: SetupHandler(caught=r1, →4)   1: LoadInt r0,1 (try body)
        // 2: PopHandler                    3: Jump →5 (skip handler)
        // 4: Move r0,r1 (handler body)     5: Halt
        let ir = ir_of(
            &[
                SetupHandler(1, 3),
                LoadInt(0, 1),
                PopHandler,
                Jump(1),
                Move(0, 1),
                Halt,
            ],
            2,
        );
        assert!(ir.contains("jade_exc_push_frame"), "frame registered:\n{ir}");
        assert!(ir.contains("call i32 @setjmp"), "setjmp split:\n{ir}");
        assert!(ir.contains("returns_twice"), "setjmp marked returns_twice:\n{ir}");
        assert!(ir.contains("jade_exc_pop"), "clean exit pops frame:\n{ir}");
        assert!(ir.contains("jade_exc_value"), "landing binds the caught value:\n{ir}");
        assert!(ir.contains("exc_landing"), "distinct landing block:\n{ir}");
    }

    #[test]
    fn globals_load_and_store_a_named_cell() {
        // x = 5 ; return x     (SetGlobal then GetGlobal)
        let ir = ir_of(
            &[
                LoadInt(0, 5),
                SetGlobal("x".to_string(), 0),
                GetGlobal(1, "x".to_string()),
                Return(Some(1)),
            ],
            2,
        );
        // One internal global cell named for the variable, nil-initialized.
        assert!(ir.contains("@jgl_x"), "named global cell emitted:\n{ir}");
        assert_eq!(ir.matches("@jgl_x = internal global").count(), 1, "one cell, reused:\n{ir}");
    }

    #[test]
    fn locals_are_moves_within_the_slot_array() {
        // GetLocal/SetLocal shuffle slots; must verify and touch both slots.
        let ir = ir_of(
            &[LoadInt(0, 7), SetLocal(1, 0), GetLocal(2, 1), Return(Some(2))],
            3,
        );
        assert!(ir.contains("ret i64"), "returns a word:\n{ir}");
    }

    #[test]
    fn unsupported_opcode_is_reported_not_panicked() {
        let context = Context::create();
        let module = context.create_module("t");
        // `ImportFile` is resolved away before lowering, so it never reaches the
        // backend in a real chunk — a clean "unsupported opcode" Err, not a panic.
        let err = lower_chunk(&context, &module, "f", &[ImportFile("a".into(), "b".into())], 1)
            .unwrap_err();
        assert!(err.contains("unsupported opcode"), "got: {err}");
    }

    #[test]
    fn typed_comparisons_lower_to_native_icmp_fcmp() {
        // r2 = (r0 < r1) int ; r5 = (r3 < r4) float ; return via bool words
        let ir = ir_of(
            &[
                LoadInt(0, 1),
                LoadInt(1, 2),
                CmpLtInt(2, 0, 1),
                LoadFloat(3, 1.0),
                LoadFloat(4, 2.0),
                CmpLtFloat(5, 3, 4),
                Return(Some(2)),
            ],
            6,
        );
        assert!(ir.contains("icmp slt"), "signed int compare:\n{ir}");
        assert!(ir.contains("fcmp olt"), "ordered float compare:\n{ir}");
        assert!(ir.contains("select i1"), "bool word materialized:\n{ir}");
    }

    #[test]
    fn print_devirtualizes_to_jrt_print_any() {
        // GetGlobal print ; LoadInt r1,5 ; Call r2 = print(r1) ; Halt
        let ir = ir_of(
            &[
                GetGlobal(0, "print".to_string()),
                LoadInt(1, 5),
                Call(2, 0, vec![1]),
                Halt,
            ],
            3,
        );
        assert!(ir.contains("jrt_print_any"), "print devirtualized to runtime:\n{ir}");
    }

    // Build a two-fn program by hand: top defines `add(a, b)` and calls it.
    fn add_program() -> Chunk {
        use std::sync::Arc;
        // fn add(a, b): return a + b   (slots 0=a, 1=b, 2=sum)
        let body = vec![AddInt(2, 0, 1), Return(Some(2))];
        let add_fn = Arc::new(CompiledFn {
            params: vec!["a".to_string(), "b".to_string()],
            defaults: vec![None, None],
            chunk: Chunk { name: "add".into(), code: body, spans: vec![], fn_defs: vec![] },
            n_slots: 3,
            source_file: String::new(),
            module_scope: None,
        });
        // top:  LoadFn r0 add ; SetGlobal add r0 ;
        //       GetGlobal r1 add ; LoadInt r2 2 ; LoadInt r3 3 ;
        //       Call r4 = r1(r2, r3) ; Halt
        let mut top = Chunk::new("<top>");
        top.fn_defs.push(add_fn);
        top.code = vec![
            LoadFn(0, 0),
            SetGlobal("add".into(), 0),
            GetGlobal(1, "add".into()),
            LoadInt(2, 2),
            LoadInt(3, 3),
            Call(4, 1, vec![2, 3]),
            Halt,
        ];
        top
    }

    #[test]
    fn user_function_lowers_to_a_direct_call() {
        let context = Context::create();
        let module = context.create_module("t");
        let top = add_program();
        lower_program(&context, &module, &top, 5, &HashMap::new(), &HashMap::new()).expect("program lowering failed");
        module.verify().expect("module failed verification");
        let ir = module.print_to_string().to_string();
        // The function body is its own LLVM function taking two i64 params.
        assert!(ir.contains("define i64 @jf_0(i64"), "fn lowered with params:\n{ir}");
        // The top-level call is a *direct* call to it (devirtualized), not indirect.
        assert!(ir.contains("call i64 @jf_0("), "direct call emitted:\n{ir}");
        assert!(ir.contains("@jade_toplevel"), "top-level fn present:\n{ir}");
    }

    #[test]
    fn call_with_omitted_default_is_filled_at_the_call_site() {
        use std::sync::Arc;
        // fn greet(n = 5): return n     ; call greet() with no args → fills 5
        let body = vec![GetLocal(1, 0), Return(Some(1))];
        let greet = Arc::new(CompiledFn {
            params: vec!["n".to_string()],
            defaults: vec![Some(VmValue::Int(5))],
            chunk: Chunk { name: "greet".into(), code: body, spans: vec![], fn_defs: vec![] },
            n_slots: 2,
            source_file: String::new(),
            module_scope: None,
        });
        let mut top = Chunk::new("<top>");
        top.fn_defs.push(greet);
        top.code = vec![
            LoadFn(0, 0),
            SetGlobal("greet".into(), 0),
            GetGlobal(1, "greet".into()),
            Call(2, 1, vec![]), // no args → default 5
            Halt,
        ];
        let context = Context::create();
        let module = context.create_module("t");
        lower_program(&context, &module, &top, 3, &HashMap::new(), &HashMap::new()).expect("lowering failed");
        module.verify().expect("verification failed");
        let ir = module.print_to_string().to_string();
        // Default 5 materialized as a tagged int (5<<1 = 10) passed to the call.
        assert!(ir.contains("call i64 @jf_0(i64 10)"), "default filled as 10:\n{ir}");
    }

    #[test]
    fn function_value_is_first_class_and_returnable() {
        use std::sync::Arc;
        // A program that *returns* a function value now succeeds — the value is a
        // boxed function pointer (a global `@jf_box_0`), not a decline.
        let f = Arc::new(CompiledFn {
            params: vec![],
            defaults: vec![],
            chunk: Chunk { name: "f".into(), code: vec![Return(None)], spans: vec![], fn_defs: vec![] },
            n_slots: 0,
            source_file: String::new(),
            module_scope: None,
        });
        let mut top = Chunk::new("<top>");
        top.fn_defs.push(f);
        top.code = vec![LoadFn(0, 0), Return(Some(0))];
        let context = Context::create();
        let module = context.create_module("t");
        lower_program(&context, &module, &top, 1, &HashMap::new(), &HashMap::new()).expect("first-class fn value should lower");
        let ir = module.print_to_string().to_string();
        assert!(ir.contains("@jf_box_0"), "boxed function pointer global emitted:\n{ir}");
    }

    #[test]
    fn keyword_call_reorders_args_to_parameter_order() {
        use std::sync::Arc;
        // fn f(a, b, c): return a   ; call f(r1, c=r3, b=r2) — named args reorder.
        let f = Arc::new(CompiledFn {
            params: vec!["a".into(), "b".into(), "c".into()],
            defaults: vec![None, None, None],
            chunk: Chunk { name: "f".into(), code: vec![GetLocal(3, 0), Return(Some(3))], spans: vec![], fn_defs: vec![] },
            n_slots: 4,
            source_file: String::new(),
            module_scope: None,
        });
        let mut top = Chunk::new("<top>");
        top.fn_defs.push(f);
        top.code = vec![
            LoadFn(0, 0),
            SetGlobal("f".into(), 0),
            GetGlobal(1, "f".into()),
            LoadInt(2, 1),
            LoadInt(3, 3),
            LoadInt(4, 2),
            // f(a=r2, c=r3, b=r4)  → positional r2 for a, named c=r3, named b=r4
            CallNamed(5, 1, vec![(None, 2), (Some("c".into()), 3), (Some("b".into()), 4)]),
            Return(Some(5)),
        ];
        let context = Context::create();
        let module = context.create_module("t");
        lower_program(&context, &module, &top, 6, &HashMap::new(), &HashMap::new()).expect("keyword call lowering");
        module.verify().expect("module failed verification");
        let ir = module.print_to_string().to_string();
        // A direct call to jf_0 with three i64 args (reordered to a, b, c).
        assert!(ir.contains("call i64 @jf_0(i64"), "direct call with reordered args:\n{ir}");
    }

    #[test]
    fn higher_order_call_lowers_to_indirect_call() {
        use std::sync::Arc;
        // fn apply(f, x): return f(x)   — f is a param, so f(x) is an indirect call.
        //   slots: 0=f, 1=x, 2=result
        let apply_body = vec![GetLocal(3, 0), GetLocal(4, 1), Call(2, 3, vec![4]), Return(Some(2))];
        let apply = Arc::new(CompiledFn {
            params: vec!["f".into(), "x".into()],
            defaults: vec![None, None],
            chunk: Chunk { name: "apply".into(), code: apply_body, spans: vec![], fn_defs: vec![] },
            n_slots: 5,
            source_file: String::new(),
            module_scope: None,
        });
        let mut top = Chunk::new("<top>");
        top.fn_defs.push(apply);
        top.code = vec![
            LoadFn(0, 0),
            SetGlobal("apply".into(), 0),
            Halt,
        ];
        let context = Context::create();
        let module = context.create_module("t");
        lower_program(&context, &module, &top, 1, &HashMap::new(), &HashMap::new()).expect("higher-order lowering");
        module.verify().expect("module failed verification");
        let ir = module.print_to_string().to_string();
        // apply's body calls its parameter indirectly (a load then a call of a ptr).
        assert!(ir.contains("call i64 %fnld") || ir.contains("%icall"), "indirect call emitted:\n{ir}");
    }

    #[test]
    fn fstring_folds_parts_with_concat() {
        // f"n={r0}"  →  concat("n=", str_of_any(r0))
        let ir = ir_of(
            &[
                LoadInt(0, 42),
                BuildFStr(1, vec![FStrPart::Literal("n=".to_string()), FStrPart::Reg(0)]),
                Return(Some(1)),
            ],
            2,
        );
        assert!(ir.contains("jrt_str_of_any"), "interpolated part rendered:\n{ir}");
        assert!(ir.contains("jrt_str_concat"), "parts folded via concat:\n{ir}");
    }

    #[test]
    fn array_make_index_and_set_lower_to_kind_runtime() {
        // a = [10, 20]; a[0]; a[1] = 30
        let ir = ir_of(
            &[
                LoadInt(0, 10),
                LoadInt(1, 20),
                MakeArray(2, vec![0, 1]),
                LoadInt(3, 0),
                GetIndex(4, 2, 3),
                LoadInt(5, 1),
                LoadInt(6, 30),
                SetIndex(2, 5, 6),
                Return(Some(4)),
            ],
            7,
        );
        assert!(ir.contains("jrt_karr_new"), "array allocated:\n{ir}");
        assert!(ir.contains("jrt_karr_push"), "elements pushed:\n{ir}");
        assert!(ir.contains("jrt_val_index"), "GetIndex via runtime dispatch:\n{ir}");
        assert!(ir.contains("jrt_val_set_index"), "SetIndex via runtime dispatch:\n{ir}");
        // The array word carries the non-string heap tag (`or …, 1`).
        assert!(ir.contains("tagptr"), "array pointer tagged TAG_PTR:\n{ir}");
    }

    /// A native fn value's layout is `{ sentinel@0, ObjKind::Fn@8, env@16 }`.
    ///
    /// The kind word at offset 8 is what makes the value safe to hand to
    /// `jrt_decref`: without it, offset 8 held the `env` pointer, and a heap
    /// address whose low byte happened to be 2/3/4 would have been read as
    /// Array/Dict/Struct and reclaimed as one. That hazard is why native refs
    /// used to veto refcounting for the entire program.
    ///
    /// Nothing covered this path before, so the slot indices could be changed
    /// silently — a wrong index compiles cleanly and corrupts at runtime.
    #[test]
    fn native_fn_value_carries_an_objkind_at_offset_8() {
        // `let f = <native ref>` — loading the ref as a value (not calling it)
        // is what materializes the box.
        let ir = ir_of(
            &[GetGlobal(0, "__native$0$somefn".into()), Return(Some(0))],
            2,
        );
        // 24-byte box, and the kind constant stored into it.
        assert!(ir.contains("native_fn_val"), "native fn box allocated:\n{ir}");
        assert!(
            ir.contains(&format!("store i64 {OBJKIND_FN}")),
            "ObjKind::Fn written at offset 8 — without it jrt_decref misreads the env pointer as a kind:\n{ir}"
        );
        assert!(ir.contains("tagptr"), "native fn value tagged TAG_PTR:\n{ir}");
    }

    #[test]
    fn async_spawn_await_lower_to_runtime() {
        use std::sync::Arc;
        // async fn f(x): return x   ; fa = spawn f(1); await fa
        let f = Arc::new(CompiledFn {
            params: vec!["x".into()],
            defaults: vec![None],
            chunk: Chunk { name: "f".into(), code: vec![GetLocal(1, 0), Return(Some(1))], spans: vec![], fn_defs: vec![] },
            n_slots: 2,
            source_file: String::new(),
            module_scope: None,
        });
        let mut top = Chunk::new("<top>");
        top.fn_defs.push(f);
        top.code = vec![
            LoadFn(0, 0),
            SetGlobal("f".into(), 0),
            GetGlobal(1, "f".into()),
            LoadInt(2, 1),
            Spawn(3, 1, vec![2]),
            Await(4, 3),
            Return(Some(4)),
        ];
        let context = Context::create();
        let module = context.create_module("t");
        lower_program(&context, &module, &top, 5, &HashMap::new(), &HashMap::new()).expect("async lowering");
        module.verify().expect("module failed verification");
        let ir = module.print_to_string().to_string();
        assert!(ir.contains("@jf_task_0"), "task wrapper emitted:\n{ir}");
        assert!(ir.contains("jade_spawn"), "spawn via runtime:\n{ir}");
        // The word-taking entry point, not the pointer-taking one. Asserting
        // "jade_await" alone would pass either way, since it is a prefix of
        // "jade_await_word" — the test has to name the tagged form to detect a
        // regression back to raw pointers.
        assert!(ir.contains("jade_await_word"), "await takes a tagged word:\n{ir}");
        // A future is a tagged value now, so the spawn result is OR'd with
        // TAG_PTR rather than passed through as a bare pointer integer.
        assert!(ir.contains("tagptr"), "spawn result is TAG_PTR-tagged:\n{ir}");
    }

    #[test]
    fn struct_make_field_and_typename_lower_to_runtime() {
        // p = Point{x: 10}; p.x; p.x = 20; typename(p)
        let ir = ir_of(
            &[
                LoadInt(0, 10),
                MakeStruct(1, "Point".to_string(), vec![("x".to_string(), 0, false)]),
                GetField(2, 1, "x".to_string()),
                LoadInt(3, 20),
                SetField(1, "x".to_string(), 3),
                GetTypeName(4, 1),
                Return(Some(2)),
            ],
            5,
        );
        assert!(ir.contains("jrt_kstruct_new"), "struct allocated:\n{ir}");
        assert!(ir.contains("jrt_kstruct_set"), "fields set:\n{ir}");
        assert!(ir.contains("jrt_get_field"), "field read:\n{ir}");
        assert!(ir.contains("jrt_set_field"), "field written:\n{ir}");
        assert!(ir.contains("jrt_get_type_name"), "type name for typed catch:\n{ir}");
    }

    #[test]
    fn in_operator_lowers_to_runtime_membership() {
        use crate::frontend::ast::BinOpKind;
        // r2 = (r0 in r1) → jrt_in_any → bool word
        let ir = ir_of(
            &[
                LoadStr(0, "x".to_string()),
                LoadStr(1, "xyz".to_string()),
                BinOp(2, BinOpKind::In, 0, 1),
                Return(Some(2)),
            ],
            3,
        );
        assert!(ir.contains("jrt_in_any"), "membership via runtime:\n{ir}");
        assert!(ir.contains("select i1"), "produces a bool word:\n{ir}");
    }

    #[test]
    fn dict_make_and_index_lower_to_kind_runtime() {
        // d = {"k": 1}; d["k"]; d["k"] = 2   (kind-tagged dict, value semantics)
        let ir = ir_of(
            &[
                LoadStr(0, "k".to_string()),
                LoadInt(1, 1),
                MakeDict(2, vec![(0, 1)]),
                LoadStr(3, "k".to_string()),
                GetIndex(4, 2, 3),
                LoadStr(5, "k".to_string()),
                LoadInt(6, 2),
                SetIndex(2, 5, 6),
                Return(Some(4)),
            ],
            7,
        );
        assert!(ir.contains("jrt_kdict_new"), "dict allocated:\n{ir}");
        assert!(ir.contains("jrt_kdict_set"), "entries set:\n{ir}");
        assert!(ir.contains("jrt_val_index"), "index via runtime dispatch:\n{ir}");
        // SetIndex stores the returned container word back (value-semantic copy).
        assert!(ir.contains("jrt_val_set_index"), "set-index via runtime:\n{ir}");
    }

    #[test]
    fn string_comparison_lowers_to_strcmp() {
        // r2 = ("a" < "b")  → strcmp on untagged data pointers, folded to a bool word.
        let ir = ir_of(
            &[
                LoadStr(0, "a".to_string()),
                LoadStr(1, "b".to_string()),
                CmpLtStr(2, 0, 1),
                Return(Some(2)),
            ],
            3,
        );
        assert!(ir.contains("call i32 @strcmp"), "compares via strcmp:\n{ir}");
        assert!(ir.contains("icmp slt"), "folds strcmp result by predicate:\n{ir}");
        assert!(ir.contains("select i1"), "produces a bool word:\n{ir}");
    }

    /// A struct type is not a function, and calling one must be a *named* build
    /// error. It cannot simply be left to fall through: a type name is not a
    /// known user function, so it would classify as an indirect call and jump
    /// through a global cell codegen never assigns for a type name — a silent
    /// miscompile rather than a diagnostic.
    #[test]
    fn calling_a_struct_type_is_a_named_build_error() {
        let mut struct_defs = HashMap::new();
        struct_defs.insert(
            "City".to_string(),
            vec![StructFieldDef::Required("name".to_string())],
        );
        let mut top = Chunk::new("<top>");
        top.code = vec![
            MakeDict(0, vec![]),
            GetGlobal(1, "City".to_string()),
            Call(2, 1, vec![0]),
            Halt,
        ];
        let context = Context::create();
        let module = context.create_module("t");
        let err = match lower_program(&context, &module, &top, 3, &struct_defs, &HashMap::new()) {
            Err(e) => e,
            Ok(_) => panic!("calling a struct type should decline"),
        };
        assert!(err.contains("City"), "the error names the type: {err}");
        assert!(err.contains("not a function"), "the error explains why: {err}");
    }

    #[test]
    fn conversion_builtins_devirtualize_to_runtime() {
        // int("42")  →  jrt_int_any
        let ir = ir_of(
            &[
                GetGlobal(0, "int".to_string()),
                LoadStr(1, "42".to_string()),
                Call(2, 0, vec![1]),
                Return(Some(2)),
            ],
            3,
        );
        assert!(ir.contains("jrt_int_any"), "int() lowered to runtime conversion:\n{ir}");
        // bool(x) and float(x) route to their own helpers.
        let ir2 = ir_of(
            &[GetGlobal(0, "bool".to_string()), LoadInt(1, 1), Call(2, 0, vec![1]), Return(Some(2))],
            3,
        );
        assert!(ir2.contains("jrt_bool_any"), "bool() lowered:\n{ir2}");
    }

    #[test]
    fn str_builtin_devirtualizes_to_str_of_any() {
        // GetGlobal str ; LoadInt r1,42 ; Call r2 = str(r1) ; Return r2
        let ir = ir_of(
            &[
                GetGlobal(0, "str".to_string()),
                LoadInt(1, 42),
                Call(2, 0, vec![1]),
                Return(Some(2)),
            ],
            3,
        );
        assert!(ir.contains("jrt_str_of_any"), "str() lowered to runtime render:\n{ir}");
    }

    #[test]
    fn print_falls_back_when_shadowed_by_a_user_global() {
        // If the program SetGlobals `print`, it is a user value, not the builtin
        // → the Call must NOT devirtualize (stays unsupported → fallback).
        let context = Context::create();
        let module = context.create_module("t");
        let err = lower_chunk(
            &context,
            &module,
            "f",
            &[
                LoadInt(0, 1),
                SetGlobal("print".to_string(), 0),
                GetGlobal(1, "print".to_string()),
                LoadInt(2, 5),
                Call(3, 1, vec![2]),
                Halt,
            ],
            4,
        )
        .unwrap_err();
        assert!(err.contains("unsupported call"), "got: {err}");
    }
}
