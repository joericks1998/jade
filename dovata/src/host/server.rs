//! Author: Joe Ricks
//! File: src/host/server.rs
//! Description: The dovata process: holds every model added to it, each planned for this
//! machine and loaded into memory, keeps a record of each in its run directory for
//! programs to read, and takes the requests programs leave there when a signal says one
//! is waiting.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::layout::{self, Layout};
use crate::load::Model as GgufModel;
use crate::memplan::{Backing, Plan};
use crate::quantize::Ftype;
use crate::target::{RegionKind, Target};

use super::memory::{self, Map};
use super::signal::{Wake, Wakeup};
use super::state::{self, Op, ProcessRecord, FORMAT};
use super::{check_name, fits, holds, run_dir, HeldFile, HeldModel, HeldRegion, HostError, ModelInfo, Open, Ready, Spec, State, TokenizerUse};

/// How long the process waits for a signal before it looks for requests anyway, in case
/// one was left without a signal.
const LOOK_EVERY: Duration = Duration::from_secs(1);

/// How often a model's record is rewritten while it loads, so status shows progress.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// How long a stopping process waits for adds still planning, which remove the names of
/// any memory they made once they see it stopping.
const ADDS_WAIT: Duration = Duration::from_secs(10);

/// Purpose: This struct holds one part file of a model, mapped whole.
struct Part {
    /// The whole file, mapped read-only.
    map: Map,
    /// The file as the process loaded it, as the model's record gives it.
    record: HeldFile,
}

/// Purpose: This struct holds one allocated region: its named shared memory object,
/// mapped.
struct Alloc {
    /// The object's name.
    name: String,
    /// The object, kept open while the process holds the model.
    _fd: OwnedFd,
    /// The object, mapped read-write.
    map: Map,
}

/// Purpose: This struct holds a plan's memory as the process holds it.
struct Held {
    /// Each part file, part 1 first.
    parts: Vec<Part>,
    /// One per region, in id order: the allocation for an allocated region, `None` for
    /// a window of a part file.
    allocs: Vec<Option<Alloc>>,
}

/// Purpose: This struct holds one model the process holds: its plan, its memory, and
/// how far loading it has got.
struct Hosted {
    /// The model's name in the process.
    name: String,
    /// The plan the memory is laid out by.
    plan: Plan,
    /// Which model it is.
    info: ModelInfo,
    /// The memory.
    held: Held,
    /// Bytes loaded so far.
    loaded: AtomicU64,
    /// Bytes to load in all, and the bytes the model counts against the machine.
    total: u64,
    /// Whether loading has finished.
    ready: AtomicBool,
    /// When it was added, in seconds since 1970.
    added: u64,
}

/// Purpose: This struct holds the process's models and the names being added.
#[derive(Default)]
struct Table {
    /// Every model, by name.
    models: BTreeMap<String, Arc<Hosted>>,
    /// Names whose models are being planned, so a second add of one is refused before it
    /// makes shared memory under the same names.
    adding: BTreeSet<String>,
    /// Set when the process stops; an add that finishes afterwards is refused.
    stopping: bool,
}

/// Purpose: This struct holds everything the process's threads share.
struct Server {
    /// The run directory.
    dir: PathBuf,
    /// The models.
    table: Mutex<Table>,
}

/// Purpose: This struct holds the process's place in the run directory while it runs:
/// a lock on `dovata.lock`, held until the process exits. Dropping it removes the names
/// of every model's shared memory and the process's records, on a stop, an error and a
/// panic alike. Requests and answers stay, so a program can read the answer to its stop;
/// the next start removes them.
struct Claim {
    /// The run directory.
    dir: PathBuf,
    /// The lock file, locked for as long as the process runs. The system releases the
    /// lock when the process exits, however it exits.
    _lock: File,
}

impl Drop for Claim {
    /// Purpose: This function removes the process's files from the run directory.
    ///
    /// Inputs: none.
    ///
    /// Outputs: none.
    fn drop(&mut self)
    {
        // A stop has removed these names already; no other process can have made them
        // again, since it cannot start until this one gives back the lock.
        for name in state::shared_names_in(&self.dir) {
            memory::unlink_shared(&name);
        }
        let _ = std::fs::remove_file(state::process_path(&self.dir));
        let _ = std::fs::remove_dir_all(state::models_dir(&self.dir));
    }
}

