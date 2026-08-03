//! Tests for the safe, pure surface of the CLI subcommands.
//!
//! A subcommand handler itself is not testable in process: every one of them
//! ends in `process::exit`, and several read stdin, reach the network, or write
//! under `~/.jade`.  What *is* tested here is the decision each one makes before
//! it touches the world — how a source file is formatted, where a build lands,
//! which archive this platform wants, how a value is displayed.  Those are the
//! parts that silently drift.
//!
//! `tests.rs` is a SIBLING of the subcommand submodules, so only `pub` /
//! `pub(crate)` items are reachable from here.

use std::path::{Path, PathBuf};

/// A uniquely named directory under the system temp dir, removed on drop.
///
/// `cargo test` runs in parallel and this crate must not mutate process-global
/// state, so every test gets its own directory rather than sharing one and
/// changing the working directory to reach it.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("jade_cli_{tag}_{}_{n}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write a file under this directory, creating parent directories.
    pub fn write(&self, rel: &str, contents: &str) -> PathBuf {
        let p = self.path.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&p, contents).expect("write file");
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ── format_bytes (cli/mod.rs) ─────────────────────────────────────────────────

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

// ── fmt::format_source ────────────────────────────────────────────────────────

mod fmt {
    use super::TempDir;
    use crate::cli::fmt::{collect_jde_files, format_source, tokens_agree};

    // ── the basics ────────────────────────────────────────────────────────────

    #[test]
    fn strips_trailing_whitespace() {
        assert_eq!(format_source("let x = 1   \n"), "let x = 1\n");
    }

    #[test]
    fn ensures_single_trailing_newline() {
        assert_eq!(format_source("print(1)"), "print(1)\n");
        assert!(format_source("a\n").ends_with('\n'));
    }

    #[test]
    fn indents_braced_blocks() {
        let src = "fn f() {\nreturn 1\n}\n";
        assert_eq!(format_source(src), "fn f() {\n    return 1\n}\n");
    }

    #[test]
    fn nested_braces_increase_indent() {
        let src = "fn f() {\nif x {\ny()\n}\n}\n";
        assert_eq!(
            format_source(src),
            "fn f() {\n    if x {\n        y()\n    }\n}\n"
        );
    }

    #[test]
    fn collapses_three_or_more_blank_lines_to_two() {
        assert_eq!(format_source("a\n\n\n\n\nb\n"), "a\n\n\nb\n");
    }

    #[test]
    fn dedent_never_goes_negative() {
        // A surplus closing brace must not panic or produce negative indent.
        assert_eq!(format_source("}\n}\nx\n"), "}\n}\nx\n");
    }

    #[test]
    fn already_formatted_is_idempotent() {
        let src = "fn f() {\n    return 1\n}\n";
        assert_eq!(format_source(src), src);
        assert_eq!(format_source(&format_source(src)), src);
    }

    #[test]
    fn a_closing_and_reopening_line_sits_at_the_outer_depth() {
        // `} else {` and `} catch e {` close one block and open another, so they
        // belong to the enclosing level, not the one they are closing.
        let src = "fn f() {\nif x {\na()\n} else {\nb()\n}\n}\n";
        assert_eq!(
            format_source(src),
            "fn f() {\n    if x {\n        a()\n    } else {\n        b()\n    }\n}\n"
        );
    }

    #[test]
    fn two_closers_on_one_line_land_at_the_outermost_depth() {
        let src = "fn f() {\nif x {\na()\n}}\n";
        assert_eq!(format_source(src), "fn f() {\n    if x {\n        a()\n}}\n");
    }

    // ── strings and comments ──────────────────────────────────────────────────

    #[test]
    fn braces_inside_strings_do_not_affect_depth() {
        let src = "let s = \"a { b\"\nlet t = 2\n";
        assert_eq!(format_source(src), src);
    }

    #[test]
    fn braces_inside_single_quoted_strings_do_not_affect_depth() {
        // The scanner used to track only `"`, so a `}` in a single-quoted string
        // dedented every line after it.
        let src = "fn f() {\nprint('a } b')\nprint(2)\n}\n";
        assert_eq!(
            format_source(src),
            "fn f() {\n    print('a } b')\n    print(2)\n}\n"
        );
    }

