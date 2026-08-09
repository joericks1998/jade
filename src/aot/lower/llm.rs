//! Prompt values and dereferences, including streaming.
//!
//! Split out of the former monolithic `lower.rs`; see this directory's README.

use super::*;

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
pub(super) fn emit_stream_call<'ctx>(
    low: &Lowerer<'_, 'ctx>,
    dest: Reg,
    prompt: Reg,
    grammar: Option<Reg>,
) -> Result<(), String> {
    let b = low.builder;
    let ptrt = low.ptrt();
    // The daemon owns model selection now: send an empty model, and it uses its
    // configured/loaded model. (The `llm.model()` introspection was removed.)
    let model = low.cstr("");
    // Unwrap: the slot holds a prompt object, not the bare string it wraps.
    let prompt_ptr = low.prompt_text_ptr(prompt);
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

impl<'a, 'ctx> Lowerer<'a, 'ctx> {
    /// The prompt text inside slot `r`, as a raw `char*` for the inference entry
    /// points, which take plain C strings.
    ///
    /// A prompt is a heap object wrapping its tagged string (`promptf.rs`), so the
    /// text has to be unwrapped rather than untagged directly. `jrt_prompt_text`
    /// returns a non-prompt value unchanged, so this stays correct on a path where
    /// the type checker guarantees a prompt but codegen cannot see one.
    pub(super) fn prompt_text_ptr(&self, r: Reg) -> PointerValue<'ctx> {
        let f =
            self.runtime_fn("jrt_prompt_text", self.i64t().fn_type(&[self.i64t().into()], false));
        let text = self
            .builder
            .build_call(f, &[self.load(r).into()], "ptext")
            .unwrap()
            .as_any_value_enum()
            .into_int_value();
        self.untag_ptr(text)
    }
}
