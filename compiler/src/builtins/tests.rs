//! Tests for the built-in registry: package lookup, the global names, and the
//! primitive-method tables.

use super::*;

#[test]
fn global_names_cover_the_core_builtins() {
    for name in ["print", "len", "write", "input", "wait", "int", "str", "toDict", "Grammar"] {
        assert!(global_names().contains(&name), "{name} missing from the global names");
    }
    assert!(!global_names().contains(&"definitely_not_a_builtin"));
}

#[test]
fn find_package_by_import_path() {
    // Every registered package resolves by its `use` import name.
    let expected = [
        ("std/string", "string"),
        ("std/array", "array"),
        ("std/dict", "dict"),
        ("std/bytes", "bytes"),
    ];
    for (import, global) in expected {
        let pkg = find_package(import).unwrap_or_else(|| panic!("missing package {import}"));
        assert_eq!(pkg.import_name, import);
        assert_eq!(pkg.global_name, global);
    }
}

/// A package written in Jade is not in the backend's table, and both kinds
/// answer through the shared lookups.
#[test]
fn library_packages_answer_through_the_shared_lookups() {
    let written_in_jade = [
        ("std/math", "math"),
        ("std/time", "time"),
        ("std/path", "path"),
        ("std/fs", "fs"),
        ("std/env", "env"),
        ("std/sh", "sh"),
        ("std/http", "http"),
        ("std/uhttp", "uhttp"),
        ("std/random", "random"),
    ];
    for (import, global) in written_in_jade {
        assert!(find_package(import).is_none(), "{import} is written in Jade now");
        assert!(is_std_package(import));
        assert_eq!(std_global_name(import), Some(global));
    }
    let (path, fns) = std_functions("path").expect("path is a std package");
    assert_eq!(path, "std::path");
    assert_eq!(fns, ["join", "basename", "dirname", "ext", "stem", "abs", "is_abs"]);
    assert!(std_functions("fs").is_some_and(|(_, f)| f.contains(&"read".to_string())));
    assert!(std_functions("nothing").is_none());
}

#[test]
fn find_package_unknown_returns_none() {
    assert!(find_package("std/nonexistent").is_none());
    assert!(find_package("string").is_none()); // global name, not import name
    // std::json was removed in 2.0.7; the per-file pass names what replaced it.
    assert!(find_package("std/json").is_none());
}

#[test]
fn is_package_global_name_matches_globals() {
    assert!(is_package_global_name("string"));
    assert!(is_package_global_name("bytes"));
    // A package written in Jade binds its name through the import pass.
    assert!(!is_package_global_name("fs"));
    assert!(!is_package_global_name("json"));
    assert!(!is_package_global_name("std/string")); // that's the import name
    assert!(!is_package_global_name("not_a_package"));
}

#[test]
fn primitive_methods_all_declare_an_arity() {
    // The tripwire for `primitive_method_arity`. A method added to one of the
    // tables and not to that list would silently go back to accepting any
    // number of arguments, which is the defect it exists to close.
    for (kind, table) in primitive_method_tables() {
        for m in table {
            assert!(
                primitive_method_arity(m).is_some(),
                "{kind}.{m} has no declared arity; add it to primitive_method_arity"
            );
        }
    }
}