    #[test]
    fn braces_inside_line_comments_do_not_affect_depth() {
        // The old formatter's own comment claimed Jade had no line comments, so
        // a stray `{` in one indented the whole rest of the file.
        let src = "// opens nothing {\nlet a = 1\n// closes nothing }\nlet b = 2\n";
        assert_eq!(format_source(src), src);
    }

    #[test]
    fn a_trailing_comment_does_not_affect_depth() {
        let src = "fn f() {\nprint(1) // note {\nprint(2)\n}\n";
        assert_eq!(
            format_source(src),
            "fn f() {\n    print(1) // note {\n    print(2)\n}\n"
        );
    }

    #[test]
    fn a_comment_line_is_indented_with_its_block() {
        let src = "fn f() {\n// why\nreturn 1\n}\n";
        assert_eq!(format_source(src), "fn f() {\n    // why\n    return 1\n}\n");
    }

    #[test]
    fn division_is_not_mistaken_for_a_comment() {
        let src = "fn f() {\nlet r = a / b\n}\n";
        assert_eq!(format_source(src), "fn f() {\n    let r = a / b\n}\n");
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string() {
        // If the `\"` closed the string, the following `{` would count as code.
        let src = "let s = \"a \\\" { b\"\nlet t = 2\n";
        assert_eq!(format_source(src), src);
    }

    #[test]
    fn an_empty_string_does_not_read_as_a_triple_quote() {
        let src = "fn f() {\nlet s = \"\"\nprint(s)\n}\n";
        assert_eq!(
            format_source(src),
            "fn f() {\n    let s = \"\"\n    print(s)\n}\n"
        );
    }

    #[test]
    fn fstring_interpolation_braces_do_not_affect_depth() {
        let src = "fn f() {\nprint(f\"x = {x}\")\nprint(2)\n}\n";
        assert_eq!(
            format_source(src),
            "fn f() {\n    print(f\"x = {x}\")\n    print(2)\n}\n"
        );
    }

    #[test]
    fn quotes_nested_in_an_fstring_interpolation_stay_balanced() {
        // `f"{s["k"]} done"` reads to this scanner as two strings with `s[`
        // between them, which is not what the lexer sees — but the brackets and
        // parens it skips over balance out either way. Real code in the wild
        // looks like this, so it is pinned rather than left to luck.
        let src = "fn f() {\nraise f\"{s[\"failed\"]} check(s) failed\"\nprint(1)\n}\n";
        assert_eq!(
            format_source(src),
            "fn f() {\n    raise f\"{s[\"failed\"]} check(s) failed\"\n    print(1)\n}\n"
        );
    }

    // ── multi-line strings ────────────────────────────────────────────────────

    #[test]
    fn triple_quoted_string_contents_are_left_byte_for_byte() {
        // Indentation inside the string is part of its value. Reindenting these
        // lines changed what the program printed, silently, in place.
        let src = "fn f() {\nlet s = \"\"\"\nline one\n  line two\n\"\"\"\n}\n";
        let out = format_source(src);
        assert!(out.contains("\nline one\n  line two\n"), "got: {out:?}");
    }

    #[test]
    fn trailing_whitespace_inside_a_triple_quoted_string_survives() {
        let src = "let s = \"\"\"\nkeep me   \n\"\"\"\n";
        assert!(format_source(src).contains("keep me   \n"));
    }

    #[test]
    fn code_after_a_triple_quoted_string_resumes_indenting() {
        let src = "fn f() {\nlet s = \"\"\"\nraw\n\"\"\"\nreturn s\n}\n";
        assert_eq!(
            format_source(src),
            "fn f() {\n    let s = \"\"\"\nraw\n\"\"\"\n    return s\n}\n"
        );
    }

    #[test]
    fn braces_inside_a_triple_quoted_string_do_not_affect_depth() {
        let src = "fn f() {\nlet s = \"\"\"\n{ { {\n\"\"\"\nreturn s\n}\n";
        assert_eq!(
            format_source(src),
            "fn f() {\n    let s = \"\"\"\n{ { {\n\"\"\"\n    return s\n}\n"
        );
    }

    #[test]
    fn triple_single_quotes_work_the_same_way() {
        let src = "fn f() {\nlet s = '''\n  raw\n'''\nreturn s\n}\n";
        assert_eq!(
            format_source(src),
            "fn f() {\n    let s = '''\n  raw\n'''\n    return s\n}\n"
        );
    }

    // ── wrapped expressions ───────────────────────────────────────────────────

    #[test]
    fn continuation_lines_keep_the_authors_alignment() {
        // How a wrapped argument list lines up is not this formatter's decision.
        // It used to flatten every one of these to column 0.
        let src = "let r = call([\n    a,\n    b\n])\n";
        assert_eq!(format_source(src), src);
    }

    #[test]
    fn a_wrapped_call_inside_a_block_is_left_alone() {
        let src = "fn f() {\nreturn call(\n        a,\n        b\n    )\n}\n";
        assert_eq!(
            format_source(src),
            "fn f() {\n    return call(\n        a,\n        b\n    )\n}\n"
        );
    }

    #[test]
    fn a_wrapped_struct_literal_keeps_its_alignment() {
        // The brace here opens an expression, not a block: there is more of the
        // line after it. Flattening the continuation to the block depth threw
        // away alignment people had written by hand.
        let src = "fn f() {\nreturn R { a: 1,\n           b: 2 }\n}\n";
        assert_eq!(
            format_source(src),
            "fn f() {\n    return R { a: 1,\n           b: 2 }\n}\n"
        );
    }

    #[test]
    fn a_wrapped_literal_does_not_leak_depth_into_later_lines() {
        // The `}` that ends the expression must close the brace that opened it,
        // or everything after it indents one level too far, forever.
        let src = "fn f() {\nlet r = R { a: 1,\n            b: 2 }\nreturn r\n}\nprint(1)\n";
        assert_eq!(
            format_source(src),
            "fn f() {\n    let r = R { a: 1,\n            b: 2 }\n    return r\n}\nprint(1)\n"
        );
    }

    #[test]
    fn a_multi_line_dict_literal_is_still_indented() {
        // Braces are block structure; brackets are expression layout. A dict
        // spanning lines has no open bracket, so it gets indented normally.
        let src = "let cfg = {\n\"a\": 1,\n\"b\": 2\n}\n";
        assert_eq!(
            format_source(src),
            "let cfg = {\n    \"a\": 1,\n    \"b\": 2\n}\n"
        );
    }

    #[test]
    fn a_balanced_call_on_one_line_does_not_leave_a_bracket_open() {
        let src = "fn f() {\nprint(g(1), h(2))\nprint(3)\n}\n";
        assert_eq!(
            format_source(src),
            "fn f() {\n    print(g(1), h(2))\n    print(3)\n}\n"
        );
    }

    // ── the safety net ────────────────────────────────────────────────────────

    #[test]
    fn formatting_every_example_preserves_its_tokens() {
        // The suite in `examples/` is the widest real sample of Jade there is.
        // Formatting any of it into a different token stream is a bug here.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
        let mut files = Vec::new();
        collect_jde_files(&root, &mut files);
        assert!(files.len() > 50, "expected the fixture suite, found {}", files.len());

        for file in &files {
            let src = std::fs::read_to_string(file).expect("read fixture");
            let out = format_source(&src);
            assert!(
                tokens_agree(&src, &out),
                "formatting changed the tokens of {}",
                file.display()
            );
            // And it has to settle: formatting an already-formatted file is a
            // no-op, or `jade fmt --check` would never pass twice in a row.
            assert_eq!(format_source(&out), out, "not idempotent: {}", file.display());
        }
    }

    use std::path::Path;

    #[test]
    fn tokens_agree_accepts_a_pure_whitespace_change() {
        assert!(tokens_agree("fn f() {\nreturn 1\n}\n", "fn f() {\n    return 1\n}\n"));
    }

    #[test]
    fn tokens_agree_rejects_a_changed_string() {
        assert!(!tokens_agree("let s = \"a\"\n", "let s = \"b\"\n"));
    }

    #[test]
    fn tokens_agree_leaves_a_file_that_does_not_lex_alone() {
        // A half-typed file is not the formatter's problem to refuse.
        assert!(tokens_agree("let s = \"unterminated\n", "anything at all\n"));
    }

    // ── file discovery ────────────────────────────────────────────────────────

    #[test]
    fn collects_jde_files_recursively() {
        let tmp = TempDir::new("collect");
        tmp.write("a.jde", "");
        tmp.write("sub/b.jde", "");
        tmp.write("sub/deep/c.jde", "");
        tmp.write("notes.md", "");
        tmp.write("sub/d.txt", "");

        let mut found = Vec::new();
        collect_jde_files(tmp.path(), &mut found);
        let mut names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["a.jde", "b.jde", "c.jde"]);
    }

