//! String literals, concatenation, comparison, and string methods.
//!
//! See this directory's README.

use super::*;

/// Emit a string primitive method `recv.method(args)` via the shared `jrt_str_*`
/// symbol (the receiver and any args are strings; results are tagged strings or
/// bool words). Only methods `chunk_str_method_supported` accepts reach here.
/// Which argument positions of a string method hold strings.
///
/// The guard this drives untags an argument to `char*`, so pointing it at an
/// int would read a small integer as a pointer — which is why the default is
/// "every position" and the exceptions are listed rather than the other way
/// round.
fn str_arg_positions(method: &str) -> &'static [usize] {
    match method {
        // (width, pad): the pad is a string, the width is not.
        "pad_start" | "pad_end" => &[1],
        // (start, end) and (n): no string arguments at all.
        "slice" | "repeat" => &[],
        _ => &[0, 1, 2],
    }
}

pub(super) fn emit_str_method<'ctx>(
    low: &Lowerer<'_, 'ctx>,
    recv: Reg,
    method: &str,
    args: &[Reg],
) -> Result<IntValue<'ctx>, String> {
    let b = low.builder;
    let ptrt = low.ptrt();
    let i32_ty = low.ctx.i32_type();
    let err = |e: inkwell::builder::BuilderError| e.to_string();

    // Every arm here untags the receiver to `char*`, so it always needs a str.
    low.require_kind(low.load(recv), WANT_STR, method)?;
    // Most arms untag their arguments too, but not all: the ones added in
    // v1.3.23 take a width or a count, and guarding an int as a string would
    // reject every correct call. Only the string-valued positions are checked,
    // which is why this is a per-method list rather than a blanket loop.
    for (i, a) in args.iter().enumerate() {
        if str_arg_positions(method).contains(&i) {
            low.require_str_arg(low.load(*a), method)?;
        }
    }

    let sp = |r: Reg| low.untag_ptr(low.load(r));

    match method {
        // `s.encode()` — the UTF-8 octets as a bytes value. The trust byte at
        // `[-1]` of the string travels with them, so a tainted string encodes
        // to a tainted blob and `decode` gives a tainted string back.
        "encode" => {
            let f = low.runtime_fn("jrt_bytes_encode", ptrt.fn_type(&[ptrt.into()], false));
            let p = b
                .build_call(f, &[sp(recv).into()], "enc")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_ptr(p))
        }
        "trim" | "upper" | "lower" | "trim_start" | "trim_end" | "capitalize" => {
            let f =
                low.runtime_fn(&format!("jrt_str_{method}"), ptrt.fn_type(&[ptrt.into()], false));
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
        // (s, sub) -> a character index or -1. `jrt_str_index_of` and friends
        // answer i64 directly, so there is no bool fold here.
        "index_of" | "last_index_of" | "count" => {
            let f = low.runtime_fn(
                &format!("jrt_str_{method}"),
                low.i64t().fn_type(&[ptrt.into(), ptrt.into()], false),
            );
            let r = b
                .build_call(f, &[sp(recv).into(), sp(args[0]).into()], "stridx")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value();
            Ok(low.tag_int(r))
        }
        "is_empty" => {
            let f = low.runtime_fn("jrt_str_is_empty", i32_ty.fn_type(&[ptrt.into()], false));
            let r = b
                .build_call(f, &[sp(recv).into()], "stre")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value();
            let bit = b
                .build_int_compare(inkwell::IntPredicate::NE, r, i32_ty.const_zero(), "b")
                .map_err(err)?;
            Ok(low.bool_word(bit))
        }
        // (s, start, end) — character indices, so the two bounds are untagged
        // as ints rather than as pointers. `str_arg_positions` is what keeps the
        // receiver guard from rejecting them.
        "slice" => {
            let f = low.runtime_fn(
                "jrt_str_slice",
                ptrt.fn_type(&[ptrt.into(), low.i64t().into(), low.i64t().into()], false),
            );
            let start = low.untag_int(low.load(args[0]));
            let end = low.untag_int(low.load(args[1]));
            let r = b
                .build_call(f, &[sp(recv).into(), start.into(), end.into()], "strsl")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_str(r))
        }
        "repeat" => {
            let f = low.runtime_fn(
                "jrt_str_repeat",
                ptrt.fn_type(&[ptrt.into(), low.i64t().into()], false),
            );
            let n = low.untag_int(low.load(args[0]));
            let r = b
                .build_call(f, &[sp(recv).into(), n.into()], "strrep")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_str(r))
        }
        // (s, width, pad): argument 0 is an int, argument 1 a string.
        "pad_start" | "pad_end" => {
            let f = low.runtime_fn(
                &format!("jrt_str_{method}"),
                ptrt.fn_type(&[ptrt.into(), low.i64t().into(), ptrt.into()], false),
            );
            let w = low.untag_int(low.load(args[0]));
            let r = b
                .build_call(f, &[sp(recv).into(), w.into(), sp(args[1]).into()], "strpad")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_str(r))
        }
        "split" => {
            // (s, sep) -> new array of substrings (tagged ptr).
            let f = low
                .runtime_fn("jrt_coll_str_split", ptrt.fn_type(&[ptrt.into(), ptrt.into()], false));
            let p = b
                .build_call(f, &[sp(recv).into(), sp(args[0]).into()], "split")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_ptr(p))
        }
        _ => Err(format!("codegen: emit_str_method: unhandled {method}")),
    }
}

