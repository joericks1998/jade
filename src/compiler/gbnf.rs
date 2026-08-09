use crate::frontend::ast::StructFieldDef;
use std::collections::HashMap;

// The canonical tool-call body GBNF used to live here (`TOOL_CALL_GBNF` +
// `tool_call_grammar()`, backing `llm.tool_grammar()`). Both are gone: tool-call
// shapes are model-specific and now ship with each model's profile, and callers
// that want a grammar constraint build one with the `Grammar` type. What remains
// here is `grammar_for` — the compiler-side generator for typed dereferences.

/// Wrap a user-supplied GBNF pattern (RHS only) into a complete grammar.
///
/// `pattern` is the right-hand side of the root rule, e.g. `"yes" | "no"`.
///
/// The implementation lives in the shared runtime so the VM and the AOT backend
/// cannot wrap patterns differently. This re-export exists because the wrapper
/// is also used for auto-generated grammars below, which are compiler-side.
pub use jade_runtime::grammarf::wrap_pattern as grammar_from_pattern;

/// Build a prefix grammar that pins the first emitted token to `open` (the
/// structure's opening bracket) and then permits any continuation. `rest`
/// matches "zero or more of any byte but NUL", so once the bracket is in place
/// the grammar imposes no further constraint — bounded enforcement at the start,
/// free generation after.
fn prefix_grammar(open: &str) -> String {
    format!("root ::= {:?} rest\nrest ::= [^\\x00]*", open)
}

/// Generate a GBNF grammar string for the given JadeLang type name.
///
/// Primitives (int, float, bool): full grammar — output is short, state count is tiny.
/// Complex types (struct, array, dict): prefix-only grammar — enforces the opening
/// bracket and first key, then `rest ::= [^\x00]*` lets the model generate freely.
/// This bounds expensive grammar-checking to ~5 tokens rather than the full output.
///
/// Returns `None` for `str` (any text is valid) and for unrecognized types.
/// The returned string uses `root` as the start rule, matching llama-cpp-2's
/// `LlamaSampler::grammar(model, gbnf, "root")` call convention.
pub fn grammar_for(
    type_name: &str,
    struct_defs: &HashMap<String, Vec<StructFieldDef>>,
) -> Option<String> {
    match type_name {
        // Trailing [ \t\n\r]* on terminal grammars is required: once the value
        // is fully emitted, llama.cpp needs at least one valid continuation token
        // (whitespace) before it can transition to EOG — without it the sampler
        // has an empty candidate set and crashes.
        "int" => Some(
            r#"root ::= "-"? ("0" | [1-9] [0-9]*) [ \t\n\r]*"#.to_owned()
        ),
        "float" => Some(
            r#"root ::= "-"? ("0" | [1-9] [0-9]*) ("." [0-9]+)? [ \t\n\r]*"#.to_owned()
        ),
        "bool" => Some(
            r#"root ::= ("true" | "false") [ \t\n\r]*"#.to_owned()
        ),
        // One character. `.` in GBNF is any byte, which would let a multi-byte
        // sequence through as several characters, so the class is spelled out:
        // one ASCII byte, or a well-formed 2/3/4-byte UTF-8 sequence.
        "char" => Some(
            r#"root ::= ([^\x00-\x1f] | [\xc2-\xdf] [\x80-\xbf] | [\xe0-\xef] [\x80-\xbf] [\x80-\xbf] | [\xf0-\xf4] [\x80-\xbf] [\x80-\xbf] [\x80-\xbf]) [ \t\n\r]*"#.to_owned()
        ),
        "str" => None,
        // Prefix grammar: force the opening `[` as the first token, then let the
        // model generate the rest freely (`rest ::= [^\x00]*` matches anything).
        //
        // An anchor-only grammar (`root ::= "["`) is a trap: it matches *only*
        // the single opening token with no legal continuation, so the model is
        // forced straight to EOG and emits literally `[`, which never coerces.
        // The daemon only retires a grammar after its anchor when an explicit
        // anchor is sent on the request — these auto-generated grammars carry
        // none, so the grammar stays active for the whole generation and the
        // model never escapes the opening bracket. The free `rest` rule keeps
        // masking effectively zero-cost (every token is permitted) while still
        // guaranteeing the value opens with the right bracket.
        "array" | "Array" => Some(prefix_grammar("[")),
        "dict" | "Dict" => Some(prefix_grammar("{")),
        name => {
            let def = struct_defs.get(name)?;
            let _ = def;
            Some(prefix_grammar("{"))
        }
    }
}

// Tests for this module live in `src/compiler/tests.rs` (`mod gbnf`).
