//! Author: Joe Ricks
//! File: src/host/tests.rs
//! Description: Runs the dovata process on a thread of the test, adds tiny models to it,
//! and checks every request, and every record a program reads, against the model file
//! and the plan.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::gguf::write::Writer;
use crate::gguf::Value;
use crate::layout::BindOptions;
use crate::load::KvType;
use crate::memplan::{Role, Source};
use crate::target::RegionKind;

use super::server::serve_in;
use super::state::{self, Op};
use super::{memory, signal, AddOptions, HostError, Open, Process, State};

#[path = "fixture.rs"]
mod fixture;

/// Purpose: This struct holds a dovata process running on a thread of the test.
struct Running {
    /// The process, as a program talks to it.
    process: Process,
    /// The thread; it returns what `serve_in` returned.
    thread: JoinHandle<Result<(), HostError>>,
    /// The fixture model's file.
    model: PathBuf,
    /// The run directory, which also holds the model; removed at the end.
    dir: fixture::TempDir,
}

/// Purpose: This function gives the add options the tests use: a small step, so the
/// fixture's regions are small.
///
/// Inputs:
/// - `name`: the model's name.
///
/// Outputs: the options.
fn opts(
    name: &str,
) -> AddOptions
{
    let step = BindOptions { n_ctx: 64, n_tokens: 8, n_outputs: 1, kv_k: KvType::F16, kv_v: KvType::F16 };
    AddOptions { name: Some(name.into()), step, ..AddOptions::default() }
}

/// Purpose: This function starts the dovata process on a thread, with a fixture model
/// written beside it but not yet added, and waits until it answers.
///
/// Inputs: none.
///
/// Outputs: the [`Running`] process.
fn run() -> Running
{
    let dir = fixture::TempDir::new();
    let model = dir.path.join("host-fixture.gguf");
    fixture::write_llama(&model);
    let (tx, rx) = mpsc::channel();
    let run_dir = dir.path.clone();
    let thread = std::thread::spawn(move || serve_in(&run_dir, move |r| tx.send(r.clone()).is_ok()));
    let ready = match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(r) => r,
        Err(_) => panic!("not ready: {:?}", thread.join()),
    };
    assert_eq!(ready.pid, std::process::id());
    assert_eq!(ready.dir, dir.path);
    Running { process: state::connect_in(&dir.path).unwrap(), thread, model, dir }
}

/// Purpose: This function stops a process the test started and checks it stopped
/// cleanly.
///
/// Inputs:
/// - `r`: the process.
///
/// Outputs: none.
fn stop(
    r: Running,
)
{
    r.process.kill().unwrap();
    assert_eq!(r.thread.join().unwrap(), Ok(()));
    let dir = r.process.dir();
    assert!(!state::process_path(dir).exists() && !state::models_dir(dir).exists(), "the process's files are removed");
    assert_eq!(r.process.status(), Err(HostError::NotRunning));
    assert_eq!(state::connect_in(dir), Err(HostError::NotRunning));
}

/// Purpose: This function lists the shared memory names a model's record gives.
///
/// Inputs:
/// - `p`: the process.
/// - `name`: the model's name.
///
/// Outputs: the names, in region order.
fn shared_names(
    p: &Process,
    name: &str,
) -> Vec<String>
{
    let m = p.model(name).unwrap();
    m.regions
        .iter()
        .filter_map(|r| match &r.open {
            Open::Shared { name } => Some(name.clone()),
            Open::File { .. } => None,
        })
        .collect()
}

/// Purpose: This function reads part of a file.
///
/// Inputs:
/// - `path`: the file.
/// - `offset`: where to start.
/// - `len`: how many bytes.
///
/// Outputs: the bytes.
fn file_bytes(
    path: &Path,
    offset: usize,
    len: usize,
) -> Vec<u8>
{
    std::fs::read(path).unwrap()[offset..offset + len].to_vec()
}

