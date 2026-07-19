//! `Grammar` runtime object + grammar-constrained prompt deref for the AOT path.
//!
//! A `Grammar` value (`Grammar.new(pattern, anchor?, stop?)`) is a heap object
//! carrying a GBNF pattern and optional anchor/stop tokens. It only ever flows
//! from `Grammar.new` to a `PromptDeref` (`?p |> g`). Codegen builds it with
//! [`jrt_grammar_new`] and runs the constrained inference with
//! [`jrt_prompt_grammar_obj`], which converts the pattern to GBNF (matching the
//! VM's `gbnf::grammar_from_pattern`) and calls the C inference entry
//! `jrt_prompt_grammar_ex`.
//!
//! The object carries an [`ObjHeader`] with [`ObjKind::Grammar`] (at offset 8),
//! so the refcount ops recognize it as a non-collection and no-op on it — like a
//! function box (see `gc::is_collection`).

use core::ffi::{c_char, c_void};
use std::ffi::CString;

use crate::heap::{ObjHeader, ObjKind};
use crate::sys::strlen;

// The inference entry in the C runtime (`infer/infer.c`); returns a tagged,
// trust-propagated response string, or NULL on error. Does not raise.
unsafe extern "C" {
    fn jrt_prompt_grammar_ex(
        prompt: *const c_char,
        model: *const c_char,
        pattern: *const c_char,
        anchor_or_null: *const c_char,
        stop_or_null: *const c_char,
    ) -> *mut c_char;
}

/// A `Grammar` value: a GBNF/pattern plus optional anchor and stop tokens.
#[repr(C)]
pub struct GrammarObj {
    /// Kind = [`ObjKind::Grammar`].
    pub header: ObjHeader,
    pattern: String,
    anchor: Option<String>,
    stop: Option<String>,
}

/// Borrow a NUL-terminated C string as `String`; NULL → `None` (an omitted
/// optional argument), empty stays empty (an explicit `""`).
unsafe fn opt_str(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    unsafe {
        let n = strlen(p as *const u8);
        Some(String::from_utf8_lossy(core::slice::from_raw_parts(p as *const u8, n)).into_owned())
    }
}

/// `Grammar.new(pattern, anchor?, stop?)` — allocate a Grammar object (leaked,
/// like the collections; the refcount ops no-op on it via its `ObjKind::Grammar`).
/// `anchor`/`stop` may be NULL (omitted). Returns the raw pointer (codegen tags it).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_grammar_new(
    pattern: *const c_char,
    anchor: *const c_char,
    stop: *const c_char,
) -> *mut c_void {
    let obj = GrammarObj {
        header: ObjHeader::new(ObjKind::Grammar, 0),
        pattern: unsafe { opt_str(pattern) }.unwrap_or_default(),
        anchor: unsafe { opt_str(anchor) },
        stop: unsafe { opt_str(stop) },
    };
    crate::gc::leak_obj(obj)
}

/// Run grammar-constrained inference for `prompt`/`model` using the Grammar
/// object `grammar_obj`. Converts a simple pattern to GBNF exactly as the VM's
/// `PromptDeref` does (a pattern that already looks like GBNF — has a `root`
/// rule — is used verbatim), then calls the C `jrt_prompt_grammar_ex`. Returns
/// its tagged, trust-propagated response string (NULL on error).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_prompt_grammar_obj(
    prompt: *const c_char,
    model: *const c_char,
    grammar_obj: *const c_void,
) -> *mut c_char {
    let g = unsafe { &*(grammar_obj as *const GrammarObj) };

    // Mirror the VM: a complete GBNF (has `root ::=`) is used as-is; a bare
    // pattern is wrapped. Keep the wrapper byte-identical to
    // `gbnf::grammar_from_pattern`.
    let gbnf = if g.pattern.contains("root") && g.pattern.contains("::=") {
        g.pattern.clone()
    } else {
        format!("root ::= {} [ \\t\\n\\r]*", g.pattern)
    };

    let gbnf_c = match CString::new(gbnf) {
        Ok(c) => c,
        Err(_) => return core::ptr::null_mut(), // interior NUL — malformed grammar
    };
    let anchor_c = g.anchor.as_deref().and_then(|s| CString::new(s).ok());
    let stop_c = g.stop.as_deref().and_then(|s| CString::new(s).ok());
    let anchor_ptr = anchor_c.as_ref().map_or(core::ptr::null(), |c| c.as_ptr());
    let stop_ptr = stop_c.as_ref().map_or(core::ptr::null(), |c| c.as_ptr());

    unsafe { jrt_prompt_grammar_ex(prompt, model, gbnf_c.as_ptr(), anchor_ptr, stop_ptr) }
}
