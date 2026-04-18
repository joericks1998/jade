use std::process;

// ── Known models ──────────────────────────────────────────────────────────────

struct KnownModel {
    provider: &'static str,
    name: &'static str,
    note: &'static str,
}

const KNOWN_MODELS: &[KnownModel] = &[
    KnownModel { provider: "anthropic", name: "claude-haiku-4-5-20251001", note: "fast, cheap" },
    KnownModel { provider: "anthropic", name: "claude-sonnet-4-5",          note: "balanced" },
    KnownModel { provider: "anthropic", name: "claude-opus-4-5-20251001",   note: "powerful" },
    KnownModel { provider: "anthropic", name: "claude-sonnet-4-6",          note: "latest" },
    KnownModel { provider: "anthropic", name: "claude-opus-4-6",            note: "most capable" },
    KnownModel { provider: "openai",    name: "gpt-4o-mini",                note: "fast, cheap" },
    KnownModel { provider: "openai",    name: "gpt-4o",                     note: "balanced" },
    KnownModel { provider: "openai",    name: "o1-mini",                    note: "reasoning" },
    KnownModel { provider: "openai",    name: "o3-mini",                    note: "advanced reasoning" },
];

// ── Commands ──────────────────────────────────────────────────────────────────

/// `jade model list`
pub fn run_model_list() {
    let cfg = crate::config::load_config();

    let anthropic: Vec<_> = KNOWN_MODELS.iter().filter(|m| m.provider == "anthropic").collect();
    let openai: Vec<_>    = KNOWN_MODELS.iter().filter(|m| m.provider == "openai").collect();

    println!("anthropic:");
    for m in anthropic {
        let is_current = m.provider == cfg.provider && m.name == cfg.model;
        let marker = if is_current { " ◀ current" } else { "" };
        println!("  {:42} ({}){}", m.name, m.note, marker);
    }
    println!();
    println!("openai:");
    for m in openai {
        let is_current = m.provider == cfg.provider && m.name == cfg.model;
        let marker = if is_current { " ◀ current" } else { "" };
        println!("  {:42} ({}){}", m.name, m.note, marker);
    }
    println!();
    println!("currently configured: {}/{}", cfg.provider, cfg.model);
    println!();
    println!("To change: jade model use <provider>/<model-name>");
    println!("           jade model use anthropic/claude-opus-4-6");
}

/// `jade model use <provider>/<model>`
pub fn run_model_use(spec: &str) {
    let parts: Vec<&str> = spec.splitn(2, '/').collect();
    if parts.len() != 2 {
        eprintln!("error: expected format <provider>/<model-name>");
        eprintln!("       e.g. jade model use anthropic/claude-opus-4-6");
        process::exit(1);
    }
    let (provider, model) = (parts[0], parts[1]);

    // Validate provider.
    if provider != "anthropic" && provider != "openai" {
        eprintln!("error: unknown provider '{}'. Supported: anthropic, openai", provider);
        process::exit(1);
    }

    let section = crate::config::ModelSection {
        provider: Some(provider.to_string()),
        model:    Some(model.to_string()),
        api_key:  None,
        max_retries: None,
        max_parallel: None,
    };

    match crate::config::write_global_config(&section) {
        Ok(()) => {
            let path = crate::config::global_config_path();
            println!("model updated: {}/{}", provider, model);
            println!("config written to: {}", path.display());
        }
        Err(e) => {
            eprintln!("error: {}", e);
            process::exit(1);
        }
    }
}
