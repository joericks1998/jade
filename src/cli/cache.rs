/// `jade cache info`
pub fn run_cache_info() {
    let entries = crate::cache::list_entries();
    let total = entries.len();
    let stale = entries
        .iter()
        .filter(|e| e.version != crate::cache::JADE_VERSION)
        .count();
    let total_bytes: u64 = entries.iter().map(|e| e.size_bytes).sum();

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let cache_path = format!("{}/.jade/cache/", home);

    println!("cache location:  {}", cache_path);
    println!("jade version:    {}", crate::cache::JADE_VERSION);
    println!("total entries:   {}", total);
    println!("stale entries:   {} (from older jade versions)", stale);
    println!("total size:      {}", format_bytes(total_bytes));
}

/// `jade cache clean [--older-than N] [--dry-run]`
pub fn run_cache_clean(older_than_days: Option<u64>, dry_run: bool) {
    use std::time::{Duration, SystemTime};

    let now = SystemTime::now();
    let current_version = crate::cache::JADE_VERSION;

    let (count, bytes) = crate::cache::purge_entries(
        |entry| {
            // Always remove version-mismatched entries.
            if entry.version != current_version {
                return true;
            }
            // Optionally remove entries older than N days.
            if let Some(days) = older_than_days {
                if let Some(modified) = entry.modified {
                    let age = now.duration_since(modified).unwrap_or(Duration::ZERO);
                    return age > Duration::from_secs(days * 86_400);
                }
            }
            false
        },
        dry_run,
    );

    if dry_run {
        println!(
            "would remove {} entries ({})",
            count,
            format_bytes(bytes)
        );
    } else {
        println!("removed {} entries ({} freed)", count, format_bytes(bytes));
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
