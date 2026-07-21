//! The `Grammar` value — shared by both engines.
//!
//! A `Grammar` (`Grammar.new(pattern, anchor?, stop?)`) carries a GBNF pattern
//! and optional anchor/stop tokens. [`GrammarObj`] is the one representation:
//! AOT holds it as a tagged heap pointer built by [`jrt_grammar_new`], the VM
//! holds the same struct behind an `Arc` as `VmValue::Grammar`.
//!
//! It flows from `Grammar.new` to either a `PromptDeref` (`?p |> g`) or a
//! `stream(..., mute_on=[g])`. Every consumer obtains its GBNF from
//! [`GrammarObj::to_gbnf`], so the pattern-vs-complete-grammar decision is made
//! exactly once. AOT runs the constrained inference through
//! [`jrt_prompt_grammar_obj`] → the C entry `jrt_prompt_grammar_ex`.
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

    /// The streaming counterpart: prints tokens as they arrive (honouring the
    /// anchor/stop mute region) and returns the *full* text, muted parts
    /// included. `start_muted` suppresses output from the first token.
    fn jrt_prompt_stream_ex(
        prompt: *const c_char,
        model: *const c_char,
        pattern_or_null: *const c_char,
        anchor_or_null: *const c_char,
        stop_or_null: *const c_char,
        start_muted: i32,
    ) -> *mut c_char;

    /// Write the newline that terminates a `stream()`'s live output. In C so it
    /// shares stdout buffering with the token callback.
    fn jrt_stream_newline();
}

/// A `Grammar` value: a GBNF/pattern plus optional anchor and stop tokens.
///
/// This is the **single** representation of a Jade `Grammar` in both engines.
/// AOT holds it as a tagged heap pointer; the VM holds it behind an `Arc`
/// (`VmValue::Grammar`). The `header` is inert in the VM's case — `Arc` does
/// that refcounting — but keeping one type means the two engines cannot
/// disagree about what a grammar *is*, which is the whole point of the
/// VmValue sunset.
#[repr(C)]
pub struct GrammarObj {
    /// Kind = [`ObjKind::Grammar`].
    pub header: ObjHeader,
    pub pattern: String,
    pub anchor: Option<String>,
    pub stop: Option<String>,
}

/// Wrap a bare GBNF pattern (the right-hand side of the root rule, e.g.
/// `"yes" | "no"`) into a complete grammar.
///
/// The trailing whitespace allowance is required, not cosmetic: once the value
/// is fully emitted llama.cpp needs at least one legal continuation token
/// before it can transition to end-of-generation. Without it the sampler has an
/// empty candidate set and crashes.
pub fn wrap_pattern(pattern: &str) -> String {
    format!("root ::= {} [ \\t\\n\\r]*", pattern)
}

impl GrammarObj {
    /// Build a grammar value. `anchor`/`stop` are optional.
    pub fn new(pattern: String, anchor: Option<String>, stop: Option<String>) -> Self {
        GrammarObj { header: ObjHeader::new(ObjKind::Grammar, 0), pattern, anchor, stop }
    }

    /// The complete GBNF this grammar denotes.
    ///
    /// A pattern that already *is* a grammar (it has a `root` rule) is used
    /// verbatim; a bare pattern is wrapped by [`wrap_pattern`].
    ///
    /// This is the one place that decision is made. It used to be made twice —
    /// once in the VM's `?p |> g` path and once here — and a third consumer,
    /// the VM's `stream(..., mute_on=[g])`, made it *not at all* and passed the
    /// bare pattern straight to the model. The same `Grammar` value therefore
    /// meant different things depending on which builtin received it. Routing
    /// every consumer through this method is what makes that class of bug
    /// unrepresentable rather than merely fixed.
    pub fn to_gbnf(&self) -> String {
        if self.pattern.contains("root") && self.pattern.contains("::=") {
            self.pattern.clone()
        } else {
            wrap_pattern(&self.pattern)
        }
    }
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
    let obj = GrammarObj::new(
        unsafe { opt_str(pattern) }.unwrap_or_default(),
        unsafe { opt_str(anchor) },
        unsafe { opt_str(stop) },
    );
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

    let gbnf_c = match CString::new(g.to_gbnf()) {
        Ok(c) => c,
        Err(_) => return core::ptr::null_mut(), // interior NUL — malformed grammar
    };
    let anchor_c = g.anchor.as_deref().and_then(|s| CString::new(s).ok());
    let stop_c = g.stop.as_deref().and_then(|s| CString::new(s).ok());
    let anchor_ptr = anchor_c.as_ref().map_or(core::ptr::null(), |c| c.as_ptr());
    let stop_ptr = stop_c.as_ref().map_or(core::ptr::null(), |c| c.as_ptr());

    unsafe { jrt_prompt_grammar_ex(prompt, model, gbnf_c.as_ptr(), anchor_ptr, stop_ptr) }
}

/// The constraints a Grammar contributes to a streaming request, in the shape
/// the C streaming entry wants.
///
/// Split out from [`jrt_prompt_stream_grammar_obj`] so the mapping is testable
/// without a running inference daemon — the `start_muted` rule in particular is
/// easy to get backwards, and it is the difference between printing a muted
/// region and hiding a visible one.
fn stream_args(g: &GrammarObj) -> (String, Option<&str>, Option<&str>, bool) {
    (
        g.to_gbnf(),
        g.anchor.as_deref(),
        g.stop.as_deref(),
        // No anchor means "mute from the very first token" — there is no point
        // at which muting would begin, so it begins immediately. With an anchor,
        // output is visible until the anchor appears. Mirrors the VM's
        // `start_muted` in the `stream()` builtin.
        g.anchor.is_none(),
    )
}