impl<'a, 'ctx> Lowerer<'a, 'ctx> {
    /// A plain NUL-terminated C string global (for compile-time struct type/field
    /// names passed to the runtime — not a tagged Jade string).
    pub(super) fn cstr(&self, s: &str) -> PointerValue<'ctx> {
        self.builder.build_global_string_ptr(s, "cstr").unwrap().as_pointer_value()
    }

    /// Materialize a compile-time string literal as a TRUSTED tagged-string
    /// global and return its **data pointer** (8-aligned). Layout mirrors
    /// `expr.rs::emit_tagged_literal`: `[7 pad][trust][bytes…][nul]`, so the
    /// data pointer is `global+8` and the trust byte lives at `data[-1]`.
    pub(super) fn str_literal_ptr(&self, s: &str) -> Result<PointerValue<'ctx>, String> {
        let i8_ty = self.ctx.i8_type();
        let i32_ty = self.ctx.i32_type();
        let bytes = s.as_bytes();
        let mut data: Vec<u8> = Vec::with_capacity(bytes.len() + 9);
        // The header's first four bytes are the refcount, and a literal's is the
        // immortal marker. It has to be baked in rather than written at startup:
        // the global is `constant`, so it lives in read-only memory and a store
        // to it would fault. `jade_runtime::string::decref` checks for the
        // marker before touching the word, which is what lets a slot holding a
        // literal be released at scope exit like any other string.
        data.extend_from_slice(&jade_runtime::string::IMMORTAL.to_ne_bytes());
        data.extend_from_slice(&[0u8; 3]);
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
    pub(super) fn str_literal_word(&self, s: &str) -> Result<IntValue<'ctx>, String> {
        let ptr = self.str_literal_ptr(s)?;
        Ok(self.tag_str(ptr))
    }

    /// Concatenate two tagged-string **data pointers** via the shared runtime
    /// `jrt_str_concat` (trust = max of inputs); returns a new data pointer.
    pub(super) fn concat_ptrs(
        &self,
        a: PointerValue<'ctx>,
        b: PointerValue<'ctx>,
    ) -> PointerValue<'ctx> {
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
    pub(super) fn str_concat(&self, l: Reg, r: Reg) -> IntValue<'ctx> {
        let lp = self.untag_ptr(self.load(l));
        let rp = self.untag_ptr(self.load(r));
        self.tag_str(self.concat_ptrs(lp, rp))
    }

    /// Release a tagged-string data pointer this frame owns outright.
    pub(super) fn free_str_ptr(&self, p: PointerValue<'ctx>) {
        let f = self
            .runtime_fn("jrt_str_free", self.ctx.void_type().fn_type(&[self.ptrt().into()], false));
        self.builder.build_call(f, &[p.into()], "").unwrap();
    }

    /// Render a value word to a tagged-string **data pointer** via the runtime's
    /// `jrt_str_of_any` (VM-faithful for scalars/strings; preserves trust). Used
    /// to interpolate an f-string part.
    pub(super) fn str_of_any(&self, r: Reg) -> PointerValue<'ctx> {
        let f =
            self.runtime_fn("jrt_str_of_any", self.ptrt().fn_type(&[self.i64t().into()], false));
        self.builder
            .build_call(f, &[self.load(r).into()], "strofany")
            .unwrap()
            .as_any_value_enum()
            .into_pointer_value()
    }

    /// Guard a str method's argument (`jrt_require_str_arg`), which is untagged
    /// to a `char*` exactly like the receiver and needs the same check.
    pub(super) fn require_str_arg(&self, val: IntValue<'ctx>, method: &str) -> Result<(), String> {
        let f = self.runtime_fn(
            "jrt_require_str_arg",
            self.ctx.void_type().fn_type(&[self.i64t().into(), self.ptrt().into()], false),
        );
        self.builder
            .build_call(f, &[val.into(), self.cstr(method).into()], "")
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Compare two tagged-string words via libc `strcmp` on their data pointers
    /// (mirrors the legacy typed-string path), folding into a bool word. Strings
    /// carry their own tag (`TAG_STR`), so this needs no per-object kind header.
    pub(super) fn str_cmp(&self, l: Reg, r: Reg, pred: IntPredicate) -> IntValue<'ctx> {
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
}
