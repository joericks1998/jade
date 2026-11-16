//! Tests for the std library written in Jade: the registry, each module
//! compiling, and the hooks' table.

use super::*;

/// Every module parses and passes the per-file pass in library mode.
#[test]
fn every_library_module_compiles_on_its_own() {
    for lib in LIBRARIES {
        if let Err(e) = lib.statements() {
            panic!("std::{} does not compile: {e}", lib.name);
        }
    }
}

/// A package's public functions are its top-level `fn`s without a leading
/// underscore, in source order, which is the order a message lists them in.
#[test]
fn public_functions_come_from_the_source_in_order() {
    let path = find("std/path").expect("path is written in Jade");
    assert_eq!(path.public_fns(), ["join", "basename", "dirname", "ext", "stem", "abs", "is_abs"]);
    let time = by_name("time").expect("time is written in Jade");
    assert_eq!(
        time.public_fns(),
        ["now", "now_ms", "monotonic", "sleep", "after", "local", "utc", "parts", "stamp"]
    );
    assert!(!time.has_fn("_days_from_civil"));
    let math = by_name("math").expect("math is written in Jade");
    assert_eq!(math.public_fns().len(), 30);
    assert_eq!(math.public_fns()[0], "floor");
    assert_eq!(math.public_fns()[29], "nan");
}

/// The packages phase B of 2.0.8 moved keep the functions the backend's
/// registry listed, in its order, which is the order a message lists them in.
#[test]
fn moved_packages_keep_their_functions_in_order() {
    let want: [(&str, &[&str]); 6] = [
        ("env", &["get", "set", "args", "cwd"]),
        (
            "fs",
            &[
                "read_stdin_bytes",
                "write_stdout_bytes",
                "read_bytes",
                "write_bytes",
                "append_bytes",
                "read",
                "write",
                "append",
                "exists",
                "delete",
                "list_dir",
                "mkdir",
                "is_file",
                "is_dir",
                "copy",
                "rename",
                "rmdir",
                "size",
            ],
        ),
        ("sh", &["exec", "run", "output"]),
        ("http", &["get", "get_bytes", "post_bytes", "post", "put", "delete", "head"]),
        ("uhttp", &["get", "post", "put", "delete", "head", "get_bytes", "post_bytes", "stream"]),
        ("random", &["int", "float", "choice", "shuffle", "seed"]),
    ];
    for (name, fns) in want {
        let lib = by_name(name).unwrap_or_else(|| panic!("{name} is written in Jade"));
        assert_eq!(lib.public_fns(), fns, "std::{name}");
    }
}

/// A library module that uses another reaches its private functions, as
/// `uhttp.jde` calls `http._fetch`; a program reaches only the public ones.
#[test]
fn only_library_code_reaches_a_modules_private_functions() {
    let tokens = crate::frontend::lexer::tokenize("use std::uhttp\n").expect("lexes");
    let ast = crate::frontend::parser::parse(tokens).expect("parses");
    let tree = crate::typechecker::resolve::resolve(ast).expect("resolves");
    let stmts = crate::mir::imports::namespace_program(tree.stmts).expect("namespaces");
    let http = by_name("http").expect("http is written in Jade").id();
    let fetch = format!("_fetch${http}");
    assert!(stmts.iter().any(|s| matches!(s, Stmt::FnDef { name, .. } if *name == fetch)));
    assert_eq!(display(&fetch).as_deref(), Some("http._fetch"));

    let src = "use std::http\nhttp._fetch(\"GET\", \"x\", nil, nil, false)\n";
    let tokens = crate::frontend::lexer::tokenize(src).expect("lexes");
    let ast = crate::frontend::parser::parse(tokens).expect("parses");
    let e = crate::typechecker::resolve::resolve(ast).map(|_| ()).expect_err("refused");
    assert!(e.to_string().contains("has no function '_fetch'"), "{e}");
}

/// Each package's namespace id is its own, and maps back to it.
#[test]
fn namespace_ids_are_distinct_and_map_back() {
    let mut seen = std::collections::HashSet::new();
    for lib in LIBRARIES {
        assert!(seen.insert(lib.id()), "two packages share id {}", lib.id());
        assert!(std::ptr::eq(by_id(lib.id()).expect("maps back"), lib));
        assert_ne!(lib.id(), crate::infer::rewrite::PRELUDE_ID);
    }
    let join = format!("join${}", find("std/path").map(Library::id).unwrap_or(0));
    assert_eq!(display(&join).as_deref(), Some("path.join"));
    assert!(folds(&join));
    assert!(!is_library_name("join$3"));
    assert!(display("join").is_none());
}

/// A second copy of a library module is dropped; a program's own names stay.
#[test]
fn dedupe_keeps_one_copy_of_each_library_definition() {
    let tokens = crate::frontend::lexer::tokenize("use std::path\n").expect("lexes");
    let ast = crate::frontend::parser::parse(tokens).expect("parses");
    let tree = crate::typechecker::resolve::resolve(ast).expect("resolves");
    let one = crate::mir::imports::namespace_program(tree.stmts).expect("namespaces");
    assert!(one.iter().any(|s| matches!(s, Stmt::FnDef { name, .. } if is_library_name(name))));
    let mut twice = one.clone();
    twice.extend(one.clone());
    assert_eq!(dedupe(twice).len(), one.len());
}

/// Every hook's name maps back to it, and each has a type.
#[test]
fn every_hook_has_a_name_and_a_type() {
    use crate::mir::SysFn;
    for h in SysFn::ALL {
        assert_eq!(SysFn::from_name(h.name()), Some(*h), "{h:?}");
        let _ = (h.params(), h.result());
    }
    assert!(SysFn::from_name("system").is_none());
}
