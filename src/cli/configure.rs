use std::io::{self, BufRead, Write};

fn flush(stdout: &mut io::Stdout) {
    stdout.flush().unwrap_or_else(|e| {
        eprintln!("error: could not flush stdout: {}", e);
        std::process::exit(1);
    });
}

fn read_line(stdin: &io::Stdin) -> String {
    let mut buf = String::new();
    stdin.lock().read_line(&mut buf).unwrap_or_else(|e| {
        eprintln!("error reading input: {}", e);
        std::process::exit(1);
    });
    buf
}

/// Run the interactive configuration wizard.
/// Reads from stdin, writes jade.toml in the current working directory.
pub fn run_configure() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let cfg = crate::config::load_config();

    if std::path::Path::new("jade.toml").exists() {
        println!("jade.toml found. Press Enter to keep each current value.");
        println!();
    } else {
        println!("No jade.toml found. Creating new configuration.");
        println!();
    }

    // ── Provider ─────────────────────────────────────────────────────────────
    print!("Provider [anthropic/openai] ({}): ", cfg.provider);
    flush(&mut stdout);
    let provider_input = read_line(&stdin);
    let provider = {
        let trimmed = provider_input.trim();
        if trimmed.is_empty() { cfg.provider.clone() } else { trimmed.to_string() }
    };

    // ── Default model ─────────────────────────────────────────────────────────
    let default_model_hint = match provider.as_str() {
        "openai" => "gpt-4o-mini",
        _ => "claude-haiku-4-5-20251001",
    };
    let current_model = if cfg.model == "claude-haiku-4-5-20251001" && provider == "openai" {
        default_model_hint.to_string()
    } else {
        cfg.model.clone()
    };
    print!("Model ({current_model}): ");
    flush(&mut stdout);
    let model_input = read_line(&stdin);
    let model = {
        let trimmed = model_input.trim();
        if trimmed.is_empty() { current_model } else { trimmed.to_string() }
    };

    // ── API key ───────────────────────────────────────────────────────────────
    let key_hint = if cfg.api_key.is_some() { "stored in jade.toml" } else { "not set" };
    print!("API key ({key_hint}, leave empty to rely on JADE_API_KEY env var): ");
    flush(&mut stdout);
    let key_input = read_line(&stdin);
    let api_key = {
        let trimmed = key_input.trim();
        if trimmed.is_empty() { cfg.api_key.clone() } else { Some(trimmed.to_string()) }
    };

    // ── Max retries ───────────────────────────────────────────────────────────
    print!("Max retries for typed dereferences ({}): ", cfg.max_retries);
    flush(&mut stdout);
    let retries_input = read_line(&stdin);
    let max_retries: usize = {
        let trimmed = retries_input.trim();
        if trimmed.is_empty() {
            cfg.max_retries
        } else {
            trimmed.parse().unwrap_or(cfg.max_retries)
        }
    };

    // ── Write jade.toml ───────────────────────────────────────────────────────
    let api_key_line = match &api_key {
        Some(k) => format!("api_key = \"{}\"\n", k),
        None => String::new(),
    };

    let toml_content = format!(
        "[model]\nprovider = \"{provider}\"\nmodel = \"{model}\"\nmax_retries = {max_retries}\n{api_key_line}"
    );

    std::fs::write("jade.toml", &toml_content).unwrap_or_else(|e| {
        eprintln!("error: could not write jade.toml: {}", e);
        std::process::exit(1);
    });

    println!();
    println!("jade.toml written successfully.");

    if api_key.is_some() {
        println!("Warning: your API key is stored in jade.toml in plaintext.");
        println!("         Consider using the JADE_API_KEY environment variable instead.");
    } else {
        println!("Set the JADE_API_KEY environment variable before running jade programs.");
    }

    println!();
    println!("Run 'jade <file.jde>' to use your configured model.");
}
