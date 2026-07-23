pub mod build;
pub mod cache;
pub mod check;
pub mod env;
pub mod fmt;
pub mod new;
pub mod pkg;
pub mod repl;
pub mod run;
pub mod test;
pub mod upgrade;

#[cfg(test)]
mod tests;

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
