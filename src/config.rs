use serde::Deserialize;

/// Resolved configuration for a Jade run — combines jade.toml values with env var overrides.
#[derive(Debug, Clone)]
pub struct JadeConfig {
    pub provider: String,
    pub model: String,
    pub api_key: Option<String>,
    pub max_retries: usize,
}

impl Default for JadeConfig {
    fn default() -> Self {
        JadeConfig {
            provider: "anthropic".to_string(),
            model: "claude-haiku-4-5-20251001".to_string(),
            api_key: None,
            max_retries: 3,
        }
    }
}

// ── TOML deserialization types ───────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct TomlConfig {
    model: Option<ModelSection>,
}

#[derive(Deserialize)]
struct ModelSection {
    provider: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
    max_retries: Option<usize>,
}

// ── Public loader ────────────────────────────────────────────────────────────

/// Load configuration from jade.toml (if present), then apply env var overrides.
/// Missing keys at each layer silently inherit the previous value.
pub fn load_config() -> JadeConfig {
    let mut cfg = JadeConfig::default();

    // Layer 1: jade.toml in the working directory
    if let Ok(content) = std::fs::read_to_string("jade.toml") {
        if let Ok(parsed) = toml::from_str::<TomlConfig>(&content) {
            if let Some(m) = parsed.model {
                if let Some(p) = m.provider     { cfg.provider    = p; }
                if let Some(m) = m.model        { cfg.model       = m; }
                if let Some(k) = m.api_key      { cfg.api_key     = Some(k); }
                if let Some(r) = m.max_retries  { cfg.max_retries = r; }
            }
        }
    }

    // Layer 2: environment variables override file values
    if let Ok(p) = std::env::var("JADE_PROVIDER")    { cfg.provider = p; }
    if let Ok(m) = std::env::var("JADE_MODEL")        { cfg.model = m; }
    if let Ok(k) = std::env::var("JADE_API_KEY")      { cfg.api_key = Some(k); }
    if let Ok(r) = std::env::var("JADE_MAX_RETRIES") {
        if let Ok(n) = r.parse::<usize>() { cfg.max_retries = n; }
    }

    cfg
}