/// Purpose: This function runs the dovata process: it takes the run directory's lock, and
/// takes requests until it is stopped. It starts with no models; each add request plans
/// one and loads it.
///
/// Inputs:
/// - `ready`: called once the process takes requests. It returns `false` when nobody is
///   listening any more, such as a `dovata start` that was stopped with Ctrl-C, and the
///   process then stops.
///
/// Outputs: `Ok(())` after a stop request or a stop signal, or a [`HostError`], such as
/// [`HostError::AlreadyRunning`].
pub fn serve(
    ready: impl FnOnce(&Ready) -> bool,
) -> Result<(), HostError>
{
    serve_in(&run_dir()?, ready)
}

/// Purpose: This function runs the dovata process in a given run directory.
///
/// Inputs:
/// - `dir`: the run directory.
/// - `ready`: as for [`serve`].
///
/// Outputs: as for [`serve`].
pub(crate) fn serve_in(
    dir: &Path,
    ready: impl FnOnce(&Ready) -> bool,
) -> Result<(), HostError>
{
    let claim = claim(dir)?;
    // The handlers go in before the record says the process runs, since SIGUSR1's
    // default is to end the program.
    let wake = Wakeup::new().map_err(|e| HostError::Io(format!("cannot handle signals: {e}")))?;
    let pid = std::process::id();
    let version = env!("CARGO_PKG_VERSION");
    state::write_json(&state::process_path(dir), &ProcessRecord { format: FORMAT, pid, version: version.into(), started_unix: state::now() })?;
    let server = Arc::new(Server { dir: dir.to_path_buf(), table: Mutex::new(Table::default()) });
    eprintln!("dovata: pid {pid}, run directory {}", dir.display());
    if !ready(&Ready { pid, dir: dir.to_path_buf(), format: FORMAT, version: version.into() }) {
        eprintln!("dovata: nobody is waiting for the start any more, so it stops");
        return Ok(());
    }

    loop {
        if wake.wait(LOOK_EVERY) == Wake::Stop {
            eprintln!("dovata: stopping on a signal");
            break;
        }
        let mut stopping = false;
        for req in state::take_requests(dir) {
            if stopping {
                state::answer(dir, &req.id, Err("dovata is stopping".into()));
                continue;
            }
            match req.op {
                Op::Stop => {
                    state::answer(dir, &req.id, Ok(()));
                    eprintln!("dovata: stopping on request");
                    stopping = true;
                }
                Op::Remove => {
                    let name = String::from_utf8_lossy(&req.body).into_owned();
                    let r = remove(&server, &name).map_err(|e| e.to_string());
                    state::answer(dir, &req.id, r);
                }
                // Planning a large model takes seconds, so each add gets a thread of its
                // own, and the process goes on taking requests meanwhile.
                Op::Add => {
                    let server = Arc::clone(&server);
                    std::thread::spawn(move || {
                        // A panic still answers, so the program that asked is not left
                        // waiting.
                        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match serde_json::from_slice::<Spec>(&req.body) {
                            Ok(spec) => add(&server, spec).map_err(|e| e.to_string()),
                            Err(e) => Err(format!("the request is not a model dovata can add: {e}")),
                        }))
                        .unwrap_or_else(|_| Err("dovata failed while adding the model; its log says why".into()));
                        state::answer(&server.dir, &req.id, r);
                    });
                }
            }
        }
        if stopping {
            break;
        }
    }
    shut_down(&server);
    drop(claim);
    eprintln!("dovata: stopped");
    Ok(())
}

/// Purpose: This function locks the model table, even after a thread panicked while
/// holding it.
///
/// Inputs:
/// - `server`: the process's state.
///
/// Outputs: the locked table.
fn table(
    server: &Server,
) -> MutexGuard<'_, Table>
{
    server.table.lock().unwrap_or_else(|e| e.into_inner())
}

