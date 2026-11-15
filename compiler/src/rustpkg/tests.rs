//! The generated crate, cargo's output, the records and the layout check, each
//! tested on its own, and one real build of `examples/ffi/rust_values`.

use std::collections::BTreeMap;
use std::path::Path;

use super::cargo;
use super::meta::{self, Field, Records, Struct};
use crate::project::{Abi, DependencyEntry};

fn rust_dep() -> DependencyEntry {
    DependencyEntry { abi: Some(Abi::Rust), ..Default::default() }
}

#[test]
fn the_generated_manifest_names_each_source_the_way_cargo_reads_it() {
    let root = std::env::temp_dir().join(format!("jade_rustpkg_manifest_{}", std::process::id()));
    std::fs::create_dir_all(root.join("crates/geo")).unwrap();
    let mut deps = BTreeMap::new();
    deps.insert(
        "geo".to_string(),
        DependencyEntry { path: Some("crates/geo".into()), ..rust_dep() },
    );
    deps.insert(
        "zlib".to_string(),
        DependencyEntry {
            git: Some("https://example.com/z.git".into()),
            tag: Some("v1".into()),
            package: Some("zlib-jade".into()),
            ..rust_dep()
        },
    );
    deps.insert("hex".to_string(), DependencyEntry { version: Some("0.4.3".into()), ..rust_dep() });
    let text = cargo::manifest("jade_program_x", &root, &deps).unwrap();
    let parsed: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(parsed["lib"]["crate-type"][0].as_str(), Some("staticlib"));
    assert_eq!(parsed["profile"]["release"]["panic"].as_str(), Some("unwind"));
    assert!(parsed.get("workspace").is_some(), "its own workspace: {text}");
    let d = &parsed["dependencies"];
    assert!(d["geo"]["path"].as_str().unwrap().ends_with("crates/geo"));
    assert_eq!(d["zlib"]["tag"].as_str(), Some("v1"));
    assert_eq!(d["zlib"]["package"].as_str(), Some("zlib-jade"));
    // Exact, as every Jade dependency's version is.
    assert_eq!(d["hex"]["version"].as_str(), Some("=0.4.3"));
    assert_eq!(cargo::lib_rs(&deps).matches("extern crate").count(), 3);

    // A path that is not there is named, not left for cargo to report.
    deps.insert("gone".to_string(), DependencyEntry { path: Some("nope".into()), ..rust_dep() });
    let err = cargo::manifest("jade_program_x", &root, &deps).unwrap_err();
    assert!(err.contains("'gone'"), "{err}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn each_project_gets_its_own_crate_and_every_project_shares_a_target() {
    let a = cargo::places(Path::new("/home/me/.jade"), Path::new("/work/a"));
    let b = cargo::places(Path::new("/home/me/.jade"), Path::new("/work/b"));
    assert_ne!(a.crate_dir, b.crate_dir);
    assert_ne!(a.crate_name, b.crate_name);
    assert_eq!(a.target_dir, b.target_dir);
    assert!(a.crate_name.starts_with("jade_program_"));
}

#[test]
fn cargo_output_gives_the_staticlib_and_its_system_libraries() {
    let manifest = Path::new("/nowhere/gen/Cargo.toml");
    let stdout = r#"{"reason":"compiler-artifact","manifest_path":"/elsewhere/Cargo.toml","filenames":["/t/libother.rlib"]}
{"reason":"compiler-artifact","manifest_path":"/nowhere/gen/Cargo.toml","filenames":["/t/release/libjade_program_x.a"]}
{"reason":"build-finished","success":true}"#;
    assert_eq!(
        cargo::staticlib_from(stdout, manifest).unwrap(),
        Path::new("/t/release/libjade_program_x.a")
    );
    let stderr = "   Compiling geo v0.1.0\nnote: native-static-libs: -lSystem -lc -lm\n";
    assert_eq!(cargo::native_libs_from(stderr), ["-lSystem", "-lc", "-lm"]);
    // With `CARGO_TERM_COLOR=always`, as CI's Rust setup sets, the note starts
    // with color codes; it is still read, so Linux links Rust's unwinder.
    let colored =
        "\u{1b}[1m\u{1b}[92mnote\u{1b}[0m\u{1b}[1m: native-static-libs: -lgcc_s -lz -lc\u{1b}[0m\n";
    assert_eq!(cargo::native_libs_from(colored), ["-lgcc_s", "-lz", "-lc"]);
    assert_eq!(cargo::diagnostics(stderr), "");
    let failed = "   Compiling geo v0.1.0\nerror[E0277]: `HashMap<i64, i64>` cannot cross\n";
    assert!(cargo::diagnostics(failed).starts_with("error[E0277]"));
}

#[test]
fn cargo_metadata_maps_each_dependency_to_its_crate() {
    let v: serde_json::Value = serde_json::from_str(
        r#"{
          "packages": [
            {"id": "gen", "targets": [{"kind": ["staticlib"], "name": "jade_program_x"}]},
            {"id": "z", "targets": [{"kind": ["lib"], "name": "zlib-jade"}]}
          ],
          "resolve": {"root": "gen", "nodes": [
            {"id": "gen", "deps": [{"name": "zlib", "pkg": "z"}]}
          ]}
        }"#,
    )
    .unwrap();
    let names = cargo::crate_names_from(&v);
    assert_eq!(names["zlib"], "zlib_jade");
}

