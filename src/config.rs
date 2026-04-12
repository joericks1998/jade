use serde::{Deserialize, Serialize};

/// Resolved configuration for a Jade run — combines all config layers.
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

/// Deserializable / serializable TOML config file shape.
#[derive(Deserialize, Serialize, Default)]
pub struct TomlConfig {
    pub model: Option<ModelSection>,
}

#[derive(Deserialize, Serialize, Default, Clone)]
pub struct ModelSection {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub max_retries: Option<usize>,
}

fn apply_model_section(cfg: &mut JadeConfig, m: &ModelSection) {
    if let Some(p) = &m.provider   { cfg.provider    = p.clone(); }
    if let Some(m) = &m.model      { cfg.model       = m.clone(); }
    if let Some(k) = &m.api_key    { cfg.api_key     = Some(k.clone()); }
    if let Some(r) = m.max_retries { cfg.max_retries = r; }
}

// ── Global config path ───────────────────────────────────────────────────────

/// Returns `~/.jade/config.toml` — the global user configuration file.
pub fn global_config_path() -> std::path::PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))  // Windows fallback
        .unwrap_or_else(|_| {
            // Neither HOME nor USERPROFILE set — fall back to CWD.
            // Config will be placed in .jade/ relative to the current directory.
            ".".to_string()
        });
    std::path::PathBuf::from(home).join(".jade").join("config.toml")
}

/// Write (or merge into) the global `~/.jade/config.toml`.
pub fn write_global_config(section: &ModelSection) -> Result<(), String> {
    let path = global_config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    // Read existing config to preserve any fields not being overwritten.
    let mut existing: TomlConfig = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default();

    let mut merged = existing.model.unwrap_or_default();
    if let Some(p) = &section.provider   { merged.provider    = Some(p.clone()); }
    if let Some(m) = &section.model      { merged.model       = Some(m.clone()); }
    if let Some(k) = &section.api_key    { merged.api_key     = Some(k.clone()); }
    if let Some(r) = section.max_retries { merged.max_retries = Some(r); }
    existing.model = Some(merged);

    let content = toml::to_string_pretty(&existing)
        .map_err(|e| format!("config serialize error: {}", e))?;
    std::fs::write(&path, content).map_err(|e| format!("config write error: {}", e))?;
    Ok(())
}

// ── Public loader ────────────────────────────────────────────────────────────

/// Load configuration with four-layer priority (lowest → highest):
///
/// 1. Built-in defaults
/// 2. `~/.jade/config.toml` (global user config)
/// 3. `./jade.toml [model]` (project-level override)
/// 4. Environment variables (`JADE_PROVIDER`, `JADE_MODEL`, `JADE_API_KEY`,
///    `JADE_MAX_RETRIES`)
pub fn load_config() -> JadeConfig {
    let mut cfg = JadeConfig::default();

    // Layer 2: global user config
    if let Ok(content) = std::fs::read_to_string(global_config_path()) {
        if let Ok(parsed) = toml::from_str::<TomlConfig>(&content) {
            if let Some(m) = &parsed.model {
                apply_model_section(&mut cfg, m);
            }
        }
    }

    // Layer 3: project jade.toml [model] section — walk up to project root so
    // running jade from a subdirectory still picks up the project config.
    if let Some(root) = crate::project::find_project_root() {
        let path = root.join("jade.toml");
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(parsed) = toml::from_str::<TomlConfig>(&content) {
                if let Some(m) = &parsed.model {
                    apply_model_section(&mut cfg, m);
                }
            }
        }
    }

    // Layer 4: environment variables (highest priority)
    if let Ok(p) = std::env::var("JADE_PROVIDER")   { cfg.provider = p; }
    if let Ok(m) = std::env::var("JADE_MODEL")       { cfg.model = m; }
    if let Ok(k) = std::env::var("JADE_API_KEY")     { cfg.api_key = Some(k); }
    if let Ok(r) = std::env::var("JADE_MAX_RETRIES") {
        if let Ok(n) = r.parse::<usize>() { cfg.max_retries = n; }
    }

    cfg
}
