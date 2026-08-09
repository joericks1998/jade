/// `jade env [--json]`
pub fn run_env(json: bool) {
    let version = env!("CARGO_PKG_VERSION");

    // Binary location
    let binary = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    // Platform
    let platform = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);

    // Cache stats
    let cache_entries = crate::cache::list_entries();
    let entry_count = cache_entries.len();
    let stale_count =
        cache_entries.iter().filter(|e| e.version != crate::cache::JADE_VERSION).count();
    let total_bytes: u64 = cache_entries.iter().map(|e| e.size_bytes).sum();
    let cache_size = super::format_bytes(total_bytes);

    let cache_root = crate::cache::cache_root().display().to_string();

    // Inference: the active provider and what's installed.
    let active_provider = crate::providers::active_provider();
    let provider_key_set =
        active_provider.as_deref().map(crate::providers::has_credential).unwrap_or(false);
    let installed_providers: Vec<String> =
        crate::providers::installed().into_iter().map(|p| p.name).collect();

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
            "cache": {
                "path":    cache_root,
                "entries": entry_count,
                "stale":   stale_count,
                "size":    cache_size,
            },
            "inference": {
                "provider":  active_provider,
                "key_set":   provider_key_set,
                "installed": installed_providers,
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
        println!("cache ({}):", cache_root);
        println!("  entries  {}", entry_count);
        println!("  stale    {}", stale_count);
        println!("  size     {}", cache_size);

        println!();
        println!("inference:");
        match &active_provider {
            Some(p) => {
                let key =
                    if provider_key_set { "key set" } else { "no key — run 'jade register'" };
                println!("  provider   {} ({})", p, key);
            }
            None => println!("  provider   none — run 'jade register' to choose one"),
        }
        let installed = if installed_providers.is_empty() {
            "none".to_string()
        } else {
            installed_providers.join(", ")
        };
        println!("  installed  {}", installed);

        if let Some((path, name, ver, entry)) = &project_info {
            println!();
            println!("project ({}):", path);
            println!("  name     {}", name);
            println!("  version  {}", ver);
            println!("  entry    {}", entry);
        }
    }
}