#[test]
fn the_process_starts_empty_and_a_program_maps_a_model_added_to_it()
{
    let r = run();
    let p = &r.process;
    let s = p.status().unwrap();
    assert_eq!(s.pid, std::process::id());
    assert!(s.models.is_empty());

    let m = p.add_parts(std::slice::from_ref(&r.model), &opts("fixture")).unwrap();
    assert_eq!((m.name.as_str(), m.state), ("fixture", State::Ready));
    assert_eq!(m.model.architecture, "llama");
    let size = std::fs::metadata(&r.model).unwrap().len();
    assert_eq!(m.files.len(), 1);
    assert_eq!(m.files[0].bytes, size);
    assert!(m.files[0].resident_bytes > 0, "the model was read into memory: {:?}", m.files[0]);
    assert!(m.regions.iter().any(|x| x.kind == RegionKind::Mmap) && m.allocated().count() >= 1, "{:?}", m.regions);
    for a in m.allocated() {
        assert_eq!(a.resident_bytes, a.bytes, "an allocated region is touched while loading: {a:?}");
    }
    assert!(m.clients.is_empty());

    let plan = p.plan("fixture").unwrap();
    assert_eq!(plan.table_hash, m.table_hash);
    // The process plans a model exactly as the compiler does for this machine, so a
    // program compiled against `dovata plan` maps it.
    let gguf = crate::load::Model::open(&r.model).unwrap();
    let layout = crate::layout::for_model(&gguf, None).unwrap();
    let target = crate::machine::current().unwrap().target(false);
    let definition = std::fs::read_to_string(r.model.with_file_name("tokenizer.json")).unwrap();
    let tokenizer = crate::tokenize::describe(&gguf, definition).unwrap();
    let direct = crate::memplan::plan(&gguf, &layout, &opts("x").step, &target, Some(&tokenizer)).unwrap();
    assert_eq!(direct.table_hash, m.table_hash);
    assert_eq!(plan.tokenizer.as_ref().map(|t| t.sha256.as_str()), Some(tokenizer.sha256.as_str()), "the plan carries the tokenizer");
    assert_eq!(plan, direct, "a program reads back the plan the process made, every float exactly");

    // The record says where every region is, so a program can open it by name.
    let held = p.model("fixture").unwrap();
    assert_eq!((held.table_hash.as_str(), held.state, held.regions.len()), (m.table_hash.as_str(), State::Ready, plan.regions.len()));
    assert_eq!(held.files[0].bytes, size);
    let names = shared_names(p, "fixture");
    assert_eq!(names.len(), m.allocated().count());
    for n in &names {
        assert!(memory::open_shared(n, true).is_ok(), "{n} opens by name");
    }
    assert_eq!(p.models().unwrap(), [held]);
    assert_eq!(m.tokenizer.as_ref().map(|t| t.tokens), Some(32));

    // A program gets the tokenizer its plan carries, and only for its own plan.
    let tok = p.tokenizer("fixture", &m.table_hash).unwrap();
    assert_eq!(tok.encode("t3 t1 t4", false).unwrap(), [3, 1, 4]);
    assert_eq!(tok.decode(&[3, 1, 4], false).unwrap(), "t3 t1 t4");
    assert!(matches!(p.tokenizer("fixture", &"0".repeat(64)), Err(HostError::Refused(e)) if e.contains("table hash")));
    assert!(matches!(p.plan("nothing"), Err(HostError::Refused(e)) if e.contains("no model named nothing") && e.contains("fixture")));
    assert!(matches!(p.model("../x"), Err(HostError::Refused(e)) if e.contains("no model named ../x")));

    // A program whose offsets came from another plan is refused.
    let wrong = p.map("fixture", &"0".repeat(64));
    assert!(matches!(&wrong, Err(HostError::Refused(e)) if e.contains("table hash")), "{wrong:?}");

    // Every weight is at its planned address, with the file's bytes.
    let mem = p.map("fixture", &m.table_hash).unwrap();
    assert_eq!(mem.table_hash(), m.table_hash);
    assert_eq!(mem.regions().len(), m.regions.len());
    let mut weights = 0;
    for e in &plan.entries {
        let (Role::Weight, Source::File { file_offset, .. }) = (&e.role, e.source) else { continue };
        let region = mem.region(e.region).unwrap();
        let got = region.read(e.offset as usize, e.bytes as usize).unwrap();
        assert_eq!(got, file_bytes(&r.model, file_offset as usize, e.bytes as usize), "{}", e.tensor);
        weights += 1;
    }
    assert!(weights > 10, "{weights} weights");

    // Weights are read-only; allocated regions are shared between every mapping.
    let w = mem.regions().iter().find(|x| x.kind() == RegionKind::Mmap).unwrap();
    assert!(!w.writable() && w.as_mut_ptr().is_none() && !w.write(0, &[1]));
    let id = mem.regions().iter().find(|x| x.writable()).unwrap().id();
    let other = p.map("fixture", &m.table_hash).unwrap();
    let pattern: Vec<u8> = (0..=255u8).collect();
    assert!(other.region(id).unwrap().write(100, &pattern));
    assert_eq!(mem.region(id).unwrap().read(100, 256).unwrap(), pattern, "a write through one mapping shows in the other");
    let a = mem.region(id).unwrap();
    assert!(a.read(a.len() - 1, 2).is_none() && !a.write(a.len(), &[0]), "reads and writes stay inside the region");

    let s = p.status().unwrap();
    let clients = &s.model("fixture").unwrap().clients;
    assert_eq!(clients.len(), 1, "a program that maps a model twice is listed once: {clients:?}");
    assert!(clients.iter().all(|c| c.pid == std::process::id() && c.alive), "{clients:?}");

    stop(r);
    // A mapping outlives the process, and the names of its memory go with it.
    assert_eq!(mem.region(id).unwrap().read(100, 4).unwrap(), &pattern[..4]);
    for n in &names {
        assert!(memory::open_shared(n, false).is_err(), "{n} is gone");
    }
}

