//! Tests for the safe, pure surface of the CLI subcommands. Most subcommand
//! handlers are side-effecting (stdin, network, process::exit, real fs) and are
//! NOT unit-tested here; see the module-level note below for what is skipped.
//!
//! `tests.rs` is a SIBLING of the subcommand submodules, so only `pub` /
//! `pub(crate)` items are reachable — private helpers (e.g. `upgrade::platform_tag`,
//! `model::KNOWN_MODELS`, `configure::read_line`) are not testable from here.

// ── format_bytes (cli/mod.rs, pub(crate)) ─────────────────────────────────────

mod format_bytes {
    use crate::cli::format_bytes;

    #[test]
    fn bytes_under_1kb() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1023), "1023 B");
    }

    #[test]
    fn kilobytes() {
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1536), "1.5 KB");
        // Just under 1 MB.
        assert_eq!(format_bytes(1024 * 1024 - 1), "1024.0 KB");
    }

    #[test]
    fn megabytes() {
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 + 512 * 1024), "3.5 MB");
    }
}

// ── fmt::format_source (pure string → string) ─────────────────────────────────

mod fmt {
    use crate::cli::fmt::format_source;

    #[test]
    fn strips_trailing_whitespace() {
        let out = format_source("let x = 1   \n");
        assert_eq!(out, "let x = 1\n");
    }

    #[test]
    fn ensures_single_trailing_newline() {
        assert_eq!(format_source("print(1)"), "print(1)\n");
        // Trailing whitespace-only content collapses cleanly.
        assert!(format_source("a\n").ends_with('\n'));
    }

    #[test]
    fn indents_braced_blocks() {
        let src = "fn f() {\nreturn 1\n}\n";
        let out = format_source(src);
        assert_eq!(out, "fn f() {\n    return 1\n}\n");
    }

    #[test]
    fn nested_braces_increase_indent() {
        let src = "fn f() {\nif x {\ny()\n}\n}\n";
        let out = format_source(src);
        assert_eq!(out, "fn f() {\n    if x {\n        y()\n    }\n}\n");
    }

    #[test]
    fn collapses_three_or_more_blank_lines_to_two() {
        let src = "a\n\n\n\n\nb\n";
        let out = format_source(src);
        assert_eq!(out, "a\n\n\nb\n");
    }

    #[test]
    fn braces_inside_strings_do_not_affect_depth() {
        // The `{` inside the string literal must not increase indentation.
        let src = "let s = \"a { b\"\nlet t = 2\n";
        let out = format_source(src);
        assert_eq!(out, "let s = \"a { b\"\nlet t = 2\n");
    }

    #[test]
    fn dedent_never_goes_negative() {
        // Extra closing brace should not panic or produce negative indent.
        let src = "}\n}\nx\n";
        let out = format_source(src);
        assert_eq!(out, "}\n}\nx\n");
    }

    #[test]
    fn already_formatted_is_idempotent() {
        let src = "fn f() {\n    return 1\n}\n";
        assert_eq!(format_source(src), src);
        // Running twice yields the same result.
        assert_eq!(format_source(&format_source(src)), src);
    }
}

// ── new::scaffold (writes into a unique temp dir, cleaned up) ──────────────────

mod new {
    use crate::cli::new::scaffold;
    use std::path::{Path, PathBuf};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("jade_cli_new_{tag}_{}_{n}", std::process::id()));
            TempDir { path }
        }
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn scaffold_basic_writes_expected_files() {
        let tmp = TempDir::new("basic");
        let dir = tmp.path().join("myapp");
        scaffold(&dir, "myapp", "basic");

        assert!(dir.join("jade.toml").exists());
        assert!(dir.join("main.jde").exists());
        assert!(dir.join(".gitignore").exists());

        let toml = std::fs::read_to_string(dir.join("jade.toml")).unwrap();
        assert!(toml.contains("name = \"myapp\""));
        assert!(toml.contains("[project]"));
        assert!(toml.contains("[scripts]"));

        let main = std::fs::read_to_string(dir.join("main.jde")).unwrap();
        assert!(main.contains("Hello from myapp!"));
        // basic template does not use prompts.
        assert!(!main.contains("prompt"));

        let gitignore = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert!(gitignore.contains("dist/"));
    }

    #[test]
    fn scaffold_llm_template_uses_prompt() {
        let tmp = TempDir::new("llm");
        let dir = tmp.path().join("llmapp");
        scaffold(&dir, "llmapp", "llm");

        let main = std::fs::read_to_string(dir.join("main.jde")).unwrap();
        assert!(main.contains("prompt p ="));
        assert!(main.contains("?p"));
    }

    #[test]
    fn scaffold_unknown_template_falls_back_to_basic() {
        let tmp = TempDir::new("fallback");
        let dir = tmp.path().join("app");
        scaffold(&dir, "app", "does-not-exist");
        let main = std::fs::read_to_string(dir.join("main.jde")).unwrap();
        // Non-"llm" templates use the basic (print) body.
        assert!(main.contains("print("));
        assert!(!main.contains("prompt"));
    }
}