    #[test]
    fn skips_hidden_and_target_directories() {
        let tmp = TempDir::new("skip");
        tmp.write("keep.jde", "");
        tmp.write(".git/hidden.jde", "");
        tmp.write("target/built.jde", "");

        let mut found = Vec::new();
        collect_jde_files(tmp.path(), &mut found);
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["keep.jde"]);
    }
}

// ── build::output_path ────────────────────────────────────────────────────────

mod build {
    use crate::cli::build::output_path;
    use std::path::PathBuf;

    #[test]
    fn explicit_output_wins() {
        assert_eq!(output_path("src/app.jde", Some("dist/app"), false), PathBuf::from("dist/app"));
        // …including for a library, extension and all.
        assert_eq!(output_path("src/app.jde", Some("libapp.so"), true), PathBuf::from("libapp.so"));
    }

    #[test]
    fn defaults_to_the_source_stem_beside_the_source() {
        assert_eq!(output_path("src/app.jde", None, false), PathBuf::from("src/app"));
        assert_eq!(output_path("app.jde", None, false), PathBuf::from("app"));
    }

    #[test]
    fn a_library_gets_the_platform_extension() {
        // `use <name>` resolves a package by stem, so the loader needs a real
        // shared-library file to open.
        let out = output_path("src/mathlib.jde", None, true);
        let expected = if cfg!(target_os = "macos") { "mathlib.dylib" } else { "mathlib.so" };
        assert_eq!(out, PathBuf::from("src").join(expected));
    }

