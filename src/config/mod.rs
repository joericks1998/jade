use serde::{Deserialize, Serialize};

/// Resolved configuration for a Jade run.
///
/// Provider, model, API key, and parallelism used to live here to feed the
/// in-language OpenAI/Anthropic backends. Those backends moved into the
/// inference daemon, which now owns all provider configuration (its own
/// `jaded` config + an eventual setup CLI). What remains is `max_retries`, a
/// language concern: how many times a typed dereference re-asks on a parse miss.
#[derive(Debug, Clone)]
pub struct JadeConfig {
    pub max_retries: usize,
}

impl Default for JadeConfig {
    fn default() -> Self {
        JadeConfig {
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

/// The `[model]` section of `jade.toml` / `~/.jade/config.toml`.
///
/// Only `max_retries` is still read. The provider/model/key fields are gone from
/// the language — the daemon owns them — but the section name and any unknown
/// keys are tolerated so an existing `jade.toml` written by an older `jade
/// configure` still loads without error.
#[derive(Deserialize, Serialize, Default, Clone)]
pub struct ModelSection {
    pub max_retries: Option<usize>,
}

fn apply_model_section(cfg: &mut JadeConfig, m: &ModelSection) {
    if let Some(r) = m.max_retries { cfg.max_retries = r; }
}

// ── Global config path ───────────────────────────────────────────────────────

/// Returns `~/.jade/config.toml` — the global user configuration file.
pub fn global_config_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| {
        // No HOME set — fall back to CWD, placing config in .jade/ relative to
        // the current directory.
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
/// 4. Environment variables (`JADE_MAX_RETRIES`)
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

    // Layer 3: nearest jade.toml [model] section — walk up from CWD so that
    // running jade from a subdirectory still picks up the config.  A jade.toml
    // without a [project] section is valid here (config-only files work too).
    if let Ok(mut dir) = std::env::current_dir() {
        loop {
            let path = dir.join("jade.toml");
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(parsed) = toml::from_str::<TomlConfig>(&content) {
                    if let Some(m) = &parsed.model {
                        apply_model_section(&mut cfg, m);
                    }
                }
                break; // stop at the first jade.toml found, whether it had [model] or not
            }
            if !dir.pop() {
                break;
            }
        }
    }

    // Layer 4: environment variables (highest priority)
    if let Ok(r) = std::env::var("JADE_MAX_RETRIES") {
        if let Ok(n) = r.parse::<usize>() { cfg.max_retries = n; }
    }

    cfg
}

#[cfg(test)]
mod tests;
