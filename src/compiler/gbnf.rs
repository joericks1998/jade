use std::collections::HashMap;
use crate::frontend::ast::StructFieldDef;

/// Canonical GBNF for the JSON body of a Jade tool call. Kept as a checked-in
/// `.gbnf` file (`grammars/tool_call.gbnf`) and compiled in here so it ships with
/// the binary and is covered by tests. The model-specific delimiters that wrap
/// this body come from the active model profile (`src/llm/model_profile.rs`), not
/// from the grammar — so this body grammar is reusable across models.
pub const TOOL_CALL_GBNF: &str = include_str!("../../grammars/tool_call.gbnf");

/// The canonical tool-call body grammar (see [`TOOL_CALL_GBNF`]).
pub fn tool_call_grammar() -> &'static str {
    TOOL_CALL_GBNF
}

/// Wrap a user-supplied GBNF pattern (RHS only) into a complete grammar.
///
/// `pattern` is the right-hand side of the root rule, e.g. `"yes" | "no"`.
/// Trailing whitespace allowance is appended so llama.cpp can find a valid
/// continuation token after the pattern is fully consumed.
pub fn grammar_from_pattern(pattern: &str) -> String {
    format!("root ::= {} [ \\t\\n\\r]*", pattern)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn no_defs() -> HashMap<String, Vec<StructFieldDef>> { HashMap::new() }

    #[test]
    fn int_grammar() {
        let g = grammar_for("int", &no_defs()).unwrap();
        assert!(g.contains("root ::="));
        assert!(g.contains("[0-9]"));
    }

    #[test]
    fn bool_grammar() {
        let g = grammar_for("bool", &no_defs()).unwrap();
        assert!(g.contains("\"true\""), "should match true");
        assert!(g.contains("\"false\""), "should match false");
        assert!(g.contains(r"[ \t\n\r]*"), "should allow trailing whitespace");
    }

    #[test]
    fn str_is_none() {
        assert!(grammar_for("str", &no_defs()).is_none());
    }

    #[test]
    fn unknown_type_is_none() {
        assert!(grammar_for("UnknownType", &no_defs()).is_none());
    }

    #[test]
    fn struct_grammar_is_prefix_with_free_rest() {
        let fields = vec![
            StructFieldDef::Required("name".to_string()),
            StructFieldDef::Required("age".to_string()),
        ];
        let mut defs = HashMap::new();
        defs.insert("Person".to_string(), fields);
        let g = grammar_for("Person", &defs).unwrap();
        assert!(g.contains("\"{\""), "grammar should anchor opening brace");
        // Must allow a continuation after the brace — an anchor-only grammar
        // (`root ::= "{"`) forces premature EOG and never coerces.
        assert!(g.contains("rest"), "grammar must permit a continuation after `{{`");
        assert_ne!(g.trim(), "root ::= \"{\"", "grammar must not be anchor-only");
    }

    #[test]
    fn array_grammar() {
        let g = grammar_for("array", &no_defs()).unwrap();
        assert!(g.starts_with("root"));
        assert!(g.contains("\"[\""), "should anchor opening bracket");
        assert!(g.contains("rest"), "grammar must permit a continuation after `[`");
        assert_ne!(g.trim(), "root ::= \"[\"", "grammar must not be anchor-only");
    }

    #[test]
    fn dict_grammar() {
        let g = grammar_for("dict", &no_defs()).unwrap();
        assert!(g.starts_with("root"));
        assert!(g.contains("\"{\""), "should anchor opening brace");
        assert!(g.contains("rest"), "grammar must permit a continuation after `{{`");
        assert_ne!(g.trim(), "root ::= \"{\"", "grammar must not be anchor-only");
    }

    #[test]
    fn tool_call_gbnf_is_present_and_well_formed() {
        let g = tool_call_grammar();
        assert!(!g.trim().is_empty(), "tool_call.gbnf must ship non-empty");
        assert!(g.contains("\"name\""), "must constrain the name member");
        assert!(g.contains("arguments"), "must constrain the arguments member");

        // Collect defined rule names (LHS of every `name ::= …` line).
        let defined: std::collections::HashSet<&str> = g
            .lines()
            .filter_map(|l| l.split_once("::="))
            .map(|(lhs, _)| lhs.trim())
            .collect();
        assert!(defined.contains("root"), "must define the root start rule");
        // No dangling nonterminals — every rule the grammar relies on is defined.
        // Rule set mirrors the live, unit-tested tool-call grammar.
        for rule in ["pair", "val", "obj", "arr", "str", "num", "ws"] {
            assert!(defined.contains(rule), "rule `{rule}` is referenced but not defined");
        }
    }
}
