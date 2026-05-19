use std::collections::HashMap;
use crate::frontend::ast::StructFieldDef;

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
        // Anchor-only: force the opening `[` as the first token, then jade-tree
        // drops the grammar sampler — zero masking overhead after token 1.
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
}