#[test]
fn one_process_holds_several_models_and_each_name_once()
{
    let r = run();
    let p = &r.process;
    let a = p.add_parts(std::slice::from_ref(&r.model), &opts("a")).unwrap();
    let b = p.add_parts(std::slice::from_ref(&r.model), &opts("b")).unwrap();
    let s = p.status().unwrap();
    let names: Vec<&str> = s.models.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["a", "b"]);
    assert_eq!(s.bytes(), a.bytes() + b.bytes());

    let again = p.add_parts(std::slice::from_ref(&r.model), &opts("a"));
    assert!(matches!(&again, Err(HostError::Refused(e)) if e.contains("already holds a model named a")), "{again:?}");

    // Each model has memory of its own: a write to one's allocation is not in the other's.
    let ma = p.map("a", &a.table_hash).unwrap();
    let mb = p.map("b", &b.table_hash).unwrap();
    let id = ma.regions().iter().find(|x| x.writable()).unwrap().id();
    assert!(ma.region(id).unwrap().write(0, &[7, 7, 7]));
    assert_eq!(mb.region(id).unwrap().read(0, 3).unwrap(), [0, 0, 0]);

    // Removing a model takes it out of the process; a program's mapping stays, and no
    // program can open its memory by name again.
    let names = shared_names(p, "a");
    p.remove("a").unwrap();
    assert!(names.iter().all(|n| memory::open_shared(n, false).is_err()), "{names:?}");
    let s = p.status().unwrap();
    assert_eq!(s.models.len(), 1);
    assert!(s.model("a").is_none());
    assert!(matches!(p.map("a", &a.table_hash), Err(HostError::Refused(_))));
    assert!(matches!(p.remove("a"), Err(HostError::Refused(e)) if e.contains("no model named a")));
    assert_eq!(ma.region(id).unwrap().read(0, 3).unwrap(), [7, 7, 7]);
    // Its name is free again.
    p.add_parts(std::slice::from_ref(&r.model), &opts("a")).unwrap();
    stop(r);
}

