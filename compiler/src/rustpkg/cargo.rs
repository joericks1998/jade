//! The generated crate, and cargo.
//!
//! A project's Rust packages are built as one crate this file writes: a `staticlib`
//! whose dependencies are every `abi = "rust"` entry in `jade.toml`, and whose
//! `lib.rs` is one `extern crate` line per package, so rustc links each. One
//! staticlib per program matters: two would each carry Rust's standard library, and
//! their symbols would collide when LLD links the program.
//!
//! The crate is per project, in `~/.jade/rust/<hash of the project's path>/`, and
//! every project builds into one target directory, `~/.jade/rust/target/`, so
//! `syn` and `jade-rs` compile once per machine rather than once per project. Cargo
//! from `CARGO` or `PATH` builds it; design/v2-rust-packages.md lists fetching a
//! pinned toolchain as the first follow-up.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::project::DependencyEntry;

/// Purpose: This struct holds where a project's generated crate and the shared
/// target directory are.
#[derive(Debug, Clone)]
pub struct Places {
    /// The generated crate's directory.
    pub crate_dir: PathBuf,
    /// The target directory every project builds into.
    pub target_dir: PathBuf,
    /// The generated crate's name, which names its staticlib.
    pub crate_name: String,
}

/// Purpose: This function says where a project's Rust is built.
///
/// Inputs:
/// - `home`: jade's home, `~/.jade`.
/// - `root`: the project's root.
///
/// Outputs: the places, named by a hash of the project's canonical path.
pub fn places(home: &Path, root: &Path) -> Places {
    use sha2::{Digest, Sha256};
    let canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let hash = Sha256::digest(canon.to_string_lossy().as_bytes());
    let key: String = hash.iter().take(8).map(|b| format!("{b:02x}")).collect();
    let base = home.join("rust");
    Places {
        crate_dir: base.join(&key),
        target_dir: base.join("target"),
        crate_name: format!("jade_program_{key}"),
    }
}

/// Purpose: This function writes the generated crate's `Cargo.toml`.
///
/// Inputs:
/// - `crate_name`: the crate's name.
/// - `root`: the project's root, which relative paths are relative to.
/// - `deps`: each Rust dependency by its Jade name.
///
/// Outputs: the manifest's text.
pub fn manifest(
    crate_name: &str,
    root: &Path,
    deps: &BTreeMap<String, DependencyEntry>,
) -> Result<String, String> {
    let mut out = format!(
        "# Written by jade for the project at {}; regenerated on every build.\n\
         [package]\n\
         name = \"{crate_name}\"\n\
         version = \"0.0.0\"\n\
         edition = \"2021\"\n\
         publish = false\n\n\
         [lib]\n\
         path = \"lib.rs\"\n\
         crate-type = [\"staticlib\"]\n\n\
         # A panic must be caught and raised in the program, never turned into an abort.\n\
         # Only the root crate's profile decides this, so a package cannot change it.\n\
         [profile.release]\n\
         panic = \"unwind\"\n\n\
         # Its own workspace, so a workspace around ~/.jade never claims it.\n\
         [workspace]\n\n\
         [dependencies]\n",
        root.display()
    );
    for (name, d) in deps {
        let mut spec: Vec<String> = Vec::new();
        if let Some(p) = &d.path {
            let full = root.join(p);
            let full = full.canonicalize().map_err(|e| {
                format!("the Rust dependency '{name}' is at '{}', which cannot be read ({e})", p)
            })?;
            spec.push(format!("path = {}", toml_str(&full.to_string_lossy())));
        } else if let Some(g) = &d.git {
            spec.push(format!("git = {}", toml_str(g)));
            for (k, v) in [("rev", &d.rev), ("tag", &d.tag), ("branch", &d.branch)] {
                if let Some(v) = v {
                    spec.push(format!("{k} = {}", toml_str(v)));
                }
            }
        } else if let Some(v) = &d.version {
            // Jade names one exact version, as it does for every dependency.
            spec.push(format!("version = {}", toml_str(&format!("={v}"))));
        }
        if let Some(p) = &d.package {
            spec.push(format!("package = {}", toml_str(p)));
        }
        out.push_str(&format!("{name} = {{ {} }}\n", spec.join(", ")));
    }
    Ok(out)
}

/// Purpose: This function writes the generated crate's `lib.rs`.
pub fn lib_rs(deps: &BTreeMap<String, DependencyEntry>) -> String {
    let mut out = String::from(
        "// Written by jade. One line per Rust package, so rustc links each one into the\n\
         // staticlib the program is linked with.\n",
    );
    for name in deps.keys() {
        out.push_str(&format!("extern crate {name};\n"));
    }
    out
}

