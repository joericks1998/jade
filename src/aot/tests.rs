//! Tests for the two compilation modes. These stop at IR (`emit_ir = true`) so
//! they need LLVM but not a linker or a build daemon.

use super::*;

fn tir(src: &str) -> TProgram {
    let tokens = crate::frontend::lexer::tokenize(src).expect("lex");
    let program = crate::frontend::parser::parse(tokens).expect("parse");
    crate::compiler::type_infer::infer(program).expect("infer")
}

fn ir(src: &str, mode: CompileMode) -> String {
    compile_with_mode(tir(src), None, Path::new("mylib"), true, mode)
        .map(|o| o.ir)
        .expect("compilation should succeed")
        .expect("emit_ir returns the IR")
}

const LIB: &str = "fn add(a, b) { return a + b }\nfn triple(x) { return x * 3 }\n";

// ── Binary mode ───────────────────────────────────────────────────────────────

#[test]
fn binary_mode_emits_main_and_no_pkg_init() {
    let out = ir(LIB, CompileMode::Binary);
    assert!(out.contains("define i32 @main("), "no main:\n{out}");
    assert!(!out.contains("@jade_pkg_init"), "a binary must not export jade_pkg_init");
}

// ── Shared-library mode ───────────────────────────────────────────────────────

#[test]
fn shared_lib_emits_pkg_init_and_no_main() {
    let out = ir(LIB, CompileMode::SharedLib { exports: vec![] });
    assert!(out.contains("define i32 @jade_pkg_init("), "no pkg_init:\n{out}");
    assert!(!out.contains("define i32 @main("), "a shared library must not define main:\n{out}");
}