#[test]
fn a_second_process_is_refused_and_a_killed_ones_memory_is_cleared()
{
    let r = run();
    let second = serve_in(&r.dir.path, |_| true);
    assert_eq!(second, Err(HostError::AlreadyRunning(std::process::id())));
    // A program left the process with memory still named, as one that was killed
    // would. The next process removes the names.
    let m = r.process.add_parts(std::slice::from_ref(&r.model), &opts("left")).unwrap();
    let names = shared_names(&r.process, "left");
    let record = std::fs::read(state::record_path(&r.dir.path, "left")).unwrap();
    stop(r);

    let dir = fixture::TempDir::new();
    std::fs::create_dir_all(state::model_dir(&dir.path, "left")).unwrap();
    std::fs::write(state::record_path(&dir.path, "left"), record).unwrap();
    for n in &names {
        drop(memory::create_shared(n, 4096).unwrap());
    }
    // A record this dovata cannot read whole still gives its names.
    let junk = format!("/dovata.{:016x}.9", std::process::id());
    drop(memory::create_shared(&junk, 4096).unwrap());
    std::fs::create_dir_all(state::model_dir(&dir.path, "junk")).unwrap();
    let odd = format!(r#"{{"format":99,"regions":[{{"open":{{"kind":"shared","name":"{junk}"}}}}]}}"#);
    std::fs::write(state::record_path(&dir.path, "junk"), odd).unwrap();
    std::fs::create_dir_all(state::requests_dir(&dir.path)).unwrap();
    let old = state::requests_dir(&dir.path).join("1-2-3.done");
    std::fs::write(&old, b"").unwrap();
    let (tx, rx) = mpsc::channel();
    let run_dir = dir.path.clone();
    let thread = std::thread::spawn(move || serve_in(&run_dir, move |r| tx.send(r.clone()).is_ok()));
    rx.recv_timeout(Duration::from_secs(30)).unwrap();
    for n in names.iter().chain([&junk]) {
        assert!(memory::open_shared(n, false).is_err(), "{n} is removed");
    }
    assert!(!old.exists(), "answers nobody read are removed");
    let process = state::connect_in(&dir.path).unwrap();
    assert!(process.models().unwrap().is_empty());
    assert_eq!(m.state, State::Ready);
    stop(Running { process, thread, model: PathBuf::new(), dir });
}

#[test]
fn a_request_dovata_cannot_read_is_refused()
{
    let r = run();
    let id = state::leave_request(&r.dir.path, Op::Add, b"not json").unwrap();
    signal::notify(std::process::id()).unwrap();
    let start = std::time::Instant::now();
    let answer = loop {
        if let Some(a) = state::read_answer(&r.dir.path, &id) {
            break a;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "no answer");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(matches!(&answer, Err(m) if m.contains("not a model dovata can add")), "{answer:?}");
    assert!(matches!(r.process.remove("nothing"), Err(HostError::Refused(m)) if m.contains("no model named nothing")));
    stop(r);
}

#[test]
fn a_stop_request_stops_its_own_process_only()
{
    // Two processes on threads of one program both wake on every signal; each takes
    // only the requests in its own run directory.
    let a = run();
    let b = run();
    stop(a);
    assert!(b.process.status().is_ok());
    b.process.add_parts(std::slice::from_ref(&b.model), &opts("still")).unwrap();

    // A request taken with a stop is answered, not dropped.
    let dir = b.dir.path.clone();
    let stop_id = state::leave_request(&dir, Op::Stop, b"").unwrap();
    let late = state::leave_request(&dir, Op::Remove, b"still").unwrap();
    signal::notify(std::process::id()).unwrap();
    assert_eq!(b.thread.join().unwrap(), Ok(()));
    assert_eq!(state::read_answer(&dir, &stop_id), Some(Ok(())));
    assert_eq!(state::read_answer(&dir, &late), Some(Err("dovata is stopping".into())));
}

#[test]
fn shared_memory_open_to_others_is_not_dovatas()
{
    use std::os::fd::{AsFd, FromRawFd, OwnedFd};
    let mine = format!("/dovata.{:016x}.7", u64::from(std::process::id()) << 8);
    let fd = memory::create_shared(&mine, 4096).unwrap();
    assert!(memory::private_to_me(fd.as_fd()).unwrap(), "dovata's own memory is this user's alone");
    memory::unlink_shared(&mine);

    // An object others may read, as another user's squatting a name would be. macOS will
    // not change a shared memory object's mode, so it is made that way.
    let open = format!("/dovata.{:016x}.8", u64::from(std::process::id()) << 8);
    memory::unlink_shared(&open);
    let c = std::ffi::CString::new(open.clone()).unwrap();
    let flags = libc::O_RDWR | libc::O_CREAT | libc::O_EXCL;
    // SAFETY: `c` lives through the call; the mode is passed as each system declares it.
    #[cfg(target_vendor = "apple")]
    let raw = unsafe { libc::shm_open(c.as_ptr(), flags, 0o644 as libc::c_uint) };
    #[cfg(not(target_vendor = "apple"))]
    let raw = unsafe { libc::shm_open(c.as_ptr(), flags, 0o644 as libc::mode_t) };
    assert!(raw >= 0, "{}", std::io::Error::last_os_error());
    // SAFETY: `raw` was just opened and nothing else owns it.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    assert_eq!(unsafe { libc::fstat(raw, &mut st) }, 0);
    // A umask that takes every bit from others leaves nothing to refuse.
    if st.st_mode & 0o077 != 0 {
        assert!(!memory::private_to_me(fd.as_fd()).unwrap(), "memory others may read is refused");
    }
    memory::unlink_shared(&open);
}

#[test]
fn a_start_nobody_waits_for_stops_the_process()
{
    let dir = fixture::TempDir::new();
    // The ready callback says nobody is listening, as when `dovata start` was stopped
    // with Ctrl-C before the process answered.
    assert_eq!(serve_in(&dir.path, |_| false), Ok(()));
    assert!(!state::process_path(&dir.path).exists());
    assert_eq!(state::connect_in(&dir.path), Err(HostError::NotRunning));
}

#[test]
fn a_model_that_cannot_be_planned_is_refused_and_the_process_goes_on()
{
    let r = run();
    let layout = r.dir.path.join("bad.toml");
    std::fs::write(&layout, b"not a layout [").unwrap();
    let bad = AddOptions { layout: Some(layout), ..opts("bad") };
    let e = r.process.add_parts(std::slice::from_ref(&r.model), &bad);
    assert!(matches!(&e, Err(HostError::Refused(m)) if m.contains("bad.toml")), "{e:?}");
    assert!(r.process.status().unwrap().models.is_empty());
    r.process.add_parts(std::slice::from_ref(&r.model), &opts("good")).unwrap();
    stop(r);
}

#[test]
fn a_model_changed_on_disk_is_not_handed_out()
{
    let r = run();
    let m = r.process.add_parts(std::slice::from_ref(&r.model), &opts("changed")).unwrap();
    drop(r.process.map("changed", &m.table_hash).unwrap());
    // Appending changes the size and the time; the bytes already mapped stay valid.
    std::fs::OpenOptions::new().append(true).open(&r.model).unwrap().write_all(b"more").unwrap();
    let refused = r.process.map("changed", &m.table_hash);
    assert!(matches!(&refused, Err(HostError::Refused(e)) if e.contains("changed on disk")), "{refused:?}");
    stop(r);
}
