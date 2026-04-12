use std::{fs, path::Path, process};

/// `jade fmt <path> [--check]`
///
/// Formats `.jde` source files in-place.  If `path` is a directory, all
/// `.jde` files are formatted recursively.  With `--check`, exits 1 if any
/// file would be changed (useful in CI).
///
/// **Note:** Jade's lexer strips comments before the token stream is produced,
/// so this formatter works directly on source text (line-based) to preserve
/// comments.  Limitations: operator spacing is not normalised; only
/// indentation and trailing whitespace are fixed.
pub fn run_fmt(path: &str, check: bool) {
    let p = Path::new(path);
    if !p.exists() {
        eprintln!("error: '{}' not found", path);
        process::exit(1);
    }

    let mut files: Vec<std::path::PathBuf> = Vec::new();
    if p.is_dir() {
        collect_jde_files(p, &mut files);
    } else if p.extension().and_then(|e| e.to_str()) == Some("jde") {
        files.push(p.to_path_buf());
    } else {
        eprintln!("error: '{}' is not a .jde file or directory", path);
        process::exit(1);
    }

    if files.is_empty() {
        eprintln!("no .jde files found in '{}'", path);
        return;
    }

    let mut any_changed = false;
    // Fix 6: collect write errors and continue processing remaining files
    // rather than aborting immediately on the first failure.
    let mut any_error = false;
    for file in &files {
        let source = match fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error reading '{}': {}", file.display(), e);
                process::exit(1);
            }
        };

        let formatted = format_source(&source);
        if formatted != source {
            any_changed = true;
            if check {
                eprintln!("would reformat: {}", file.display());
            } else {
                if let Err(e) = fs::write(file, &formatted) {
                    eprintln!("error writing '{}': {}", file.display(), e);
                    any_error = true;
                } else {
                    eprintln!("reformatted: {}", file.display());
                }
            }
        }
    }

    if any_error || (check && any_changed) {
        process::exit(1);
    }
}

/// Format a single source string.
///
/// Rules applied (in order, per line):
/// 1. Strip trailing whitespace.
/// 2. Track brace depth to compute expected indentation.
///    - A line starting with `}` dedents **before** applying indentation.
///    - A line ending with `{` increments depth **after** the line is emitted.
/// 3. Collapse 3+ consecutive blank lines to 2.
/// 4. Ensure a single trailing newline.
pub fn format_source(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut depth: i32 = 0;
    let mut blank_run = 0usize;

    for raw_line in src.lines() {
        let line = raw_line.trim_end(); // strip trailing whitespace

        if line.is_empty() {
            blank_run += 1;
            // Collapse: allow at most 2 consecutive blank lines.
            if blank_run <= 2 {
                out.push('\n');
            }
            continue;
        }
        blank_run = 0;

        // Count leading `}` to dedent first.
        let stripped = line.trim_start();
        let leading_close = stripped.chars().take_while(|&c| c == '}').count();
        depth -= leading_close as i32;
        if depth < 0 { depth = 0; }

        // Emit the line with correct indentation.
        let indent = "    ".repeat(depth as usize);
        out.push_str(&indent);
        out.push_str(stripped);
        out.push('\n');

        // Count net brace change for subsequent lines.
        let mut in_str = false;
        let mut escape = false;
        let mut net: i32 = 0;
        let mut chars = stripped.chars().peekable();
        while let Some(ch) = chars.next() {
            if escape { escape = false; continue; }
            if ch == '\\' && in_str { escape = true; continue; }
            if ch == '"' { in_str = !in_str; continue; }
            if in_str { continue; }
            // Jade has no line-comment syntax, so every character is significant.
            match ch {
                '{' => net += 1,
                '}' => net -= 1,
                _ => {}
            }
        }
        // net already accounts for leading `}` we already dedented;
        // re-add them so only the surplus `{` increments depth.
        depth += net + leading_close as i32;
        if depth < 0 { depth = 0; }
    }

    // Ensure exactly one trailing newline.
    if !out.ends_with('\n') {
        out.push('\n');
    }

    out
}

fn collect_jde_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with('.') && name != "target" {
                collect_jde_files(&path, out);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("jde") {
            out.push(path);
        }
    }
}
