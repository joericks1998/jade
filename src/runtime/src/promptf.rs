//! `PromptObj` — a prompt value on the AOT heap.
//!
//! A prompt is a *type* in Jade, not a string that happens to be sent to a model:
//! `?p` dereferences it, `value_type_name` calls it `"prompt"`, and printing one
//! shows `<prompt>` rather than its text, the way a future shows `<future>`.
//!
//! The AOT backend used to erase all of that. `MakePrompt` simply moved the
//! underlying string into the destination slot, on the reasoning — written down
//! in `src/codegen/` — that "a prompt only ever flows to `PromptDeref`". It does
//! not. A prompt can be printed, stored in a struct field, passed to a function,
//! or returned from one, and at each of those points a compiled binary saw a
//! string where the VM saw a prompt. The visible half was `print(p)`, which gave
//! `<prompt>` under `jade run` and the raw prompt text from the same program
//! built. The invisible half was that `MakeStruct` had to refuse prompt fields
//! outright, because there was nothing to store that would read back correctly —
//! which is why `examples/structs/prompt_fields` was excluded from the parity
//! gate rather than passing it.
//!
//! So a prompt carries an [`ObjHeader`] like every other non-scalar value. The
//! payload is one word: the tagged string holding the prompt text. `PromptDeref`
//! and the streaming path unwrap it with [`jrt_prompt_text`] before handing the
//! text to the inference entry points, which still take a plain `char*`.
//!
//! The VM needs none of this — it has `VmValue::Prompt(String)` — so unlike
//! `GrammarObj` this type is not shared between the engines. It exists to give
//! the AOT the distinction the VM already had.

use core::ffi::c_void;

use crate::heap::{ObjHeader, ObjKind};

/// A prompt value: a header plus the tagged string word holding its text.
///
/// `repr(C)` and header-first, like every other kind — `gc::free_obj` and the
/// refcount ops read the kind byte at offset 8 before they know what they are
/// looking at.
#[repr(C)]
pub struct PromptObj {
    /// Kind = [`ObjKind::Prompt`].
    pub header: ObjHeader,
    /// Tagged string word. Owned: `free_obj` decrefs it.
    pub text: i64,
}

impl PromptObj {
    pub fn new(text: i64) -> Self {
        PromptObj { header: ObjHeader::new(ObjKind::Prompt, 0), text }
    }
}

/// Box a prompt's text into a prompt value. `text` is a tagged string word; the
/// prompt takes a reference to it. Returns the raw pointer (codegen tags it).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_prompt_new(text: i64) -> *mut c_void {
    crate::gc::jrt_incref(text);
    crate::gc::leak_obj(PromptObj::new(text))
}

/// The tagged string word inside a prompt value.
///
/// Borrowed, not owned: the caller must not free it, and must not hold it past
/// the prompt's own lifetime. Every caller uses it immediately, to pass the text
/// to an inference entry point.
///
/// A non-prompt pointer returns its own argument unchanged, so a caller that was
/// handed a bare string still works. That is deliberate: it keeps the unwrap
/// harmless on the paths where the type checker already guarantees a prompt but
/// codegen cannot see it.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_prompt_text(v: i64) -> i64 {
    let val = crate::value::JadeValue::from_bits(v as u64);
    if !val.is_ptr() {
        return v;
    }
    let p = val.as_ptr();
    let kind = unsafe { (*(p as *const ObjHeader)).kind };
    if kind == ObjKind::Prompt as u8 { unsafe { (*(p as *const PromptObj)).text } } else { v }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::JadeValue;

    fn tagged_text(s: &str) -> i64 {
        let owned = format!("{s}\0");
        let p = crate::ffi::jrt_str_dup(owned.as_ptr(), crate::string::TRUSTED);
        JadeValue::from_str_ptr(p as *const ()).bits() as i64
    }

    #[test]
    fn a_prompt_carries_its_kind_and_gives_its_text_back() {
        let text = tagged_text("hello");
        let obj = PromptObj::new(text);
        assert_eq!(obj.header.kind, ObjKind::Prompt as u8);
        assert_eq!(obj.text, text);
    }

    /// Unwrapping something that is not a prompt hands the value straight back,
    /// so the unwrap is safe to apply on a path that may already hold the text.
    #[test]
    fn unwrapping_a_non_prompt_is_the_identity() {
        let plain = tagged_text("not a prompt");
        assert_eq!(jrt_prompt_text(plain), plain);
        let int_word = JadeValue::from_int(7).bits() as i64;
        assert_eq!(jrt_prompt_text(int_word), int_word);
    }
}