    #[test]
    fn a_source_without_an_extension_still_produces_a_path() {
        assert_eq!(output_path("app", None, false), PathBuf::from("app"));
    }
}

// ── upgrade::archive_label ────────────────────────────────────────────────────

mod upgrade {
    use crate::cli::upgrade::archive_label;

    #[test]
    fn names_the_archive_release_yml_actually_publishes() {
        // Only two platforms are built. These names must match `release.yml`,
        // and they are deliberately not the `pkg::fetch::platform_tag` values.
        let label = archive_label();
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => assert_eq!(label, Some("macos-arm64")),
            ("linux", "x86_64") => assert_eq!(label, Some("linux-x86_64")),
            _ => assert_eq!(label, None, "an unbuilt platform must upgrade to nothing"),
        }
    }
}

// ── register::join_names ──────────────────────────────────────────────────────

mod register {
    use crate::cli::register::join_names;
    use crate::providers::InstalledProvider;

    fn provider(name: &str) -> InstalledProvider {
        InstalledProvider { name: name.to_string(), path: std::path::PathBuf::from("/dev/null") }
    }

    #[test]
    fn no_providers_reads_as_none() {
        // This lands mid-sentence in "installed: {}", so an empty list has to
        // say something rather than trail off.
        assert_eq!(join_names(&[]), "none");
    }

    #[test]
    fn lists_providers_in_order() {
        assert_eq!(join_names(&[provider("anthropic")]), "anthropic");
        assert_eq!(
            join_names(&[provider("anthropic"), provider("openai")]),
            "anthropic, openai"
        );
    }
}