#[test]
fn an_empty_export_list_exports_every_function() {
    // Jade has no `pub`, so everything top-level is public by construction.
    let out = ir(LIB, CompileMode::SharedLib { exports: vec![] });
    assert!(out.contains(r#""jade_export$add""#), "add not exported:\n{out}");
    assert!(out.contains(r#""jade_export$triple""#), "triple not exported:\n{out}");
}

/// Every package carries the value ABI it was built against, so a host can refuse
/// one it cannot talk to.
///
/// The symbol is emitted rather than borrowed from the linked runtime because the
/// linker drops `jrt_abi_version` from a package that never calls it. Losing this
/// test would mean losing the check silently: a package with no version symbol is
/// *allowed* to load, since that is how a plain C shim looks.
#[test]
fn a_shared_library_declares_the_abi_it_was_built_against() {
    let out = ir(LIB, CompileMode::SharedLib { exports: vec![] });
    assert!(out.contains("@jade_pkg_abi_version"), "a package must declare its value ABI:\n{out}");
    assert!(
        out.contains(&format!("ret i32 {}", jade_runtime::RUNTIME_ABI_VERSION)),
        "the declared ABI must be this runtime's ({}):\n{out}",
        jade_runtime::RUNTIME_ABI_VERSION
    );
}

/// A binary has no package ABI to declare — the symbol is part of the package
/// surface, not of every emitted object.
#[test]
fn a_binary_declares_no_package_abi() {
    let out = ir(LIB, CompileMode::Binary);
    assert!(!out.contains("@jade_pkg_abi_version"), "binaries should not carry it:\n{out}");
}

#[test]
fn an_export_list_narrows_the_bindings() {
    let out = ir(LIB, CompileMode::SharedLib { exports: vec!["add".to_string()] });
    assert!(out.contains(r#""jade_export$add""#), "add not exported:\n{out}");
    assert!(
        !out.contains(r#""jade_export$triple""#),
        "triple should have been filtered out:\n{out}"
    );
}

#[test]
fn exporting_an_unknown_function_names_it() {
    let err = compile_with_mode(
        tir(LIB),
        None,
        Path::new("mylib"),
        true,
        CompileMode::SharedLib { exports: vec!["nope".to_string()] },
    )
    .unwrap_err();
    assert!(err.contains("nope"), "error should name the function: {err}");
}

#[test]
fn a_file_with_no_functions_cannot_be_a_package() {
    let err = compile_with_mode(
        tir("let x = 1\n"),
        None,
        Path::new("mylib"),
        true,
        CompileMode::SharedLib { exports: vec![] },
    )
    .unwrap_err();
    assert!(err.contains("no top-level functions"), "unexpected message: {err}");
}

#[test]
fn wrappers_marshal_through_the_ffi_helpers() {
    // The lowered functions speak the tagged word; the host speaks JadeVal.
    let out = ir(LIB, CompileMode::SharedLib { exports: vec![] });
    assert!(out.contains("@jrt_ffi_to_tagged"), "no inbound marshalling:\n{out}");
    assert!(out.contains("@jrt_ffi_from_tagged"), "no outbound marshalling:\n{out}");
}

#[test]
fn both_modes_share_one_initializer() {
    // Binary and package must initialize identically, or a package would skip
    // the native-package dlopen prologue that a binary runs.
    for mode in [CompileMode::Binary, CompileMode::SharedLib { exports: vec![] }] {
        let out = ir(LIB, mode);
        assert!(out.contains("define void @jade_mod_init()"), "missing initializer:\n{out}");
        assert!(out.contains("call void @jade_mod_init()"), "initializer never called:\n{out}");
    }
}

#[test]
fn pkg_init_runs_the_module_body_only_once() {
    // A host may call jade_pkg_init more than once; re-running the top level
    // would repeat its side effects.
    let out = ir(LIB, CompileMode::SharedLib { exports: vec![] });
    assert!(out.contains("@jade_pkg_inited"), "no once-guard:\n{out}");
}

// ── Integration: a Jade package that actually loads ───────────────────────────

/// End-to-end for `jade build --lib`: lower a Jade file to a real shared
/// library, then load it through the same loader a consumer project uses. This
/// is the only test that exercises linking and `dlopen` together.
#[test]
fn a_compiled_jade_package_loads_and_binds_its_exports() {
    let dir = std::env::temp_dir().join(format!("jade_libmode_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    let out = dir.join(format!("mathpkg.{ext}"));

    let result =
        compile_with_mode(tir(LIB), None, &out, false, CompileMode::SharedLib { exports: vec![] })
            .map(|o| o.ir);
    assert!(result.is_ok(), "linking a package failed: {:?}", result.err());
    assert!(out.exists(), "no shared library was produced at {}", out.display());

    let span = crate::frontend::error::Span { line: 0, col: 0 };
    let pkg = crate::native::load_native_package(&out, span)
        .expect("a compiled Jade package must satisfy the native package ABI");

    assert!(pkg.contains_key("add"), "missing export: {:?}", pkg.keys().collect::<Vec<_>>());
    assert!(pkg.contains_key("triple"), "missing export: {:?}", pkg.keys().collect::<Vec<_>>());

    let _ = std::fs::remove_dir_all(&dir);
}

// ── Finding a dependency after the artifact moves ─────────────────────────
//
// A compiled artifact used to name every dependency by the absolute path it sat
// at when it was built, so it worked on the machine that produced it and nowhere
// else. These pin the two halves of the replacement: the key the artifact
// carries, and the single root every image resolves it against.

/// A project with one locked dependency, built from a Jade package.
///
/// Returns `(project root, the built dependency's install dir)`. Written out
/// rather than reusing `pkg::tests` scaffolding because what matters here is the
/// *shape on disk* — a real `libs/<name>-<version>/` that import resolution will
/// find — not the lock bookkeeping around it.
fn project_with_dependency(tag: &str, dep_src: &str, main_src: &str) -> std::path::PathBuf {
    let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    let root = std::env::temp_dir().join(format!("jade_reloc_{}_{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let libs = root.join("libs").join("dep-local");
    std::fs::create_dir_all(&libs).unwrap();

    let dep_out = libs.join(format!("dep.{ext}"));
    compile_with_mode(
        tir(dep_src),
        None,
        &dep_out,
        false,
        CompileMode::SharedLib { exports: vec![] },
    )
    .expect("the dependency should build");

    std::fs::write(
        root.join("jade.toml"),
        format!(
            "[project]\nname = \"reloc\"\n\n[lib.dep]\npath = \"libs/dep-local\"\nfiles = [\"dep.{ext}\"]\n"
        ),
    )
    .unwrap();
    std::fs::write(root.join("main.jde"), main_src).unwrap();
    root
}

#[test]
fn a_dependency_is_named_by_a_libs_relative_key_not_an_absolute_path() {
    let root = project_with_dependency(
        "key",
        "fn value() { return 7 }\n",
        "use dep\nprint(dep.value())\n",
    );
    let out = compile_with_mode(
        tir(&std::fs::read_to_string(root.join("main.jde")).unwrap()),
        Some(&root.join("main.jde")),
        &root.join("app"),
        true,
        CompileMode::Binary,
    )
    .expect("compile")
    .ir
    .expect("emit_ir returns the IR");

    let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    assert!(out.contains("jrt_native_load_rel"), "should load by key:\n{out}");
    assert!(out.contains(&format!("dep-local/dep.{ext}")), "no relative key:\n{out}");
    // A binary is a host, so it says where the libraries are. Exactly once.
    assert_eq!(
        out.matches("jrt_libs_root_publish").count(),
        2,
        "publish should be declared and called once:\n{out}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_package_does_not_publish_a_libs_root() {
    // Only a host may: it owns the process, so it is the one thing entitled to
    // decide the root every image resolves against. A second publisher would be
    // a second root, and a second root is a second copy of a dependency.
    let root = project_with_dependency(
        "nopublish",
        "fn value() { return 7 }\n",
        "use dep\nfn wrapped() { return dep.value() }\n",
    );
    let out = compile_with_mode(
        tir(&std::fs::read_to_string(root.join("main.jde")).unwrap()),
        Some(&root.join("main.jde")),
        &root.join("pkg"),
        true,
        CompileMode::SharedLib { exports: vec![] },
    )
    .expect("compile")
    .ir
    .expect("emit_ir returns the IR");

    assert!(out.contains("jrt_native_load_rel"), "should still load by key:\n{out}");
    assert!(!out.contains("jrt_libs_root_publish"), "a package must not publish a root:\n{out}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_library_outside_libs_keeps_its_absolute_path() {
    // A hand-written `[lib]` pointing anywhere on disk is not a dependency
    // artifact, has no `libs/`-relative spelling, and never had one. It must keep
    // working exactly as it did.
    let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    let root = std::env::temp_dir().join(format!("jade_adhoc_{}", std::process::id()));
    let elsewhere = root.join("vendor");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&elsewhere).unwrap();

    let dep_out = elsewhere.join(format!("adhoc.{ext}"));
    compile_with_mode(
        tir("fn value() { return 3 }\n"),
        None,
        &dep_out,
        false,
        CompileMode::SharedLib { exports: vec![] },
    )
    .expect("build");

    std::fs::write(
        root.join("jade.toml"),
        format!("[project]\nname = \"adhoc\"\n\n[lib.adhoc]\npath = \"vendor\"\nfiles = [\"adhoc.{ext}\"]\n"),
    )
    .unwrap();
    let main = root.join("main.jde");
    std::fs::write(&main, "use adhoc\nprint(adhoc.value())\n").unwrap();

    let out = compile_with_mode(
        tir(&std::fs::read_to_string(&main).unwrap()),
        Some(&main),
        &root.join("app"),
        true,
        CompileMode::Binary,
    )
    .expect("compile")
    .ir
    .expect("ir");

    // Null key, real path: the second argument is all there is.
    assert!(out.contains(&dep_out.to_string_lossy().to_string()), "absolute path missing:\n{out}");

    let _ = std::fs::remove_dir_all(&root);
}

/// Link a binary from a project, bundle its dependencies beside it, and run it
/// from a directory that has nothing to do with the build.
///
/// The move is the whole point: an artifact that only works where it was built
/// is what this machinery exists to stop, and nothing catches that without
/// actually taking the build tree away.
fn build_bundle_and_run(root: &std::path::Path, ship: &std::path::Path) -> std::process::Output {
    let main = root.join("main.jde");
    let app = ship.join("app");
    std::fs::create_dir_all(ship).unwrap();
    compile_with_mode(
        tir(&std::fs::read_to_string(&main).unwrap()),
        Some(&main),
        &app,
        false,
        CompileMode::Binary,
    )
    .expect("the program should build");

    let libs = std::fs::canonicalize(root.join("libs")).unwrap();
    crate::pkg::bundle_beside_artifact(&app, &libs).expect("bundling should succeed");

    // Moved rather than deleted, so a failure can be inspected.
    let stashed = root.with_extension("moved");
    let _ = std::fs::remove_dir_all(&stashed);
    std::fs::rename(root, &stashed).unwrap();

    let out = std::process::Command::new(&app)
        .current_dir(std::env::temp_dir())
        .output()
        .expect("the shipped binary should run");
    std::fs::rename(&stashed, root).unwrap();
    out
}

#[test]
fn a_shipped_binary_runs_with_the_build_tree_gone() {
    let root = project_with_dependency(
        "ship",
        "fn value() { return 7 }\n",
        "use dep\nprint(dep.value())\n",
    );
    let ship = std::env::temp_dir().join(format!("jade_ship_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ship);

    let out = build_bundle_and_run(&root, &ship);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "run failed: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout.trim(), "7", "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&ship);
}

#[test]
fn a_missing_bundle_says_where_it_looked_and_why() {
    // A bare dyld error names the file and nothing else, which is the same
    // message whether the dependency is missing or the root is wrong.
    let root = project_with_dependency(
        "missing",
        "fn value() { return 7 }\n",
        "use dep\nprint(dep.value())\n",
    );
    let ship = std::env::temp_dir().join(format!("jade_missing_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ship);
    std::fs::create_dir_all(&ship).unwrap();
    let app = ship.join("app");
    let main = root.join("main.jde");
    compile_with_mode(
        tir(&std::fs::read_to_string(&main).unwrap()),
        Some(&main),
        &app,
        false,
        CompileMode::Binary,
    )
    .expect("build");

    // Deliberately not bundled, and the project taken away.
    let stashed = root.with_extension("moved");
    let _ = std::fs::remove_dir_all(&stashed);
    std::fs::rename(&root, &stashed).unwrap();
    let out =
        std::process::Command::new(&app).current_dir(std::env::temp_dir()).output().expect("run");
    std::fs::rename(&stashed, &root).unwrap();

    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "should have failed");
    assert!(err.contains("dep-local"), "should name the dependency: {err}");
    assert!(err.contains("no libraries directory"), "should say there is no root: {err}");

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&ship);
}

#[test]
fn a_root_without_the_dependency_says_where_it_looked_and_why() {
    // The other half of the failure: there *is* a root, and the dependency is
    // not in it. Naming where the root came from is what separates "you shipped
    // an incomplete bundle" from "you pointed JADE_LIBS somewhere wrong", which
    // a bare dyld error cannot.
    let root = project_with_dependency(
        "wrongroot",
        "fn value() { return 7 }
",
        "use dep
print(dep.value())
",
    );
    let ship = std::env::temp_dir().join(format!("jade_wrongroot_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ship);
    std::fs::create_dir_all(ship.join("libs")).unwrap();
    let app = ship.join("app");
    let main = root.join("main.jde");
    compile_with_mode(
        tir(&std::fs::read_to_string(&main).unwrap()),
        Some(&main),
        &app,
        false,
        CompileMode::Binary,
    )
    .expect("build");

    let stashed = root.with_extension("moved");
    let _ = std::fs::remove_dir_all(&stashed);
    std::fs::rename(&root, &stashed).unwrap();
    let out =
        std::process::Command::new(&app).current_dir(std::env::temp_dir()).output().expect("run");
    std::fs::rename(&stashed, &root).unwrap();

    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "should have failed");
    assert!(err.contains("where this program looks"), "should name the root: {err}");
    assert!(err.contains("the bundle beside this program"), "should say where it came from: {err}");

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&ship);
}

#[test]
fn a_user_set_root_is_deferred_to_and_a_wrong_one_does_not_fall_back() {
    // The variable is how a process with no Jade host — a C program embedding a
    // package — gets one agreed root at all, so a host must never overwrite it.
    // The cost is that a wrong one fails rather than being silently rescued, and
    // that is deliberate: the rescue path is a second root, which is a second
    // copy of the dependency.
    let root = project_with_dependency(
        "userroot",
        "fn value() { return 7 }
",
        "use dep
print(dep.value())
",
    );
    let ship = std::env::temp_dir().join(format!("jade_userroot_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ship);
    let app = ship.join("app");
    std::fs::create_dir_all(&ship).unwrap();
    let main = root.join("main.jde");
    compile_with_mode(
        tir(&std::fs::read_to_string(&main).unwrap()),
        Some(&main),
        &app,
        false,
        CompileMode::Binary,
    )
    .expect("build");
    let libs = std::fs::canonicalize(root.join("libs")).unwrap();
    crate::pkg::bundle_beside_artifact(&app, &libs).expect("bundle");

    // Set on the child, never on the test process: `cargo test` is parallel and
    // `std::env::set_var` is a real data race.
    let elsewhere = std::env::temp_dir().join(format!("jade_empty_{}", std::process::id()));
    std::fs::create_dir_all(&elsewhere).unwrap();
    let wrong = std::process::Command::new(&app)
        .env("JADE_LIBS", &elsewhere)
        .current_dir(std::env::temp_dir())
        .output()
        .expect("run");
    assert!(!wrong.status.success(), "a wrong root must not be rescued by the bundle");
    let err = String::from_utf8_lossy(&wrong.stderr);
    assert!(err.contains("JADE_LIBS"), "should say the root came from the variable: {err}");

    // And pointed at the real one, it works.
    let right = std::process::Command::new(&app)
        .env("JADE_LIBS", ship.join("libs"))
        .current_dir(std::env::temp_dir())
        .output()
        .expect("run");
    assert!(right.status.success(), "stderr: {}", String::from_utf8_lossy(&right.stderr));
    assert_eq!(String::from_utf8_lossy(&right.stdout).trim(), "7");

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&ship);
    let _ = std::fs::remove_dir_all(&elsewhere);
}

#[test]
fn a_shared_dependency_is_loaded_once_not_once_per_package() {
    // The property the single-root design exists for, and the only one ordinary
    // testing cannot see: two packages that both use one dependency must get the
    // *same* copy of it. A second copy has its own globals and runs its own
    // initializer, so for a library that owns a device or a graphics context two
    // copies are two devices — a correctness problem in the operating system,
    // not a memory one.
    //
    // The dependency's module top level prints once per load, so counting the
    // line counts the instances.
    let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    let root = std::env::temp_dir().join(format!("jade_once_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    let build_pkg = |dir: &std::path::Path, src: &str, out: &std::path::Path| {
        std::fs::create_dir_all(dir).unwrap();
        compile_with_mode(tir(src), None, out, false, CompileMode::SharedLib { exports: vec![] })
            .expect("package should build");
    };

    // One dependency, and two packages that each reach it through their own
    // project — so nothing but the shared root makes them agree.
    let libs = root.join("libs");
    build_pkg(
        &libs.join("dep-local"),
        "print(\"[loaded]\")\n\nfn value() { return 7 }\n",
        &libs.join("dep-local").join(format!("dep.{ext}")),
    );

    let lib_toml = |name: &str, deps: &[&str]| {
        let mut t = format!("[project]\nname = \"{name}\"\n");
        for d in deps {
            t.push_str(&format!(
                "\n[lib.{d}]\npath = \"libs/{d}-local\"\nfiles = [\"{d}.{ext}\"]\n"
            ));
        }
        t
    };

    for (name, body) in [("a", "return dep.value() + 1"), ("b", "return dep.value() + 2")] {
        let proj = root.join(format!("pkg{name}"));
        std::fs::create_dir_all(&proj).unwrap();
        // Each package project points at the *same* libs/ directory, which is
        // what a bundle is: one tree, shared.
        std::os::unix::fs::symlink(&libs, proj.join("libs")).unwrap();
        std::fs::write(proj.join("jade.toml"), lib_toml(&format!("pkg{name}"), &["dep"])).unwrap();
        let src = proj.join("main.jde");
        std::fs::write(&src, format!("use dep\n\nfn {name}() {{ {body} }}\n")).unwrap();
        let out = libs.join(format!("pkg{name}-local"));
        std::fs::create_dir_all(&out).unwrap();
        compile_with_mode(
            tir(&std::fs::read_to_string(&src).unwrap()),
            Some(&src),
            &out.join(format!("pkg{name}.{ext}")),
            false,
            CompileMode::SharedLib { exports: vec![] },
        )
        .expect("package should build");
    }

    std::fs::write(root.join("jade.toml"), lib_toml("host", &["pkga", "pkgb"])).unwrap();
    std::fs::write(
        root.join("main.jde"),
        "use pkga\nuse pkgb\n\nprint(pkga.a())\nprint(pkgb.b())\n",
    )
    .unwrap();

    let ship = std::env::temp_dir().join(format!("jade_once_ship_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ship);
    let out = build_bundle_and_run(&root, &ship);

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "run failed: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        stdout.matches("[loaded]").count(),
        1,
        "the shared dependency must be loaded once, not once per package:\n{stdout}"
    );
    // And both packages really did reach it, rather than one silently failing.
    assert!(stdout.contains('8') && stdout.contains('9'), "both should work:\n{stdout}");

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&ship);
}
