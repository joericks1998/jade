//! `jade register` / `jade use` — the global provider registry commands.
//!
//! Providers ship with jade (see [`crate::providers`]); these commands record
//! which one `?p` uses and store its API key under `~/.jade`, machine-wide. The
//! language reads that selection at every prompt via
//! [`crate::llm::select_backend`].
//!
//! `install.sh` invokes `jade register` interactively after an install, so this
//! is often a fresh user's very first `jade` command — it stays chatty and
//! forgiving.

use std::io::{IsTerminal, Write};

use crate::providers;

/// `jade register [PROVIDER] [--key K] [--list] [--remove]`.
pub fn run_register(provider: Option<&str>, key: Option<&str>, list: bool, remove: bool) {
    if list {
        print_registry();
        return;
    }

    let installed = providers::installed();
    if installed.is_empty() {
        eprintln!(
            "No inference providers are installed.\n\
             Providers ship with jade under <prefix>/lib/jade/providers/ — if this is a source\n\
             build, point JADE_PROVIDERS_DIR at your built provider .so's."
        );
        std::process::exit(1);
    }

    // Resolve which provider we're acting on.
    let name = match provider {
        Some(p) => p.to_string(),
        None => match pick_interactive(&installed) {
            Some(n) => n,
            None => std::process::exit(1),
        },
    };

    // It must actually be installed — you can't select a provider with no library.
    if !installed.iter().any(|p| p.name == name) {
        eprintln!(
            "Provider '{name}' is not installed. Installed: {}.",
            join_names(&installed)
        );
        std::process::exit(1);
    }

    if remove {
        if let Err(e) = providers::remove_credential(&name) {
            eprintln!("Failed to remove credential for '{name}': {e}");
            std::process::exit(1);
        }
        // If it was the active provider, refresh the slot so the now-removed key
        // stops being served.
        if providers::active_provider().as_deref() == Some(name.as_str()) {
            let _ = providers::activate(&name);
        }
        println!("Removed the stored credential for '{name}'.");
        return;
    }

    // Obtain the credential: explicit --key, else an env var already in the
    // environment (recorded but not copied to disk), else an interactive prompt.
    let env_var = providers::credential_env_var(&name);
    if let Some(k) = key {
        store_or_exit(&name, k);
    } else if std::env::var(&env_var).map(|v| !v.is_empty()).unwrap_or(false) {
        println!("Using {env_var} from your environment (not storing a copy on disk).");
    } else if std::io::stdin().is_terminal() {
        let entered = prompt_key(&name);
        if entered.is_empty() {
            eprintln!("No key entered; leaving '{name}' unregistered.");
            std::process::exit(1);
        }
        store_or_exit(&name, &entered);
    } else {
        eprintln!(
            "No API key provided. Pass --key, or set {env_var} in the environment, \
             then re-run `jade register {name}`."
        );
        std::process::exit(1);
    }

    activate_or_exit(&name);
    println!("✓ '{name}' is now your active inference provider.");
    if !providers::has_credential(&name) {
        eprintln!(
            "Warning: no credential is reachable for '{name}'. Set {env_var} or \
             run `jade register {name} --key <KEY>`."
        );
    }
}

/// `jade use PROVIDER` — switch the active provider without touching its key.
pub fn run_use(provider: &str) {
    let installed = providers::installed();
    if !installed.iter().any(|p| p.name == provider) {
        eprintln!(
            "Provider '{provider}' is not installed. Installed: {}.",
            join_names(&installed)
        );
        std::process::exit(1);
    }
    activate_or_exit(provider);
    println!("✓ '{provider}' is now your active inference provider.");
    if !providers::has_credential(provider) {
        eprintln!(
            "Warning: '{provider}' has no credential yet — run `jade register {provider}`."
        );
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn print_registry() {
    let installed = providers::installed();
    let active = providers::active_provider();

    if installed.is_empty() {
        println!("No inference providers are installed.");
        return;
    }

    println!("Installed providers:");
    for p in &installed {
        let is_active = active.as_deref() == Some(p.name.as_str());
        let marker = if is_active { "*" } else { " " };
        let cred = if providers::has_credential(&p.name) { "key set" } else { "no key" };
        println!("  {marker} {:<12} ({cred})", p.name);
    }
    match &active {
        Some(a) => println!("\nActive: {a}  (marked * above)"),
        None => println!("\nActive: none — run `jade register` to choose one."),
    }
}

fn pick_interactive(installed: &[providers::InstalledProvider]) -> Option<String> {
    if !std::io::stdin().is_terminal() {
        eprintln!(
            "No provider specified. Run `jade register <provider>` (installed: {}).",
            join_names(installed)
        );
        return None;
    }
    println!("Choose an inference provider:");
    for (i, p) in installed.iter().enumerate() {
        println!("  {}) {}", i + 1, p.name);
    }
    print!("Enter a number [1-{}]: ", installed.len());
    let _ = std::io::stdout().flush();

    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return None;
    }
    match line.trim().parse::<usize>() {
        Ok(n) if n >= 1 && n <= installed.len() => Some(installed[n - 1].name.clone()),
        _ => {
            eprintln!("Not a valid choice.");
            None
        }
    }
}

fn prompt_key(name: &str) -> String {
    // NOTE: this echoes the key. Hidden entry (termios/rpassword) is a planned
    // follow-up; for now, prefer the <PROVIDER>_API_KEY env var for secrecy.
    print!("Enter the API key for '{name}': ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    line.trim().to_string()
}

fn store_or_exit(name: &str, key: &str) {
    if let Err(e) = providers::store_credential(name, key) {
        eprintln!("Failed to store credential for '{name}': {e}");
        std::process::exit(1);
    }
}

fn activate_or_exit(name: &str) {
    if let Err(e) = providers::activate(name) {
        eprintln!("Failed to activate '{name}': {e}");
        std::process::exit(1);
    }
}

fn join_names(installed: &[providers::InstalledProvider]) -> String {
    if installed.is_empty() {
        return "none".to_string();
    }
    installed.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
}