// ── repl::prints_own_output ───────────────────────────────────────────────────

mod repl {
    use crate::cli::repl::prints_own_output;
    use crate::frontend::ast::Expr;
    use crate::frontend::error::Span;

    fn span() -> Span {
        Span { line: 1, col: 1 }
    }

    fn ident(name: &str) -> Expr {
        Expr::Identifier { name: name.to_string(), span: span() }
    }

    fn call(callee: Expr) -> Expr {
        Expr::Call { callee: Box::new(callee), args: Vec::new(), kwargs: Vec::new(), span: span() }
    }

    /// A dereference prints as it generates, and a `|>` stage over one still
    /// does — the REPL must not echo the result too, or every token appears
    /// twice. `stream(...)` used to be the other case; it no longer exists.
    #[test]
    fn a_deref_prints_as_it_generates() {
        let deref = Expr::PromptDeref {
            expr: Box::new(ident("p")),
            constraint: None,
            style: crate::frontend::ast::DerefStyle::Prefix,
            span: span(),
        };
        assert!(prints_own_output(&deref));
        assert!(prints_own_output(&Expr::Pipe {
            value: Box::new(deref),
            stage: Box::new(ident("g")),
            span: span(),
        }));
    }

    #[test]
    fn an_ordinary_call_does_not() {
        assert!(!prints_own_output(&call(ident("len"))));
    }

    #[test]
    fn a_bare_identifier_does_not() {
        assert!(!prints_own_output(&ident("x")));
    }
}

// ── run::format_global (the `jade run -v` dump) ───────────────────────────────

mod run {
    use crate::cli::run::format_global;
    use crate::vm::VmValue;

    #[test]
    fn scalars_read_back_as_they_were_written() {
        assert_eq!(format_global("n", &VmValue::Int(42)).unwrap(), "n = 42");
        assert_eq!(format_global("b", &VmValue::Bool(true)).unwrap(), "b = true");
        assert_eq!(
            format_global("s", &VmValue::Str("hi".into())).unwrap(),
            "s = \"hi\""
        );
    }

    #[test]
    fn a_whole_float_keeps_its_point() {
        // `1.0` formats as `1`, which would read as an int in the dump.
        assert_eq!(format_global("f", &VmValue::Float(1.0)).unwrap(), "f = 1.0");
        assert_eq!(format_global("f", &VmValue::Float(-2.0)).unwrap(), "f = -2.0");
        assert_eq!(format_global("f", &VmValue::Float(1.5)).unwrap(), "f = 1.5");
    }

    #[test]
    fn machinery_a_user_never_wrote_is_hidden() {
        // Every program starts with the built-in slots filled in; listing them
        // would bury the globals that are the only reason to pass `-v`.
        assert!(format_global("nothing", &VmValue::Nil).is_none());
    }
}

// ── new::scaffold ─────────────────────────────────────────────────────────────

mod new {
    use super::TempDir;
    use crate::cli::new::scaffold;

    #[test]
    fn scaffold_basic_writes_expected_files() {
        let tmp = TempDir::new("new_basic");
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
        let tmp = TempDir::new("new_llm");
        let dir = tmp.path().join("llmapp");
        scaffold(&dir, "llmapp", "llm");

        let main = std::fs::read_to_string(dir.join("main.jde")).unwrap();
        assert!(main.contains("prompt p ="));
        assert!(main.contains("?p"));
    }

    #[test]
    fn scaffold_unknown_template_falls_back_to_basic() {
        let tmp = TempDir::new("new_fallback");
        let dir = tmp.path().join("app");
        scaffold(&dir, "app", "does-not-exist");
        let main = std::fs::read_to_string(dir.join("main.jde")).unwrap();
        // Non-"llm" templates use the basic (print) body.
        assert!(main.contains("print("));
        assert!(!main.contains("prompt"));
    }