/// Streaming inference: the AOT half of `stream(?p)` / `stream(?p, mute_on=[g])`.
///
/// Prints tokens live (suppressing the grammar's mute region, if any) and
/// returns the complete response as a raw string, muted region included — the
/// same contract as the VM's `stream()`, which prints through
/// `vm_drain_token_stream_printing` and returns the full text.
///
/// `grammar_obj` may be NULL, meaning unconstrained and unmuted.
///
/// Sibling of [`jrt_prompt_grammar_obj`], and deliberately built the same way:
/// the pattern comes from [`GrammarObj::to_gbnf`], so the constrained and
/// streaming paths cannot disagree about what a grammar means.
///
/// The trailing newline is emitted here rather than by codegen because it is
/// part of `stream()`'s contract (the VM drains with `newline = true`), and
/// keeping it beside the token output is what stops the two engines differing
/// by a single byte of stdout — the exact class of drift backend-parity exists
/// to catch.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_prompt_stream_obj(
    prompt: *const c_char,
    model: *const c_char,
    grammar_obj: *const c_void,
) -> *mut c_char {
    let nul = core::ptr::null();
    let (gbnf_c, anchor_c, stop_c, start_muted) = if grammar_obj.is_null() {
        (None, None, None, false)
    } else {
        let g = unsafe { &*(grammar_obj as *const GrammarObj) };
        let (gbnf, anchor, stop, muted) = stream_args(g);
        let Ok(gbnf_c) = CString::new(gbnf) else {
            return core::ptr::null_mut(); // interior NUL — malformed grammar
        };
        (
            Some(gbnf_c),
            anchor.and_then(|s| CString::new(s).ok()),
            stop.and_then(|s| CString::new(s).ok()),
            muted,
        )
    };

    let out = unsafe {
        jrt_prompt_stream_ex(
            prompt,
            model,
            gbnf_c.as_ref().map_or(nul, |c| c.as_ptr()),
            anchor_c.as_ref().map_or(nul, |c| c.as_ptr()),
            stop_c.as_ref().map_or(nul, |c| c.as_ptr()),
            start_muted as i32,
        )
    };
    unsafe { jrt_stream_newline() };
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(pattern: &str) -> GrammarObj {
        GrammarObj::new(pattern.to_string(), None, None)
    }

    #[test]
    fn a_bare_pattern_is_wrapped_into_a_root_rule() {
        assert_eq!(g(r#""yes" | "no""#).to_gbnf(), r#"root ::= "yes" | "no" [ \t\n\r]*"#);
    }

    #[test]
    fn a_complete_grammar_is_used_verbatim() {
        let src = "root ::= [0-9]+\n";
        assert_eq!(g(src).to_gbnf(), src);
    }

    // Both halves of the recognizer matter. A pattern mentioning `root` but
    // with no rule arrow is still a bare pattern, and vice versa.
    #[test]
    fn root_without_an_arrow_is_still_a_bare_pattern() {
        assert_eq!(g(r#""root""#).to_gbnf(), r#"root ::= "root" [ \t\n\r]*"#);
    }

    #[test]
    fn an_arrow_without_root_is_still_a_bare_pattern() {
        let out = g("a ::= b").to_gbnf();
        assert_eq!(out, r"root ::= a ::= b [ \t\n\r]*");
    }

    // The trailing whitespace allowance is load-bearing: without a legal
    // continuation token llama.cpp's sampler has an empty candidate set at the
    // end of a match and crashes. Pin it so nobody "tidies" it away.
    #[test]
    fn wrapping_always_allows_a_trailing_whitespace_token() {
        assert!(g("[0-9]+").to_gbnf().ends_with(r"[ \t\n\r]*"));
    }

    // The regression this collapse was built to make unrepresentable: `?p |> g`
    // wrapped the pattern, `stream(mute_on=[g])` passed it through raw, so one
    // Grammar meant two different things. Now there is only one accessor.
    #[test]
    fn every_consumer_sees_the_same_gbnf_for_one_grammar() {
        let anchored = GrammarObj::new(
            r#""yes" | "no""#.to_string(),
            Some("<a>".to_string()),
            Some("</a>".to_string()),
        );
        assert_eq!(anchored.to_gbnf(), g(r#""yes" | "no""#).to_gbnf());
        assert_eq!(anchored.anchor.as_deref(), Some("<a>"));
        assert_eq!(anchored.stop.as_deref(), Some("</a>"));
    }

    // An anchor says "stay visible until you see this", so output starts
    // visible. No anchor means there is nothing to wait for, so muting starts
    // immediately. Getting this backwards silently inverts what the user sees.
    #[test]
    fn an_anchored_grammar_starts_visible() {
        let g = GrammarObj::new("p".into(), Some("<t>".into()), Some("</t>".into()));
        let (_, anchor, stop, start_muted) = stream_args(&g);
        assert_eq!(anchor, Some("<t>"));
        assert_eq!(stop, Some("</t>"));
        assert!(!start_muted);
    }

    #[test]
    fn an_unanchored_grammar_starts_muted() {
        let g = GrammarObj::new("p".into(), None, Some("</t>".into()));
        let (_, anchor, _, start_muted) = stream_args(&g);
        assert_eq!(anchor, None);
        assert!(start_muted);
    }

    // The streaming path takes its pattern from the same accessor as the
    // constrained path, so the two cannot constrain the model differently.
    #[test]
    fn streaming_uses_the_same_gbnf_as_constrained_inference() {
        let g = GrammarObj::new(r#""yes" | "no""#.into(), None, None);
        assert_eq!(stream_args(&g).0, g.to_gbnf());
    }

    #[test]
    fn header_is_tagged_as_a_grammar() {
        assert_eq!(g("x").header.kind, ObjKind::Grammar as u8);
    }
}
