use super::*;

// ── Defaults ──────────────────────────────────────────────────────────────

#[test]
fn default_config_values() {
    let cfg = JadeConfig::default();
    assert_eq!(cfg.max_retries, 3);
}

// ── apply_model_section ───────────────────────────────────────────────────

#[test]
fn apply_model_section_sets_max_retries() {
    let mut cfg = JadeConfig::default();
    let section = ModelSection { max_retries: Some(7) };
    apply_model_section(&mut cfg, &section);
    assert_eq!(cfg.max_retries, 7);
}

#[test]
fn apply_empty_model_section_preserves_defaults() {
    let mut cfg = JadeConfig::default();
    apply_model_section(&mut cfg, &ModelSection::default());
    assert_eq!(cfg.max_retries, 3);
}

// ── TOML parsing ──────────────────────────────────────────────────────────

#[test]
fn parse_reads_max_retries() {
    let src = r#"
[model]
max_retries = 5
"#;
    let parsed: TomlConfig = toml::from_str(src).expect("should parse");
    let m = parsed.model.expect("model section present");
    assert_eq!(m.max_retries, Some(5));
}

#[test]
fn legacy_provider_keys_are_tolerated() {
    // A jade.toml written by an older `jade configure` still carried
    // provider/model/api_key. The daemon owns those now, but the language must
    // not choke on their presence — no deny_unknown_fields, so they're ignored.
    let src = r#"
[model]
provider = "openai"
model = "gpt-4o"
api_key = "abc123"
max_parallel = 2
max_retries = 5
"#;
    let parsed: TomlConfig = toml::from_str(src).expect("legacy keys tolerated");
    assert_eq!(parsed.model.unwrap().max_retries, Some(5));
}

#[test]
fn parse_toml_missing_model_section() {
    let parsed: TomlConfig = toml::from_str("").expect("empty is valid");
    assert!(parsed.model.is_none());
}

#[test]
fn parse_malformed_toml_is_err() {
    let src = "[model\nmax_retries = ";
    let res: std::result::Result<TomlConfig, _> = toml::from_str(src);
    assert!(res.is_err());
}

#[test]
fn parse_wrong_type_toml_is_err() {
    // max_retries must be an integer, not a string.
    let src = r#"
[model]
max_retries = "not-a-number"
"#;
    let res: std::result::Result<TomlConfig, _> = toml::from_str(src);
    assert!(res.is_err());
}

#[test]
fn toml_roundtrip_serialize_then_parse() {
    let original = TomlConfig {
        model: Some(ModelSection { max_retries: Some(2) }),
    };
    let s = toml::to_string_pretty(&original).expect("serialize");
    let back: TomlConfig = toml::from_str(&s).expect("reparse");
    assert_eq!(back.model.unwrap().max_retries, Some(2));
}

// ── global_config_path ────────────────────────────────────────────────────

#[test]
fn global_config_path_ends_with_expected_suffix() {
    let p = global_config_path();
    assert!(p.ends_with(".jade/config.toml"), "got {:?}", p);
}