/// Purpose: This function takes every model out of the process as it stops, and removes
/// the names of their shared memory. Programs that mapped them keep their mappings.
///
/// Inputs:
/// - `server`: the process's state.
///
/// Outputs: none.
fn shut_down(
    server: &Server,
)
{
    {
        let mut t = table(server);
        t.stopping = true;
        for m in std::mem::take(&mut t.models).into_values() {
            m.held.unlink();
        }
    }
    // An add still planning may make shared memory before it sees the process stopping;
    // it removes the names once it does. Exiting first would leave them until the machine
    // restarts.
    let start = Instant::now();
    while !table(server).adding.is_empty() && start.elapsed() < ADDS_WAIT {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Purpose: This struct holds a name being added; dropping it frees the name.
struct Adding<'a> {
    /// The process's state.
    server: &'a Server,
    /// The name.
    name: String,
}

impl Drop for Adding<'_> {
    /// Purpose: This function frees the name.
    ///
    /// Inputs: none.
    ///
    /// Outputs: none.
    fn drop(&mut self)
    {
        table(self.server).adding.remove(&self.name);
    }
}

/// Purpose: This function adds a model: it plans it for this machine, sets up its
/// memory, checks it fits beside the models already held, writes its record, and starts
/// loading it on a thread of its own.
///
/// Inputs:
/// - `server`: the process's state.
/// - `spec`: the model.
///
/// Outputs: `Ok(())` once the model is loading, or a [`HostError`] saying why it cannot
/// be added.
fn add(
    server: &Arc<Server>,
    spec: Spec,
) -> Result<(), HostError>
{
    check_name(&spec.name)?;
    let _adding = {
        let mut t = table(server);
        if t.stopping {
            return Err(HostError::Refused("dovata is stopping".into()));
        }
        if t.models.contains_key(&spec.name) || t.adding.contains(&spec.name) {
            return Err(HostError::Refused(format!(
                "dovata already holds a model named {}; remove it first, or add this one with another --name",
                spec.name
            )));
        }
        t.adding.insert(spec.name.clone());
        Adding { server, name: spec.name.clone() }
    };
    // Planning and setting up memory happen outside the lock, so other requests are
    // answered meanwhile. Setting up only reserves memory; loading is what fills it, and
    // it starts once the model is known to fit.
    let (plan, info) = plan(&spec)?;
    let held = Held::set_up(&server.dir, &spec.name, &spec.parts, &plan)?;
    let total = held.parts.iter().map(|p| p.record.bytes).sum::<u64>() + held.allocs.iter().flatten().map(|a| a.map.len() as u64).sum::<u64>();
    let model = Arc::new(Hosted {
        name: spec.name.clone(),
        plan,
        info,
        held,
        loaded: AtomicU64::new(0),
        total,
        ready: AtomicBool::new(false),
        added: state::now(),
    });
    {
        let mut t = table(server);
        let refused = if t.stopping {
            Some(HostError::Refused("dovata is stopping".into()))
        } else {
            let in_use = t.models.values().map(|m| m.total).sum::<u64>();
            fits(in_use, total, &model.plan.target).err().map(HostError::Refused)
        };
        let written = match refused {
            Some(e) => Err(e),
            None => write_files(&server.dir, &model),
        };
        if let Err(e) = written {
            model.held.unlink();
            let _ = std::fs::remove_dir_all(state::model_dir(&server.dir, &model.name));
            return Err(e);
        }
        t.models.insert(spec.name.clone(), Arc::clone(&model));
    }
    eprintln!("dovata: adding {}, {} bytes, from {}", model.name, total, spec.parts[0].display());
    let server = Arc::clone(server);
    std::thread::spawn(move || {
        let mut last = Instant::now();
        model.held.load(|n| {
            model.loaded.fetch_add(n, Ordering::Relaxed);
            if last.elapsed() >= PROGRESS_EVERY {
                last = Instant::now();
                rewrite_record(&server, &model);
            }
        });
        model.ready.store(true, Ordering::Release);
        rewrite_record(&server, &model);
        eprintln!("dovata: {} is ready", model.name);
    });
    Ok(())
}

