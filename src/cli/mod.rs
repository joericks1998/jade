pub mod build;
pub mod cache;
pub mod check;
pub mod configure;
pub mod env;
pub mod fmt;
pub mod help;
pub mod model;
pub mod new;
pub mod repl;
pub mod rt;
pub mod run;
pub mod test;

/// Format a byte count as a human-readable string (B / KB / MB).
pub(crate) fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