/// Purpose: This function quotes a string for TOML.
fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// Purpose: This function writes a file only when its contents change, through a
/// rename, so cargo sees an unchanged crate as fresh and two builds at once never
/// read half a file.
pub fn write_if_changed(path: &Path, text: &str) -> Result<(), String> {
    if std::fs::read_to_string(path).is_ok_and(|t| t == text) {
        return Ok(());
    }
    // Named for the process and a count, since two threads of one process, such
    // as two tests, may write the same crate at once.
    static WRITES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {} ({e})", dir.display()))?;
    let tmp = dir.join(format!(
        ".{}.{}.{n}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {} ({e})", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot write {} ({e})", path.display())
    })
}

/// Purpose: This function finds cargo.
///
/// Outputs: `CARGO` when it is set, which cargo sets for what it runs, else `cargo`
/// from `PATH`, or a message saying how to get one.
pub fn cargo_program() -> Result<PathBuf, String> {
    if let Some(c) = std::env::var_os("CARGO").filter(|c| !c.is_empty()) {
        return Ok(PathBuf::from(c));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).map(|d| d.join("cargo")).find(|p| p.is_file()).ok_or_else(|| {
        "this program uses a Rust package, and building one needs cargo, Rust's build tool, \
         which is not on PATH. Install a Rust toolchain from https://rustup.rs, or put cargo on \
         PATH, and build again; jade does not fetch one for you yet"
            .to_string()
    })
}

/// Purpose: This struct holds what a successful build produced.
#[derive(Debug, Clone)]
pub struct Built {
    /// The staticlib.
    pub staticlib: PathBuf,
    /// The system libraries it needs, as rustc's `native-static-libs` note lists
    /// them: `-lSystem`, `-framework Security` and the like.
    pub native_libs: Vec<String>,
}

/// Purpose: This function builds the generated crate.
///
/// Inputs:
/// - `p`: where the crate and target directory are.
///
/// Outputs: the staticlib and its system libraries, or cargo's account of what
/// failed, as rustc rendered it.
pub fn build(p: &Places) -> Result<Built, String> {
    let built = build_once(p)?;
    if !built.native_libs.is_empty() {
        return Ok(built);
    }
    // rustc names the system libraries the staticlib needs only when it links the
    // staticlib, and cargo replays that note for a fresh build only when it kept
    // it. Without the list, a program links on macOS, where libSystem holds them
    // all, but not on Linux, where Rust's unwinder is libgcc_s. Touching the
    // generated crate's one source file makes rustc link it again, which takes a
    // moment, since every dependency stays built.
    let lib_rs = p.crate_dir.join("src").join("lib.rs");
    std::fs::File::options()
        .write(true)
        .open(&lib_rs)
        .and_then(|f| f.set_modified(std::time::SystemTime::now()))
        .map_err(|e| format!("cannot touch {} ({e})", lib_rs.display()))?;
    build_once(p)
}