/// Purpose: This function writes one record the way `jade-rs` does.
fn record(json: &str) -> Vec<u8> {
    let mut out = b"JADEMETA".to_vec();
    out.extend((json.len() as u32).to_le_bytes());
    out.extend(json.as_bytes());
    out
}

#[test]
fn a_section_splits_into_records_past_the_padding_between_them() {
    let mut section = record("{\"a\":1}");
    section.extend([0, 0, 0]);
    section.extend(record("{\"b\":2}"));
    assert_eq!(meta::split_records(&section).unwrap(), ["{\"a\":1}", "{\"b\":2}"]);
    assert!(meta::split_records(b"garbage!").is_err());
    let mut cut = record("{\"a\":1}");
    cut.truncate(cut.len() - 1);
    assert!(meta::split_records(&cut).is_err());
}

fn point(offsets: [u64; 3], size: u64) -> Struct {
    Struct {
        krate: "geo".into(),
        name: "Point".into(),
        size,
        align: 8,
        fields: vec![
            Field { name: "x".into(), ty: "float".into(), offset: offsets[0] },
            Field { name: "tag".into(), ty: "byte?".into(), offset: offsets[1] },
            Field { name: "name".into(), ty: "str".into(), offset: offsets[2] },
        ],
    }
}

#[test]
fn a_struct_laid_out_as_c_lays_it_out_passes_and_any_other_is_refused() {
    let mut r = Records::default();
    r.structs.insert("geo::Point".into(), point([0, 8, 16], 40));
    assert!(meta::check(&r).is_ok());

    r.structs.insert("geo::Point".into(), point([0, 8, 24], 48));
    let err = meta::check(&r).unwrap_err();
    assert!(err.contains("geo::Point") && err.contains("laid out differently"), "{err}");
}

#[test]
fn a_function_naming_a_type_with_no_c_form_is_refused() {
    let mut r = Records::default();
    let f = meta::Function {
        krate: "geo".into(),
        name: "nest".into(),
        symbol: "__jade_geo_nest".into(),
        params: vec![meta::Param { name: "x".into(), ty: "int??".into() }],
        ret: "nil".into(),
        raises: false,
    };
    r.functions.entry("geo".into()).or_default().insert("nest".into(), f);
    let err = meta::check(&r).unwrap_err();
    assert!(err.contains("geo.nest") && err.contains("one nil"), "{err}");
}

#[test]
fn a_record_of_another_format_names_the_jade_rs_to_use() {
    let mut r = Records::default();
    let err = meta::add_record(&mut r, "{\"v\":99,\"kind\":\"fn\"}").unwrap_err();
    assert!(err.contains("format 99") && err.contains("jade-rs"), "{err}");
    let err = meta::add_record(&mut r, "{\"v\":1,\"kind\":\"enum\"}").unwrap_err();
    assert!(err.contains("'enum'"), "{err}");
}

/// One real build, the way `jade check` does it: cargo builds the generated
/// crate and the records come out of the staticlib.
#[test]
fn building_the_rust_values_example_reads_every_export() {
    if cargo::cargo_program().is_err() {
        eprintln!("skipped: no cargo");
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/ffi/rust_values");
    let manifest = crate::project::load_project(&root).unwrap();
    let built = super::build(&root, &manifest).unwrap().unwrap();
    let pkg = &built.packages["values"];
    assert_eq!(pkg.krate, "values");
    let add = &pkg.functions["add"];
    assert_eq!(add.symbol, "__jade_values_add");
    assert_eq!(add.params.len(), 2);
    assert_eq!(add.ret, "int");
    assert!(pkg.functions["parse_port"].raises);
    assert_eq!(pkg.functions["find"].ret, "str?");
    assert_eq!(pkg.structs["Point"], "values::Point");
    assert!(built.staticlib.is_file());
}
