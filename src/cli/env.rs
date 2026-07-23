/// `jade env [--json]`
pub fn run_env(json: bool) {
    let version = env!("CARGO_PKG_VERSION");

    // Binary location
    let binary = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    // Platform
    let platform = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);

    // Config
    let cfg = crate::config::load_config();
    let global_path = crate::config::global_config_path();
    let global_path_str = global_path.display().to_string();

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
    let cache_size = super::format_bytes(total_bytes);

    let cache_root = crate::cache::cache_root().display().to_string();

    // Project info
    let project_info = crate::project::find_project_root().and_then(|root| {
        crate::project::load_project(&root).ok().and_then(|m| {
            let entry = m.entry_file().to_string();
            let p = m.project?;
            Some((
                root.join("jade.toml").display().to_string(),
                p.name,
                p.version.unwrap_or_else(|| "?".to_string()),
                entry,
            ))
        })
    });

    if json {
        let project_value = match &project_info {
            Some((path, name, ver, entry)) => serde_json::json!({
                "path":    path,
                "name":    name,
                "version": ver,
                "entry":   entry,
            }),
            None => serde_json::Value::Null,
        };

        let output = serde_json::json!({
            "jade_version": version,
            "binary":       binary,
            "platform":     platform,
            "config": {
                "source":      config_source,
                "max_retries": cfg.max_retries,
            },
            "cache": {
                "path":    cache_root,
                "entries": entry_count,
                "stale":   stale_count,
                "size":    cache_size,
            },
            "project": project_value,
        });

        println!("{}", serde_json::to_string_pretty(&output).unwrap_or_default());
    } else {
        // Human-readable output
        println!("jade {}", version);
        println!("  binary    {}", binary);
        println!("  platform  {}", platform);
        println!();
        println!("config:");
        println!("  source       {}", config_source);
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
