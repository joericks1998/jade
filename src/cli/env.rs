/// `jade env [--json]`
pub fn run_env(json: bool) {
    let version = env!("CARGO_PKG_VERSION");

    // Binary location
    let binary = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    // Platform
    let platform = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);

    // LLVM status
    #[cfg(feature = "llvm")]
    let llvm_status = "enabled";
    #[cfg(not(feature = "llvm"))]
    let llvm_status = "disabled (rebuild with --features llvm)";

    // Config
    let cfg = crate::config::load_config();
    let global_path = crate::config::global_config_path();
    let global_path_str = global_path.display().to_string();

    let api_key_status = match &cfg.api_key {
        Some(_) if std::env::var("JADE_API_KEY").is_ok() => "set (via JADE_API_KEY env)",
        Some(_) => "set (via config file)",
        None => "not set",
    };

    // Config source
    let config_source = if global_path.exists() {
        global_path_str.clone()
    } else {
        "defaults only".to_string()
    };

    // Cache stats
    let cache_entries = crate::cache::list_entries();
    let entry_count = cache_entries.len();
    let stale_count = cache_entries
        .iter()
        .filter(|e| e.version != crate::cache::JADE_VERSION)
        .count();
    let total_bytes: u64 = cache_entries.iter().map(|e| e.size_bytes).sum();
    let cache_size = format_bytes(total_bytes);

    let cache_root = {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        format!("{}/.jade/cache/", home)
    };

    // Project info
    let project_info = crate::project::find_project_root().and_then(|root| {
        crate::project::load_project(&root).ok().map(|m| {
            let entry = m.entry_file().to_string();
            let p = m.project.unwrap();
            (
                root.join("jade.toml").display().to_string(),
                p.name,
                p.version.unwrap_or_else(|| "?".to_string()),
                entry,
            )
        })
    });

    if json {
        // JSON output
        let project_json = match &project_info {
            Some((path, name, ver, entry)) => format!(
                r#"  "project": {{"path": "{path}", "name": "{name}", "version": "{ver}", "entry": "{entry}"}}"#
            ),
            None => r#"  "project": null"#.to_string(),
        };

        println!(
            r#"{{
  "jade_version": "{version}",
  "binary": "{binary}",
  "llvm": "{llvm_status}",
  "platform": "{platform}",
  "config": {{
    "source": "{config_source}",
    "provider": "{provider}",
    "model": "{model}",
    "api_key": "{api_key_status}",
    "max_retries": {max_retries}
  }},
  "cache": {{
    "path": "{cache_root}",
    "entries": {entry_count},
    "stale": {stale_count},
    "size": "{cache_size}"
  }},
{project_json}
}}"#,
            provider = cfg.provider,
            model = cfg.model,
            max_retries = cfg.max_retries,
        );
    } else {
        // Human-readable output
        println!("jade {}", version);
        println!("  binary    {}", binary);
        println!("  llvm      {}", llvm_status);
        println!("  platform  {}", platform);
        println!();
        println!("config:");
        println!("  source       {}", config_source);
        println!("  provider     {}", cfg.provider);
        println!("  model        {}", cfg.model);
        println!("  api key      {}", api_key_status);
        println!("  max retries  {}", cfg.max_retries);
        println!();
        println!("cache ({}):", cache_root);
        println!("  entries  {}", entry_count);
        println!("  stale    {}", stale_count);
        println!("  size     {}", cache_size);

        if let Some((path, name, ver, entry)) = &project_info {
            println!();
            println!("project ({}):", path);
            println!("  name     {}", name);
            println!("  version  {}", ver);
            println!("  entry    {}", entry);
        }
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