/// Purpose: This function runs cargo on the generated crate once.
///
/// Inputs:
/// - `p`: where the crate and target directory are.
///
/// Outputs: the staticlib and the system libraries rustc's note named, if cargo
/// printed or replayed it.
fn build_once(p: &Places) -> Result<Built, String> {
    let cargo = cargo_program()?;
    let manifest = p.crate_dir.join("Cargo.toml");
    // A first build fetches and compiles every crate, which takes a while; a
    // later one asks cargo and is done in a moment, so only the first says so.
    let built_before =
        p.target_dir.join("release").join(format!("lib{}.a", p.crate_name)).is_file();
    if !built_before {
        eprintln!(
            "note: building this project's Rust packages with cargo; the first build fetches \
             and compiles their crates"
        );
    }
    let out = Command::new(&cargo)
        .current_dir(&p.crate_dir)
        .args(["rustc", "--release", "--lib", "--message-format=json-render-diagnostics"])
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--target-dir")
        .arg(&p.target_dir)
        .args(["--", "--print=native-static-libs"])
        .output()
        .map_err(|e| format!("cannot run {} ({e})", cargo.display()))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(format!(
            "the project's Rust packages did not build:\n{}",
            diagnostics(&stderr)
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let staticlib = staticlib_from(&stdout, &manifest).ok_or_else(|| {
        format!(
            "cargo built the project's Rust packages but did not name a staticlib for {}",
            manifest.display()
        )
    })?;
    Ok(Built { staticlib, native_libs: native_libs_from(&stderr) })
}

/// Purpose: This function keeps what a person needs from cargo's stderr: the
/// errors and warnings rustc rendered, without cargo's progress lines.
pub fn diagnostics(stderr: &str) -> String {
    const PROGRESS: [&str; 10] = [
        "Compiling",
        "Updating",
        "Locking",
        "Blocking",
        "Downloading",
        "Downloaded",
        "Fresh",
        "Finished",
        "Adding",
        "Checking",
    ];
    stderr
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !PROGRESS.iter().any(|p| t.starts_with(p)) && !t.starts_with("note: native-static-libs")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Purpose: This function finds the staticlib in cargo's JSON messages.
///
/// Inputs:
/// - `stdout`: cargo's messages, one JSON object a line.
/// - `manifest`: the generated crate's manifest, whose artifact is wanted.
///
/// Outputs: the `.a` file, when cargo named one.
pub fn staticlib_from(stdout: &str, manifest: &Path) -> Option<PathBuf> {
    let want = manifest.canonicalize().unwrap_or_else(|_| manifest.to_path_buf());
    stdout.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()).find_map(|m| {
        if m.get("reason")?.as_str()? != "compiler-artifact" {
            return None;
        }
        let path = PathBuf::from(m.get("manifest_path")?.as_str()?);
        if path.canonicalize().unwrap_or(path) != want {
            return None;
        }
        m.get("filenames")?
            .as_array()?
            .iter()
            .filter_map(|f| f.as_str())
            .find(|f| f.ends_with(".a"))
            .map(PathBuf::from)
    })
}

/// Purpose: This function reads rustc's `native-static-libs` note.
///
/// Inputs:
/// - `stderr`: cargo's stderr, where the note is rendered, and replayed when the
///   crate was already built.
///
/// Outputs: the linker arguments it lists.
pub fn native_libs_from(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("note: native-static-libs:"))
        .map(|rest| rest.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Purpose: This function asks cargo which crate each dependency of the generated
/// crate is, since a package may name its library differently from itself.
///
/// Inputs:
/// - `p`: where the crate is. It has been built, so cargo answers from its lock.
///
/// Outputs: each dependency's name in `jade.toml`, and the name rustc gives its
/// library, which its records carry.
pub fn crate_names(p: &Places) -> Result<BTreeMap<String, String>, String> {
    let cargo = cargo_program()?;
    let out = Command::new(&cargo)
        .current_dir(&p.crate_dir)
        .args(["metadata", "--format-version", "1", "--offline"])
        .arg("--manifest-path")
        .arg(p.crate_dir.join("Cargo.toml"))
        .output()
        .map_err(|e| format!("cannot run {} ({e})", cargo.display()))?;
    if !out.status.success() {
        return Err(format!(
            "cargo could not describe the project's Rust packages:\n{}",
            diagnostics(&String::from_utf8_lossy(&out.stderr))
        ));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("cargo's description of the Rust packages cannot be read ({e})"))?;
    Ok(crate_names_from(&v))
}

/// Purpose: This function reads `cargo metadata`'s answer for [`crate_names`].
pub fn crate_names_from(v: &serde_json::Value) -> BTreeMap<String, String> {
    let mut libs: BTreeMap<&str, String> = BTreeMap::new();
    for p in v.get("packages").and_then(|p| p.as_array()).into_iter().flatten() {
        let Some(id) = p.get("id").and_then(|i| i.as_str()) else { continue };
        let lib = p.get("targets").and_then(|t| t.as_array()).into_iter().flatten().find(|t| {
            t.get("kind")
                .and_then(|k| k.as_array())
                .is_some_and(|k| k.iter().any(|k| matches!(k.as_str(), Some("lib" | "rlib"))))
        });
        if let Some(name) = lib.and_then(|t| t.get("name")).and_then(|n| n.as_str()) {
            libs.insert(id, name.replace('-', "_"));
        }
    }
    let root = v.get("resolve").and_then(|r| r.get("root")).and_then(|r| r.as_str());
    let mut out = BTreeMap::new();
    let nodes = v.get("resolve").and_then(|r| r.get("nodes")).and_then(|n| n.as_array());
    for node in nodes.into_iter().flatten() {
        if node.get("id").and_then(|i| i.as_str()) != root {
            continue;
        }
        for dep in node.get("deps").and_then(|d| d.as_array()).into_iter().flatten() {
            let (Some(name), Some(pkg)) =
                (dep.get("name").and_then(|n| n.as_str()), dep.get("pkg").and_then(|p| p.as_str()))
            else {
                continue;
            };
            if let Some(lib) = libs.get(pkg) {
                out.insert(name.to_string(), lib.clone());
            }
        }
    }
    out
}
