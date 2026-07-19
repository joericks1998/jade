use super::*;

// ── Defaults ──────────────────────────────────────────────────────────────

#[test]
fn default_config_values() {
    let cfg = JadeConfig::default();
    assert_eq!(cfg.provider, "anthropic");
    assert_eq!(cfg.model, "claude-haiku-4-5-20251001");
    assert!(cfg.api_key.is_none());
    assert_eq!(cfg.max_retries, 3);
    assert!(cfg.max_parallel.is_none());
}

// ── apply_model_section ───────────────────────────────────────────────────

#[test]
fn apply_full_model_section_overrides_all() {
    let mut cfg = JadeConfig::default();
    let section = ModelSection {
        provider: Some("openai".to_string()),
        model: Some("gpt-4".to_string()),
        api_key: Some("sk-test".to_string()),
        max_retries: Some(7),
        max_parallel: Some(4),
    };
    apply_model_section(&mut cfg, &section);
    assert_eq!(cfg.provider, "openai");
    assert_eq!(cfg.model, "gpt-4");
    assert_eq!(cfg.api_key.as_deref(), Some("sk-test"));
    assert_eq!(cfg.max_retries, 7);
    assert_eq!(cfg.max_parallel, Some(4));
}

#[test]
fn apply_empty_model_section_preserves_defaults() {
    let mut cfg = JadeConfig::default();
    let section = ModelSection::default();
    apply_model_section(&mut cfg, &section);
    // Nothing set → defaults unchanged.
    assert_eq!(cfg.provider, "anthropic");
    assert_eq!(cfg.model, "claude-haiku-4-5-20251001");
    assert!(cfg.api_key.is_none());
    assert_eq!(cfg.max_retries, 3);
    assert!(cfg.max_parallel.is_none());
}

#[test]
fn apply_partial_model_section_only_overrides_present() {
    let mut cfg = JadeConfig::default();
    let section = ModelSection {
        provider: None,
        model: Some("custom-model".to_string()),
        api_key: None,
        max_retries: None,
        max_parallel: None,
    };
    apply_model_section(&mut cfg, &section);
    assert_eq!(cfg.provider, "anthropic"); // untouched
    assert_eq!(cfg.model, "custom-model"); // overridden
    assert_eq!(cfg.max_retries, 3); // untouched
}

// ── TOML parsing ──────────────────────────────────────────────────────────

#[test]
fn parse_well_formed_toml() {
    let src = r#"
[model]
provider = "openai"
model = "gpt-4o"
api_key = "abc123"
max_retries = 5
max_parallel = 2
"#;
    let parsed: TomlConfig = toml::from_str(src).expect("should parse");
    let m = parsed.model.expect("model section present");
    assert_eq!(m.provider.as_deref(), Some("openai"));
    assert_eq!(m.model.as_deref(), Some("gpt-4o"));
    assert_eq!(m.api_key.as_deref(), Some("abc123"));
    assert_eq!(m.max_retries, Some(5));
    assert_eq!(m.max_parallel, Some(2));
}

#[test]
fn parse_toml_missing_model_section() {
    let src = "";
    let parsed: TomlConfig = toml::from_str(src).expect("empty is valid");
    assert!(parsed.model.is_none());
}

#[test]
fn parse_toml_partial_model_section() {
    let src = r#"
[model]
provider = "anthropic"
"#;
    let parsed: TomlConfig = toml::from_str(src).unwrap();
    let m = parsed.model.unwrap();
    assert_eq!(m.provider.as_deref(), Some("anthropic"));
    assert!(m.model.is_none());
    assert!(m.max_retries.is_none());
}

#[test]
fn parse_malformed_toml_is_err() {
    // Unclosed table header / invalid syntax.
    let src = "[model\nprovider = ";
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
        model: Some(ModelSection {
            provider: Some("anthropic".to_string()),
            model: Some("claude-opus".to_string()),
            api_key: None,
            max_retries: Some(2),
            max_parallel: None,
        }),
    };
    let s = toml::to_string_pretty(&original).expect("serialize");
    let back: TomlConfig = toml::from_str(&s).expect("reparse");
    let m = back.model.unwrap();
    assert_eq!(m.provider.as_deref(), Some("anthropic"));
    assert_eq!(m.model.as_deref(), Some("claude-opus"));
    assert_eq!(m.max_retries, Some(2));
    assert!(m.max_parallel.is_none());
}

// ── global_config_path ────────────────────────────────────────────────────

#[test]
fn global_config_path_ends_with_expected_suffix() {
    let p = global_config_path();
    assert!(p.ends_with(".jade/config.toml"), "got {:?}", p);
}
