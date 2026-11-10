//! Tests for the built-in registry: package lookup, the global names, and the
//! primitive-method tables.

use super::*;

#[test]
fn global_names_cover_the_core_builtins() {
    for name in ["print", "len", "write", "input", "wait", "int", "str", "Grammar"] {
        assert!(global_names().contains(&name), "{name} missing from the global names");
    }
    assert!(!global_names().contains(&"definitely_not_a_builtin"));
}

#[test]
fn find_package_by_import_path() {
    // Every registered package resolves by its `use` import name.
    let expected = [
        ("std/string", "string"),
        ("std/math", "math"),
        ("std/array", "array"),
        ("std/dict", "dict"),
        ("std/fs", "fs"),
        ("std/time", "time"),
        ("std/http", "http"),
        ("std/sh", "sh"),
        ("std/json", "json"),
        ("std/env", "env"),
        ("std/path", "path"),
        ("std/random", "random"),
    ];
    for (import, global) in expected {
        let pkg = find_package(import).unwrap_or_else(|| panic!("missing package {import}"));
        assert_eq!(pkg.import_name, import);
        assert_eq!(pkg.global_name, global);
    }
}

#[test]
fn find_package_uhttp_present_on_unix() {
    let pkg = find_package("std/uhttp").expect("uhttp should be registered on unix");
    assert_eq!(pkg.global_name, "uhttp");
}

#[test]
fn find_package_unknown_returns_none() {
    assert!(find_package("std/nonexistent").is_none());
    assert!(find_package("string").is_none()); // global name, not import name
}

#[test]
fn is_package_global_name_matches_globals() {
    assert!(is_package_global_name("string"));
    assert!(is_package_global_name("math"));
    assert!(is_package_global_name("json"));
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
