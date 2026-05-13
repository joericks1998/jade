use std::collections::HashMap;
use crate::frontend::ast::StructFieldDef;

const VALUE_RULES: &str = r#"value  ::= object | array | string | number | "true" | "false" | "null"
object ::= "{" ws (string ws ":" ws value (ws "," ws string ws ":" ws value)*)? ws "}"
array  ::= "[" ws (value (ws "," ws value)*)? ws "]"
string ::= "\"" ([^"\\\x7F\x00-\x1F] | "\\" (["\\/bfnrt] | "u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F]))* "\""
number ::= "-"? ("0" | [1-9] [0-9]*) ("." [0-9]+)? ([eE] [-+]? [0-9]+)?
ws     ::= ([ \t\n\r])*"#;

/// Generate a GBNF grammar string for the given JadeLang type name.
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
        "array" | "Array" => Some(format!(
            "root   ::= \"[\" ws (value (ws \",\" ws value)*)? ws \"]\" [ \\t\\n\\r]*\n{VALUE_RULES}"
        )),
        "dict" | "Dict" => Some(format!(
            "root   ::= \"{{\" ws (string ws \":\" ws value (ws \",\" ws string ws \":\" ws value)*)? ws \"}}\" [ \\t\\n\\r]*\n{VALUE_RULES}"
        )),
        name => {
            let def = struct_defs.get(name)?;
            if def.is_empty() {
                return Some(format!(
                    "root   ::= \"{{\" ws \"}}\" [ \\t\\n\\r]*\n{VALUE_RULES}"
                ));
            }
            let fields: Vec<String> = def.iter().map(|f| {
                let json_key = serde_json::to_string(f.name()).unwrap_or_else(|_| {
                    format!("\"{}\"", f.name())
                });
                format!("{json_key} ws \":\" ws value")
            }).collect();
            let root = format!(
                "root   ::= \"{{\" ws {} ws \"}}\" [ \\t\\n\\r]*",
                fields.join(" ws \",\" ws ")
            );
            Some(format!("{root}\n{VALUE_RULES}"))
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
    fn struct_grammar_contains_field_names() {
        let fields = vec![
            StructFieldDef::Required("name".to_string()),
            StructFieldDef::Required("age".to_string()),
        ];
        let mut defs = HashMap::new();
        defs.insert("Person".to_string(), fields);
        let g = grammar_for("Person", &defs).unwrap();
        assert!(g.contains("\"name\""), "grammar should contain field name");
        assert!(g.contains("\"age\""), "grammar should contain field age");
        assert!(g.contains("value"), "grammar should have value rule");
    }

    #[test]
    fn array_grammar() {
        let g = grammar_for("array", &no_defs()).unwrap();
        assert!(g.starts_with("root"));
        assert!(g.contains("value"));
    }

    #[test]
    fn dict_grammar() {
        let g = grammar_for("dict", &no_defs()).unwrap();
        assert!(g.starts_with("root"));
        assert!(g.contains("string ws \":\" ws value"));
    }
}