    #[test]
    fn every_scaffolded_project_is_already_formatted() {
        // `jade new` is often a user's first look at Jade. Whatever it writes is
        // the shape the formatter would leave, or `jade fmt` reformats a project
        // the moment it is created.
        for template in ["basic", "llm"] {
            let tmp = TempDir::new(&format!("new_fmt_{template}"));
            let dir = tmp.path().join("app");
            scaffold(&dir, "app", template);
            let main = std::fs::read_to_string(dir.join("main.jde")).unwrap();
            assert_eq!(
                crate::cli::fmt::format_source(&main),
                main,
                "the {template} template is not formatted"
            );
        }
    }
}

// ── build::compare_sources ────────────────────────────────────────────────────
//
// The decision behind `[package] sources`: does the declared file list match
// what the entry actually imports. Both mismatches are things the import graph
// cannot report on its own — a declared file nothing reaches never lands in the
// artifact, and a reached file nobody declared ships without being decided on.

mod package_sources {
    use crate::cli::build::compare_sources;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    /// Declared names and reached names, both relative to the same fake root.
    /// Paths that do not exist canonicalize to themselves, which is exactly the
    /// comparison being tested.
    fn check(declared: &[&str], reached: &[&str]) -> Result<(), String> {
        let root = Path::new("/proj");
        let declared: Vec<String> = declared.iter().map(|s| s.to_string()).collect();
        let reached: HashSet<PathBuf> = reached.iter().map(|s| root.join(s)).collect();
        compare_sources(root, &declared, &reached, "mathlib.jde")
    }

    #[test]
    fn a_matching_list_passes() {
        assert!(check(&["mathlib.jde", "geometry.jde"], &["mathlib.jde", "geometry.jde"]).is_ok());
    }

    #[test]
    fn order_does_not_matter() {
        // sources is an inventory, not a build order — the import graph decides
        // what compiles when.
        assert!(check(&["geometry.jde", "mathlib.jde"], &["mathlib.jde", "geometry.jde"]).is_ok());
    }

    #[test]
    fn a_declared_file_nothing_imports_is_reported() {
        let err = check(&["mathlib.jde", "orphan.jde"], &["mathlib.jde"]).unwrap_err();
        assert!(err.contains("orphan.jde"), "error should name the file: {err}");
        assert!(err.contains("declared but never imported"), "unexpected message: {err}");
        assert!(err.contains("mathlib.jde"), "error should name the entry it walked from: {err}");
    }

    #[test]
    fn an_imported_file_nobody_declared_is_reported() {
        let err = check(&["mathlib.jde"], &["mathlib.jde", "text.jde"]).unwrap_err();
        assert!(err.contains("text.jde"), "error should name the file: {err}");
        assert!(err.contains("imported but not declared"), "unexpected message: {err}");
        assert!(err.contains("add them to sources"), "error should say how to fix it: {err}");
    }

    #[test]
    fn both_directions_are_reported_at_once() {
        // Editing a manifest by hand should not become a sequence of one-error
        // builds, the same reasoning as pkg::verify_in_sync.
        let err = check(&["mathlib.jde", "orphan.jde"], &["mathlib.jde", "text.jde"]).unwrap_err();
        assert!(err.contains("orphan.jde"), "should report the declared-only file: {err}");
        assert!(err.contains("text.jde"), "should report the reached-only file: {err}");
    }

    #[test]
    fn a_nested_source_is_named_the_way_the_manifest_writes_it() {
        // Reached paths are absolute; the error has to echo them back
        // project-relative or the user cannot find them in jade.toml.
        let err = check(&["mathlib.jde"], &["mathlib.jde", "internal/helper.jde"]).unwrap_err();
        assert!(
            err.contains("internal/helper.jde"),
            "should render relative to the project root: {err}"
        );
        assert!(!err.contains("/proj/"), "should not leak the absolute path: {err}");
    }

    #[test]
    fn every_mismatch_is_listed_not_just_the_first() {
        let err = check(&["mathlib.jde", "a.jde", "b.jde"], &["mathlib.jde"]).unwrap_err();
        assert!(err.contains("a.jde") && err.contains("b.jde"), "both should appear: {err}");
    }
}
