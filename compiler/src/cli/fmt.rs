use std::{fs, path::Path, process};

use crate::frontend::lexer::tokenize;

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
        if formatted == source {
            continue;
        }

        // Refuse to write a file the formatter would have changed the meaning
        // of.  A whitespace-only rewrite has to lex to the same tokens; if it
        // does not, that is a bug here and the user's file is the wrong place
        // to discover it.
        if !tokens_agree(&source, &formatted) {
            eprintln!(
                "error: formatting '{}' would change what it means; left unchanged \
                 (please report this)",
                file.display()
            );
            any_error = true;
            continue;
        }

        any_changed = true;
        if check {
            eprintln!("would reformat: {}", file.display());
        } else if let Err(e) = fs::write(file, &formatted) {
            eprintln!("error writing '{}': {}", file.display(), e);
            any_error = true;
        } else {
            eprintln!("reformatted: {}", file.display());
        }
    }

    if any_error || (check && any_changed) {
        process::exit(1);
    }
}

/// Whether two source texts lex to the same token stream.
///
/// Formatting moves whitespace and nothing else, so the tokens have to match.
/// Spans deliberately do not take part in the comparison — reindenting a line
/// is *supposed* to move every column on it.
///
/// A source that does not lex at all is not the formatter's problem: `jade fmt`
/// runs on files people are still editing, so a syntax error means "leave it
/// alone", not "refuse".  Only the formatted side failing to lex, or the two
/// sides disagreeing, is a real fault.
pub(crate) fn tokens_agree(before: &str, after: &str) -> bool {
    let Ok(a) = tokenize(before) else { return true };
    match tokenize(after) {
        Ok(b) => a.len() == b.len() && a.iter().zip(&b).all(|(x, y)| x.kind == y.kind),
        Err(_) => false,
    }
}

/// An open delimiter, and what it means for indentation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Open {
    /// A `{` that ended its line, so it opens a block: `fn f() {`, `if x {`, a
    /// dict literal written across lines.  Its contents indent one level.
    Block,
    /// A `(`, `[`, or a `{` with more of the same line after it — a single
    /// expression that happens to wrap.  Its contents are left alone.
    Expr,
}

/// Format a single source string.
///
/// Rules applied (in order, per line):
/// 1. Strip trailing whitespace.
/// 2. Track block depth to compute expected indentation, four spaces per level.
///    - A line starting with `}` is written at the *enclosing* depth.
///    - Delimiters inside strings and inside `//` comments do not count.
/// 3. Collapse 3+ consecutive blank lines to 2.
/// 4. Ensure a single trailing newline.
///
/// Two kinds of line keep the leading whitespace they were written with:
///
/// - Lines inside a triple-quoted string, which are copied through byte for
///   byte.  Their indentation is part of the string's value, so touching it
///   would change what the program prints.
/// - Lines continuing a wrapped expression — an argument list, a collection
///   literal, a struct literal.  How a wrapped expression lines up is a layout
///   decision this formatter does not make, so it leaves the author's alignment
///   alone rather than flattening it to the enclosing block's depth.
///
/// What separates a block from a wrapped expression is where the `{` sits. One
/// that ends its line opens a block; one with more of the line after it is part
/// of an expression that ran long. So `let cfg = {` indents what follows, and
/// `Result { name: name,` does not.
pub fn format_source(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    // The delimiters open at the start of the current line, outermost first.
    let mut stack: Vec<Open> = Vec::new();
    let mut blank_run = 0usize;
    // The quote character of a triple-quoted string still open from an earlier
    // line, if any.
    let mut open: Option<char> = None;

    for raw_line in src.lines() {
        // Inside a multi-line string this line is data, not code.
        if open.is_some() {
            out.push_str(raw_line);
            out.push('\n');
            open = scan_line(raw_line, open, &mut stack);
            continue;
        }

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

        let stripped = line.trim_start();
        if stack.last() == Some(&Open::Expr) {
            // A continuation line: keep the author's alignment.
            out.push_str(line);
        } else {
            out.push_str(&"    ".repeat(indent_for(&stack, stripped)));
            out.push_str(stripped);
        }
        out.push('\n');

        open = scan_line(stripped, None, &mut stack);
    }

    // Ensure exactly one trailing newline.
    if !out.ends_with('\n') {
        out.push('\n');
    }

    out
}

/// How many levels to indent a line that starts with `line`.
///
/// A line opening with `}` closes blocks it is not itself inside, so those come
/// off the count first — that is what puts `}` and `} else {` at the depth of
/// the block they close rather than one level in.
fn indent_for(stack: &[Open], line: &str) -> usize {
    let blocks = stack.iter().filter(|o| **o == Open::Block).count();
    let mut closes = 0usize;
    let mut top = stack.len();
    for c in line.chars() {
        if c != '}' || top == 0 {
            break;
        }
        top -= 1;
        if stack[top] == Open::Block {
            closes += 1;
        }
    }
    blocks.saturating_sub(closes)
}

/// Walk one line, updating `stack` with the delimiters it opens and closes and
/// returning whichever triple quote is left open at the end.
///
/// Strings and `//` comments are skipped, so nothing inside them counts.
/// `open` carries a triple-quoted string in from the previous line.
fn scan_line(line: &str, open: Option<char>, stack: &mut Vec<Open>) -> Option<char> {
    let chars: Vec<char> = line.chars().collect();
    let mut open = open;
    // The last character of actual code, used to tell a block-opening `{` from
    // one in the middle of an expression.
    let mut last_code: Option<char> = None;
    let mut i = 0usize;

    while i < chars.len() {
        // Inside a triple-quoted string nothing matters but its terminator.
        if let Some(q) = open {
            if chars[i] == q && chars.get(i + 1) == Some(&q) && chars.get(i + 2) == Some(&q) {
                open = None;
                last_code = Some(q);
                i += 3;
            } else {
                i += 1;
            }
            continue;
        }

        let c = chars[i];
        match c {
            // A `//` comment runs to the end of the line, braces and all.
            '/' if chars.get(i + 1) == Some(&'/') => break,
            '"' | '\'' => {
                if chars.get(i + 1) == Some(&c) && chars.get(i + 2) == Some(&c) {
                    open = Some(c);
                    i += 3;
                } else {
                    // Ordinary string: skip to its close, honouring escapes.
                    i += 1;
                    while i < chars.len() {
                        if chars[i] == '\\' {
                            i += 2;
                        } else if chars[i] == c {
                            i += 1;
                            break;
                        } else {
                            i += 1;
                        }
                    }
                }
                last_code = Some(c);
            }
            // Provisionally an expression brace; promoted below if it turns out
            // to end the line.
            '{' | '(' | '[' => {
                stack.push(Open::Expr);
                last_code = Some(c);
                i += 1;
            }
            '}' | ')' | ']' => {
                // A surplus closer in a half-written file pops nothing.
                stack.pop();
                last_code = Some(c);
                i += 1;
            }
            _ => {
                if !c.is_whitespace() {
                    last_code = Some(c);
                }
                i += 1;
            }
        }
    }

    // A `{` that ended the line opens a block. It is necessarily the last thing
    // pushed, since nothing after it could have popped it.
    if last_code == Some('{')
        && let Some(top) = stack.last_mut()
    {
        *top = Open::Block;
    }

    open
}

pub(crate) fn collect_jde_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
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