/// Purpose: This function removes a model: its record goes, and the names of its shared
/// memory, so no program can map it again. Programs that mapped it keep their mappings.
///
/// Inputs:
/// - `server`: the process's state.
/// - `name`: the model's name.
///
/// Outputs: `Ok(())`, or [`HostError::Refused`] naming the models the process holds.
fn remove(
    server: &Server,
    name: &str,
) -> Result<(), HostError>
{
    let mut t = table(server);
    let Some(m) = t.models.remove(name) else {
        drop(t);
        return Err(state::not_held(&server.dir, name));
    };
    m.held.unlink();
    let _ = std::fs::remove_dir_all(state::model_dir(&server.dir, name));
    eprintln!("dovata: removed {name}");
    Ok(())
}

/// Purpose: This function writes a model's directory: its plan, its record and the
/// directory programs that map it register in.
///
/// Inputs:
/// - `dir`: the run directory.
/// - `m`: the model.
///
/// Outputs: `Ok(())`, or a [`HostError::Io`].
fn write_files(
    dir: &Path,
    m: &Hosted,
) -> Result<(), HostError>
{
    let clients = state::clients_dir(dir, &m.name);
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&clients).map_err(|e| HostError::Io(format!("{}: {e}", clients.display())))?;
    state::write_json(&state::plan_path(dir, &m.name), &m.plan)?;
    state::write_json(&state::record_path(dir, &m.name), &record(dir, m))
}

/// Purpose: This function rewrites a model's record with its state now, unless the model
/// has been removed or the process is stopping.
///
/// Inputs:
/// - `server`: the process's state.
/// - `m`: the model.
///
/// Outputs: none.
fn rewrite_record(
    server: &Server,
    m: &Arc<Hosted>,
)
{
    let t = table(server);
    if t.models.get(&m.name).is_some_and(|held| Arc::ptr_eq(held, m)) {
        if let Err(e) = state::write_json(&state::record_path(&server.dir, &m.name), &record(&server.dir, m)) {
            eprintln!("dovata: {e}");
        }
    }
}

/// Purpose: This function builds a model's record: where every region of its memory is,
/// and how far loading has got.
///
/// Inputs:
/// - `dir`: the run directory.
/// - `m`: the model.
///
/// Outputs: the [`HeldModel`].
fn record(
    dir: &Path,
    m: &Hosted,
) -> HeldModel
{
    let state = if m.ready.load(Ordering::Acquire) {
        State::Ready
    } else {
        State::Loading { loaded_bytes: m.loaded.load(Ordering::Relaxed), total_bytes: m.total }
    };
    let regions = m
        .plan
        .regions
        .iter()
        .map(|r| {
            let (open, offset, writable) = match r.backing {
                Backing::File { part, file_start } => (Open::File { part: usize::from(part) }, file_start, false),
                Backing::Alloc => (Open::Shared { name: state::shared_name(dir, &m.name, &m.plan.table_hash, r.id) }, 0, true),
            };
            HeldRegion { id: r.id, kind: r.kind, holds: holds(&m.plan, r.id), open, offset, bytes: r.bytes, align: r.align, writable }
        })
        .collect();
    HeldModel {
        format: FORMAT,
        name: m.name.clone(),
        added_unix: m.added,
        state,
        model: m.info.clone(),
        target: m.plan.target,
        table_hash: m.plan.table_hash.clone(),
        files: m.held.parts.iter().map(|p| p.record.clone()).collect(),
        regions,
        tokenizer: m.plan.tokenizer.as_ref().map(|t| TokenizerUse { kind: t.kind.clone(), tokens: t.tokens, stop: t.stop.clone(), sha256: t.sha256.clone() }),
    }
}

