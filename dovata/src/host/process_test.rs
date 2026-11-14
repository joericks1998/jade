//! Author: Joe Ricks
//! File: src/host/process_test.rs
//! Description: Starts the real `dovata` program the way a Jade program does, adds a
//! tiny model to it, maps its memory into this test, checks the weights, and stops it,
//! through both the library and the command line.

use std::path::Path;
use std::process::Command;

use dovata::gguf::write::Writer;
use dovata::gguf::Value;
use dovata::host::{self, AddOptions, HostError, Open, StartOptions, State};
use dovata::layout::BindOptions;
use dovata::load::KvType;
use dovata::memplan::Source;

#[path = "fixture.rs"]
mod fixture;

/// The `dovata` program cargo built for this test.
const DOVATA: &str = env!("CARGO_BIN_EXE_dovata");

/// Purpose: This function runs the `dovata` program with the test's run directory.
///
/// Inputs:
/// - `run_dir`: the run directory.
/// - `args`: the arguments.
///
/// Outputs: the exit status's success, standard output and standard error.
fn dovata(
    run_dir: &Path,
    args: &[&str],
) -> (bool, String, String)
{
    let o = Command::new(DOVATA).args(args).env("DOVATA_RUN_DIR", run_dir).output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

#[test]
fn a_program_starts_dovata_adds_a_model_maps_it_and_kills_dovata()
{
    let dir = fixture::TempDir::new();
    // The library reads the run directory from the environment, as a Jade program's
    // would. It is set before any thread starts.
    std::env::set_var("DOVATA_RUN_DIR", &dir.path);
    let model = dir.path.join("host-fixture.gguf");
    fixture::write_llama(&model);
    let step = BindOptions { n_ctx: 64, n_tokens: 8, n_outputs: 1, kv_k: KvType::F16, kv_v: KvType::F16 };
    let opts = AddOptions { step, ..AddOptions::default() };

    // Nothing runs yet.
    assert_eq!(host::connect(), Err(HostError::NotRunning));
    let (ok, _, err) = dovata(&dir.path, &["add", model.to_str().unwrap()]);
    assert!(!ok && err.contains("dovata is not running; start it with `dovata start`"), "{err}");

    // Start from code, the way a Jade program does, then add a model: a local file here,
    // a Hugging Face link the same way.
    let p = host::start(&StartOptions { program: Some(DOVATA.into()) }).unwrap();
    let s = p.status().unwrap();
    assert_ne!(s.pid, std::process::id(), "dovata is a process of its own");
    assert!(s.models.is_empty());
    assert!(matches!(host::start(&StartOptions { program: Some(DOVATA.into()) }), Err(HostError::AlreadyRunning(pid)) if pid == s.pid));
    let m = p.add(model.to_str().unwrap(), &opts).unwrap();
    assert_eq!((m.name.as_str(), m.state), ("host-fixture", State::Ready));

    // Map it, and read one weight where the plan says it is.
    let mem = p.map("host-fixture", &m.table_hash).unwrap();
    let plan = p.plan("host-fixture").unwrap();
    let e = plan.entries.iter().find(|e| e.tensor == "blk.1.ffn_down.weight").unwrap();
    let (region, offset, bytes) = (e.region, e.offset as usize, e.bytes as usize);
    let got = mem.region(region).unwrap().read(offset, bytes).unwrap();
    let file = std::fs::read(&model).unwrap();
    let Source::File { file_offset, .. } = e.source else { panic!("{:?} is not from the file", e.source) };
    let off = file_offset as usize;
    assert_eq!(got, &file[off..off + bytes]);

    // The tokenizer comes with the plan, from the process the program reads it from.
    assert_eq!(plan.schema_version, 2);
    let tok = p.tokenizer("host-fixture", &m.table_hash).unwrap();
    assert_eq!(tok.encode("t7 t0 t31", false).unwrap(), [7, 0, 31]);

    // The command line sees the same process: a second model, the status, and
    // `dovata machine usage`.
    let (ok, text, err) = dovata(&dir.path, &["start"]);
    assert!(ok && text.starts_with(&format!("dovata is already running, pid {}, with 1 model.", s.pid)), "{text}{err}");
    let (ok, text, err) = dovata(&dir.path, &["add", model.to_str().unwrap(), "--name", "second", "--ctx", "64", "--tokens", "8"]);
    assert!(ok && text.starts_with("second, added ") && text.contains("state      ready"), "{text}{err}");
    assert!(text.contains("tokenizer  gpt2, 32 tokens"), "{text}");
    let (ok, text, err) = dovata(&dir.path, &["status"]);
    assert!(ok, "{err}");
    assert!(text.starts_with(&format!("dovata {}, pid {}", env!("CARGO_PKG_VERSION"), s.pid)) && text.contains(", 2 models, "), "{text}");
    assert!(text.contains(&format!("mapped by  pid {}, running", std::process::id())), "{text}");
    let (ok, text, err) = dovata(&dir.path, &["status", "host-fixture", "--json"]);
    assert!(ok, "{err}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["table_hash"].as_str(), Some(m.table_hash.as_str()));
    let (ok, text, err) = dovata(&dir.path, &["machine", "usage"]);
    assert!(ok, "{err}");
    assert!(text.contains(&format!("  dovata     pid {}, 2 models", s.pid)) && text.contains("  model      second, "), "{text}");

    // Take one model out from the command line; the other stays.
    let (ok, text, err) = dovata(&dir.path, &["remove", "second"]);
    assert!(ok && text == "second removed\n", "{text}{err}");
    let (ok, _, err) = dovata(&dir.path, &["remove", "second"]);
    assert!(!ok && err.contains("no model named second"), "{err}");
    assert_eq!(p.status().unwrap().models.len(), 1);

    // A model the process cannot plan is refused with the process's own reason.
    let bad = dir.path.join("bad.toml");
    std::fs::write(&bad, b"not a layout [").unwrap();
    let (ok, _, err) = dovata(&dir.path, &["add", model.to_str().unwrap(), "--name", "bad", "--layout", bad.to_str().unwrap()]);
    assert!(!ok && err.contains("bad.toml"), "{err}");

    // Stop it from code. Its records go, and the mapping stays readable.
    p.kill().unwrap();
    assert_eq!(host::connect(), Err(HostError::NotRunning));
    assert_eq!(mem.region(region).unwrap().read(offset, 4).unwrap(), &file[off..off + 4]);
    let (ok, text, _) = dovata(&dir.path, &["status"]);
    assert!(ok && text.starts_with("dovata is not running"), "{text}");

    // Start and stop from the command line.
    let (ok, text, err) = dovata(&dir.path, &["start"]);
    assert!(ok && text.starts_with("dovata is running, pid ") && text.contains("dovata add <model>"), "{text}{err}");
    let (ok, text, err) = dovata(&dir.path, &["add", model.to_str().unwrap(), "--ctx", "64", "--tokens", "8"]);
    assert!(ok && text.starts_with("host-fixture, added "), "{text}{err}");
    let (ok, text, err) = dovata(&dir.path, &["kill"]);
    assert!(ok && text == "dovata stopped\n", "{text}{err}");
    assert_eq!(host::connect(), Err(HostError::NotRunning));
    let (ok, _, err) = dovata(&dir.path, &["kill"]);
    assert!(!ok && err.contains("not running"), "{err}");

    // SIGTERM stops it too, and takes the names of its memory with it.
    let (ok, text, err) = dovata(&dir.path, &["start"]);
    assert!(ok, "{text}{err}");
    let (ok, text, err) = dovata(&dir.path, &["add", model.to_str().unwrap(), "--ctx", "64", "--tokens", "8"]);
    assert!(ok, "{text}{err}");
    let p = host::connect().unwrap();
    let names: Vec<String> = p
        .model("host-fixture")
        .unwrap()
        .regions
        .into_iter()
        .filter_map(|r| match r.open {
            Open::Shared { name } => Some(name),
            Open::File { .. } => None,
        })
        .collect();
    assert!(!names.is_empty() && names.iter().all(|n| shared_exists(n)), "{names:?}");
    assert!(Command::new("kill").args(["-TERM", &p.pid().to_string()]).status().unwrap().success());
    let start = std::time::Instant::now();
    while host::connect() != Err(HostError::NotRunning) {
        assert!(start.elapsed().as_secs() < 10, "dovata still runs after SIGTERM");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(names.iter().all(|n| !shared_exists(n)), "{names:?}");
    assert!(!dir.path.join("models").exists() && !dir.path.join("process").exists());
}

/// Purpose: This function says whether a named shared memory object exists.
///
/// Inputs:
/// - `name`: its name.
///
/// Outputs: `true` when it opens.
fn shared_exists(
    name: &str,
) -> bool
{
    let c = std::ffi::CString::new(name).unwrap();
    // SAFETY: `c` is a NUL-terminated string that lives through the call. Opening for
    // reading makes nothing, so the mode is not used; macOS declares shm_open variadic
    // and Linux takes it as a third argument.
    #[cfg(target_vendor = "apple")]
    let fd = unsafe { libc::shm_open(c.as_ptr(), libc::O_RDONLY) };
    #[cfg(not(target_vendor = "apple"))]
    let fd = unsafe { libc::shm_open(c.as_ptr(), libc::O_RDONLY, 0) };
    if fd >= 0 {
        // SAFETY: `fd` was just opened.
        unsafe { libc::close(fd) };
    }
    fd >= 0
}
