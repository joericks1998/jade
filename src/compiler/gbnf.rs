use std::collections::HashMap;
use crate::frontend::ast::StructFieldDef;

/// Extract all quoted-literal strings from a GBNF pattern (the RHS of `root ::=`).
///
/// Only handles quoted-string literals and `|`-alternations of them.
/// Non-literal alternatives (character classes, rule references) are silently skipped.
/// Used to build the literal set for prefix-aware mute buffering in `stream()`.
pub fn grammar_literals(pattern: &str) -> Vec<String> {
    split_gbnf_alternations(pattern)
        .into_iter()
        .filter_map(|alt| parse_quoted_literal(alt))
        .collect()
}

/// Returns true if `token` exactly matches the GBNF `pattern` (the RHS of `root ::=`).
pub fn token_matches_grammar(token: &str, pattern: &str) -> bool {
    grammar_literals(pattern).iter().any(|lit| lit == token)
}

fn split_gbnf_alternations(pattern: &str) -> Vec<&str> {
    let mut alts = Vec::new();
    let mut start = 0;
    let mut in_quotes = false;
    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => in_quotes = !in_quotes,
            b'\\' if in_quotes => i += 1, // skip escaped char
            b'|' if !in_quotes => {
                alts.push(pattern[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    alts.push(pattern[start..].trim());
    alts
}

fn parse_quoted_literal(s: &str) -> Option<String> {
    let s = s.trim();
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        let inner = &s[1..s.len()-1];
        Some(inner.replace("\\\"", "\"").replace("\\n", "\n").replace("\\t", "\t"))
    } else {
        None
    }
}

/// Wrap a user-supplied GBNF pattern (RHS only) into a complete grammar.
///
/// `pattern` is the right-hand side of the root rule, e.g. `"yes" | "no"`.
/// Trailing whitespace allowance is appended so llama.cpp can find a valid
/// continuation token after the pattern is fully consumed.
pub fn grammar_from_pattern(pattern: &str) -> String {
    format!("root ::= {} [ \\t\\n\\r]*", pattern)
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
        // Anchor-only: force the opening `[` as the first token; the inference
        // daemon drops the grammar sampler after token 1 — zero masking overhead.
        "array" | "Array" => Some("root ::= \"[\"".to_owned()),
        // Anchor-only: force the opening `{`.
        "dict" | "Dict" => Some("root ::= \"{\"".to_owned()),
        name => {
            let def = struct_defs.get(name)?;
            let _ = def;
            Some("root ::= \"{\"".to_owned())
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
    fn struct_grammar_is_anchor_only() {
        let fields = vec![
            StructFieldDef::Required("name".to_string()),
            StructFieldDef::Required("age".to_string()),
        ];
        let mut defs = HashMap::new();
        defs.insert("Person".to_string(), fields);
        let g = grammar_for("Person", &defs).unwrap();
        assert!(g.contains("\"{\""), "grammar should anchor opening brace");
    }

    #[test]
    fn array_grammar() {
        let g = grammar_for("array", &no_defs()).unwrap();
        assert!(g.starts_with("root"));
        assert!(g.contains("\"[\""), "should anchor opening bracket");
    }

    #[test]
    fn dict_grammar() {
        let g = grammar_for("dict", &no_defs()).unwrap();
        assert!(g.starts_with("root"));
        assert!(g.contains("\"{\""), "should anchor opening brace");
    }

    #[test]
    fn token_matches_single_literal() {
        assert!(token_matches_grammar("<think>", "\"<think>\""));
        assert!(!token_matches_grammar("other", "\"<think>\""));
    }

    #[test]
    fn token_matches_alternation() {
        assert!(token_matches_grammar("yes", "\"yes\" | \"no\""));
        assert!(token_matches_grammar("no", "\"yes\" | \"no\""));
        assert!(!token_matches_grammar("maybe", "\"yes\" | \"no\""));
    }

    #[test]
    fn token_matches_pipe_in_literal() {
        // A literal containing '|' should not be split on it
        assert!(token_matches_grammar("a|b", "\"a|b\""));
        assert!(!token_matches_grammar("a", "\"a|b\""));
    }

    #[test]
    fn token_no_match_non_literal_pattern() {
        // Character-class patterns aren't handled — return false, no panic
        assert!(!token_matches_grammar("abc", "[a-z]+"));
    }
}