/// Purpose: This function plans a model for this machine.
///
/// Inputs:
/// - `spec`: the model and how to plan it.
///
/// Outputs: the plan and the model's description, or a [`HostError`].
fn plan(
    spec: &Spec,
) -> Result<(Plan, ModelInfo), HostError>
{
    let first = spec.parts.first().ok_or_else(|| HostError::Model("the model has no files".into()))?;
    let bad = |e: &dyn std::fmt::Display| HostError::Model(format!("{}: {e}", first.display()));
    let model = GgufModel::from_parts(&spec.parts).map_err(|e| bad(&e))?;
    let caller = match &spec.layout {
        Some(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| HostError::Model(format!("{}: {e}", path.display())))?;
            Some(Layout::from_toml(&text).map_err(|e| HostError::Model(format!("{}: {e}", path.display())))?)
        }
        None => None,
    };
    let layout = layout::for_model(&model, caller).map_err(|e| bad(&e))?;
    let tok = |e: &dyn std::fmt::Display| HostError::Model(format!("{}: {e}", spec.tokenizer.display()));
    let definition = std::fs::read_to_string(&spec.tokenizer).map_err(|e| tok(&e))?;
    let tokenizer = crate::tokenize::describe(&model, definition).map_err(|e| tok(&e))?;
    let machine = crate::machine::current().map_err(|e| HostError::Model(format!("cannot read this machine: {e}")))?;
    let target = machine.target(spec.cpu);
    if let Target::Cuda { .. } = target {
        return Err(HostError::Unsupported(
            "this machine's plan is for CUDA, and dovata holds Apple and CPU plans only so far; add the model with --cpu to hold it in host memory".into(),
        ));
    }
    let plan = crate::memplan::plan(&model, &layout, &spec.step, &target, Some(&tokenizer)).map_err(|e| bad(&e))?;
    let meta = model.meta();
    let info = ModelInfo {
        name: meta.name().map(str::to_owned),
        architecture: model.architecture().to_owned(),
        file_type: meta.file_type().and_then(Ftype::from_file_type).map(|f| f.to_string()),
    };
    Ok((plan, info))
}

/// Purpose: This function takes the run directory for this process: its lock, then the
/// files a killed process left behind, removing the names of that process's shared
/// memory.
///
/// Inputs:
/// - `dir`: the run directory, made if missing, readable by this user only.
///
/// Outputs: the claim that holds the lock and removes the process's files when dropped,
/// or [`HostError::AlreadyRunning`] when another dovata process holds the lock.
fn claim(
    dir: &Path,
) -> Result<Claim, HostError>
{
    let io = |e: io::Error, what: &Path| HostError::Io(format!("{}: {e}", what.display()));
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|e| io(e, dir))?;
    let lock_path = state::lock_path(dir);
    let mut lock = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&lock_path).map_err(|e| io(e, &lock_path))?;
    // A program checking whether dovata runs holds a shared lock for a moment, so a lock
    // that is held is tried again for a little while before another process is said to
    // hold it. Its named holder proves nothing: the file may name a process that is gone
    // and whose id is now another program's, or name no process yet.
    let mut tries = 0;
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if tries < 10 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                let pid = std::fs::read_to_string(&lock_path).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0);
                return Err(HostError::AlreadyRunning(pid));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(io(e, &lock_path)),
        }
    }
    // A killed process's record would say it still runs until this one writes its own.
    let _ = std::fs::remove_file(state::process_path(dir));
    // The lock file names its holder, so a refused start can say which process runs.
    {
        use std::io::{Seek, Write};
        let _ = lock.set_len(0).and_then(|()| lock.rewind()).and_then(|_| write!(lock, "{}", std::process::id())).and_then(|()| lock.flush());
    }
    // A process that was killed left its records, and the names of its shared memory,
    // which would otherwise stay until the machine restarts. Only the names are read, so
    // a record of another version still gives them.
    for name in state::shared_names_in(dir) {
        memory::unlink_shared(&name);
    }
    let claim = Claim { dir: dir.to_path_buf(), _lock: lock };
    let _ = std::fs::remove_dir_all(state::models_dir(dir));
    let _ = std::fs::remove_dir_all(state::requests_dir(dir));
    for d in [state::models_dir(dir), state::requests_dir(dir)] {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&d).map_err(|e| io(e, &d))?;
    }
    Ok(claim)
}

impl Held {
    /// Purpose: This function sets up a plan's memory: it maps each part file once,
    /// read-only, and makes a named shared memory object for each allocated region.
    ///
    /// Inputs:
    /// - `dir`: the run directory, which the shared memory's names come from.
    /// - `name`: the model's name.
    /// - `paths`: the part files, part 1 first.
    /// - `plan`: the plan.
    ///
    /// Outputs: the [`Held`] memory, or a [`HostError`] for a file that cannot be mapped
    /// or a region dovata cannot hold.
    fn set_up(
        dir: &Path,
        name: &str,
        paths: &[PathBuf],
        plan: &Plan,
    ) -> Result<Held, HostError>
    {
        if paths.len() != plan.model.parts.len() {
            return Err(HostError::Model(format!("the plan has {} part files, and the model {}", plan.model.parts.len(), paths.len())));
        }
        let mut parts = Vec::new();
        for path in paths {
            let io = |e: io::Error| HostError::Io(format!("{}: {e}", path.display()));
            let file = File::open(path).map_err(io)?;
            let meta = file.metadata().map_err(io)?;
            let bytes = meta.len();
            let len = usize::try_from(bytes).map_err(|_| HostError::Io(format!("{}: {bytes} bytes do not fit in memory", path.display())))?;
            let map = Map::new(file.as_fd(), 0, len, false).map_err(io)?;
            let record = HeldFile { path: path.clone(), bytes, modified_sec: meta.mtime(), modified_nsec: meta.mtime_nsec() };
            parts.push(Part { map, record });
        }
        let page = memory::page_size() as u64;
        let mut held = Held { parts, allocs: Vec::new() };
        for r in &plan.regions {
            let bad = |m: String| HostError::Model(format!("region {}: {m}", r.id));
            let alloc = match (r.backing, r.kind) {
                (Backing::File { part, file_start }, RegionKind::Mmap) => {
                    let p = held.parts.get(usize::from(part)).ok_or_else(|| bad(format!("it maps part {}, and the model has {}", part + 1, held.parts.len())))?;
                    if file_start.checked_add(r.bytes).is_none_or(|end| end > p.record.bytes) {
                        return Err(bad(format!("its window runs past the end of {}", p.record.path.display())));
                    }
                    None
                }
                (Backing::Alloc, RegionKind::Host | RegionKind::Unified) => {
                    if r.align > page {
                        return Err(bad(format!("it needs alignment {}, more than the {page}-byte pages shared memory starts on", r.align)));
                    }
                    let len = r.bytes.max(1).div_ceil(page) * page;
                    let name = state::shared_name(dir, name, &plan.table_hash, r.id);
                    let io = |e: io::Error| HostError::Io(format!("shared memory {name} of {len} bytes for region {}: {e}", r.id));
                    // A failure removes the names made so far, this one included.
                    let made = memory::create_shared(&name, len);
                    let fd = match made {
                        Ok(fd) => fd,
                        Err(e) => {
                            held.unlink();
                            return Err(io(e));
                        }
                    };
                    match Map::new(fd.as_fd(), 0, len as usize, true) {
                        Ok(map) => Some(Alloc { name, _fd: fd, map }),
                        Err(e) => {
                            memory::unlink_shared(&name);
                            held.unlink();
                            return Err(io(e));
                        }
                    }
                }
                (Backing::Alloc, RegionKind::Vram) => {
                    held.unlink();
                    return Err(HostError::Unsupported(format!("region {} is VRAM, and a dovata process cannot hold CUDA memory yet", r.id)));
                }
                (backing, kind) => {
                    held.unlink();
                    return Err(bad(format!("{kind:?} memory backed by {backing:?} is not a region dovata knows")));
                }
            };
            held.allocs.push(alloc);
        }
        Ok(held)
    }

    /// Purpose: This function removes the names of the model's shared memory, so no
    /// program can open it again. The process and programs that mapped it keep it.
    ///
    /// Inputs: none.
    ///
    /// Outputs: none.
    fn unlink(&self)
    {
        for a in self.allocs.iter().flatten() {
            memory::unlink_shared(&a.name);
        }
    }

    /// Purpose: This function brings the whole model into physical memory: every page of
    /// every part file, and every page of every allocated region.
    ///
    /// Inputs:
    /// - `progress`: called with the bytes done since its last call.
    ///
    /// Outputs: none.
    fn load(
        &self,
        mut progress: impl FnMut(u64),
    )
    {
        for p in &self.parts {
            p.map.touch(&mut progress);
        }
        for a in self.allocs.iter().flatten() {
            a.map.touch(&mut progress);
        }
    }
}
