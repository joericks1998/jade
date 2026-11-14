//! Author: Joe Ricks
//! File: src/host/client.rs
//! Description: Starts the dovata process from another program, leaves it requests and
//! signals it to take them, reads its status from its run directory, and maps a model's
//! memory by opening each file and shared memory object by the name its record gives.

use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::hf_hub::{self, ModelSource};

use super::memory::{self, Map};
use super::signal;
use super::state::{self, Op, FORMAT};
use super::{
    log_path, run_dir, spec, AddOptions, Client, FileUse, HeldModel, HostError, MappedRegion, Mapping, ModelStatus, Open, Process, RegionUse, StartLine,
    StartOptions, State, Status,
};

/// How long [`Process::kill`] waits for a stopped process to exit.
const STOP_WAIT: Duration = Duration::from_secs(10);

/// How long a program waits for the process to answer an add, which plans the model
/// first: a few seconds for a large one.
const ADD_WAIT: Duration = Duration::from_secs(300);

/// How long a program waits for the process to answer any other request.
const OTHER_WAIT: Duration = Duration::from_secs(30);

/// How often a program looks for the answer to its request.
const ANSWER_POLL: Duration = Duration::from_millis(10);

/// How often [`Process::add`] reads whether the model it added is in memory yet.
const LOAD_POLL: Duration = Duration::from_millis(50);

/// Purpose: This function starts the dovata process, with no models in it.
///
/// Inputs:
/// - `opts`: which `dovata` program to run.
///
/// Outputs: the [`Process`] once it takes requests, or a [`HostError`], such as
/// [`HostError::AlreadyRunning`] or the process's own reason for stopping.
pub fn start(
    opts: &StartOptions,
) -> Result<Process, HostError>
{
    let dir = run_dir()?;
    match state::connect_in(&dir) {
        Ok(p) => return Err(HostError::AlreadyRunning(p.pid)),
        Err(HostError::NotRunning) => {}
        Err(e) => return Err(e),
    }
    let io = |e: io::Error, what: &Path| HostError::Io(format!("{}: {e}", what.display()));
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).map_err(|e| io(e, &dir))?;
    let log = log_path(&dir);
    let log_file = std::fs::OpenOptions::new().create(true).append(true).open(&log).map_err(|e| io(e, &log))?;

    let program = opts.program.clone().or_else(|| std::env::var_os("DOVATA_PROGRAM").map(PathBuf::from)).unwrap_or_else(|| PathBuf::from("dovata"));
    // `dovata host` writes one line to standard output once it takes requests, and its
    // log to standard error. Its own process group keeps a Ctrl-C at this program's
    // terminal from reaching it.
    let mut child = Command::new(&program)
        .arg("host")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(log_file)
        .current_dir("/")
        .process_group(0)
        .spawn()
        .map_err(|e| HostError::Start(format!("cannot run {}: {e}. Install dovata, or set DOVATA_PROGRAM to its path", program.display())))?;
    let mut line = String::new();
    if let Some(stdout) = child.stdout.take() {
        let _ = BufReader::new(stdout).read_line(&mut line);
    }
    match serde_json::from_str::<StartLine>(line.trim()) {
        Ok(StartLine::Ready(r)) if r.format == FORMAT => {
            // The process outlives this call. Waiting for it on a thread of its own
            // reaps it when it exits, so it does not linger as a zombie while this
            // program runs.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(Process { dir: r.dir, pid: r.pid })
        }
        Ok(StartLine::Ready(r)) => {
            let _ = kill(&Process { dir: r.dir, pid: r.pid });
            let _ = child.wait();
            Err(HostError::Start(format!(
                "{} is dovata {} and writes format {}, and this program reads format {FORMAT}; use the same dovata version",
                program.display(),
                r.version,
                r.format
            )))
        }
        Ok(StartLine::Error(m)) => {
            let _ = child.wait();
            Err(HostError::Start(m))
        }
        Err(_) => {
            let code = child.wait().map(|s| s.to_string()).unwrap_or_else(|e| e.to_string());
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            let tail: Vec<&str> = text.lines().rev().take(5).collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            Err(HostError::Start(format!("{} {code}; its log {} ends:\n{}", program.display(), log.display(), tail.join("\n"))))
        }
    }
}

/// Purpose: This function leaves the process a request, signals it, and waits for its
/// answer.
///
/// Inputs:
/// - `p`: the process.
/// - `op`: what the request asks for.
/// - `body`: what it holds.
/// - `wait`: the longest to wait for the answer.
///
/// Outputs: `Ok(())`, or a [`HostError`]: [`HostError::NotRunning`] when the process has
/// stopped, and [`HostError::Refused`] when it refuses the request.
fn ask(
    p: &Process,
    op: Op,
    body: &[u8],
    wait: Duration,
) -> Result<(), HostError>
{
    state::running(&p.dir)?;
    let id = state::leave_request(&p.dir, op, body)?;
    // The process also looks for requests every second, so a signal that cannot be sent
    // only slows the answer.
    let _ = signal::notify(p.pid);
    let start = Instant::now();
    let mut checked = start;
    loop {
        if let Some(answer) = state::read_answer(&p.dir, &id) {
            return answer.map_err(HostError::Refused);
        }
        if checked.elapsed() >= Duration::from_secs(1) {
            checked = Instant::now();
            if let Err(e) = state::running(&p.dir) {
                state::withdraw(&p.dir, &id, op);
                return Err(e);
            }
        }
        if start.elapsed() >= wait {
            state::withdraw(&p.dir, &id, op);
            return Err(HostError::Io(format!(
                "dovata did not answer within {} s; its log is {}",
                wait.as_secs(),
                log_path(&p.dir).display()
            )));
        }
        std::thread::sleep(ANSWER_POLL);
    }
}

/// Purpose: This function reads the process's status from its run directory, with how
/// much of each model is in physical memory right now.
///
/// Inputs:
/// - `p`: the process.
///
/// Outputs: the [`Status`], or a [`HostError`].
pub(crate) fn status(
    p: &Process,
) -> Result<Status, HostError>
{
    let record = state::running(&p.dir)?;
    let models = state::models(&p.dir)?;
    Ok(Status { pid: record.pid, version: record.version, started_unix: record.started_unix, models: models.iter().map(|m| model_status(&p.dir, m)).collect() })
}

/// Purpose: This function reads one model's status from its record, counting each part
/// file's and each region's bytes in physical memory with `mincore` on a mapping of its
/// own. Pages in memory belong to the file or object, not to one mapping, so a fresh
/// mapping sees the pages the process and programs read in.
///
/// Inputs:
/// - `dir`: the run directory.
/// - `m`: the model's record.
///
/// Outputs: the [`ModelStatus`].
fn model_status(
    dir: &Path,
    m: &HeldModel,
) -> ModelStatus
{
    let file_maps: Vec<Option<Map>> = m.files.iter().map(|f| map_whole(&f.path, f.bytes)).collect();
    let files = m
        .files
        .iter()
        .zip(&file_maps)
        .map(|(f, map)| FileUse { path: f.path.clone(), bytes: f.bytes, resident_bytes: map.as_ref().and_then(|x| x.resident(0, x.len()).ok()).unwrap_or(0) })
        .collect();
    let regions = m
        .regions
        .iter()
        .map(|r| {
            let len = usize::try_from(r.bytes).unwrap_or(0);
            let resident = match &r.open {
                Open::File { part } => file_maps.get(*part).and_then(Option::as_ref).and_then(|x| x.resident(r.offset as usize, len).ok()),
                Open::Shared { name } => memory::open_shared(name, false).ok().and_then(|fd| map_object(fd.as_fd(), len)).and_then(|x| x.resident(0, len).ok()),
            };
            RegionUse { id: r.id, kind: r.kind, holds: r.holds.clone(), bytes: r.bytes, resident_bytes: resident.unwrap_or(0) }
        })
        .collect();
    ModelStatus {
        name: m.name.clone(),
        added_unix: m.added_unix,
        state: m.state,
        model: m.model.clone(),
        target: m.target,
        table_hash: m.table_hash.clone(),
        files,
        regions,
        clients: clients(dir, &m.name),
        tokenizer: m.tokenizer.clone(),
    }
}

/// Purpose: This function maps a whole file read-only, to count its pages in memory.
///
/// Inputs:
/// - `path`: the file.
/// - `bytes`: its size as the record gives it.
///
/// Outputs: the mapping, or `None` when the file cannot be opened or mapped.
fn map_whole(
    path: &Path,
    bytes: u64,
) -> Option<Map>
{
    let file = File::open(path).ok()?;
    let size = memory::object_size(file.as_fd()).ok()?;
    map_object(file.as_fd(), usize::try_from(size.min(bytes)).ok()?)
}

/// Purpose: This function maps the start of a file or object read-only.
///
/// Inputs:
/// - `fd`: the file or object.
/// - `len`: the bytes to map.
///
/// Outputs: the mapping, or `None` for no bytes or a failed map.
fn map_object(
    fd: BorrowedFd<'_>,
    len: usize,
) -> Option<Map>
{
    (len > 0).then(|| Map::new(fd, 0, len, false).ok()).flatten()
}

/// Purpose: This function lists the programs that mapped a model.
///
/// Inputs:
/// - `dir`: the run directory.
/// - `name`: the model's name.
///
/// Outputs: each program, in the order they mapped the model.
fn clients(
    dir: &Path,
    name: &str,
) -> Vec<Client>
{
    let Ok(entries) = std::fs::read_dir(state::clients_dir(dir, name)) else { return Vec::new() };
    let mut out: Vec<Client> = entries
        .flatten()
        .filter_map(|e| {
            let pid = e.file_name().to_str()?.parse::<u32>().ok()?;
            let mapped = e.metadata().ok()?.modified().ok()?.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
            Some(Client { pid, mapped_unix: mapped, alive: memory::alive(pid) })
        })
        .collect();
    out.sort_by_key(|c| (c.mapped_unix, c.pid));
    out
}

/// Purpose: This function gets a model and adds it to the process.
///
/// Inputs:
/// - `p`: the process.
/// - `model`: a GGUF file, or a Hugging Face link to a repo or to one file in it.
/// - `opts`: the add options.
///
/// Outputs: the model's status once it is in memory, or a [`HostError`].
pub(crate) fn add(
    p: &Process,
    model: &str,
    opts: &AddOptions,
) -> Result<ModelStatus, HostError>
{
    // Checking first means a stopped process is reported before anything is downloaded.
    state::running(&p.dir)?;
    let source = ModelSource::parse(model).map_err(|e| HostError::Model(e.to_string()))?;
    let fetched = hf_hub::fetch(&source, opts.want, &opts.fetch).map_err(|e| HostError::Model(e.to_string()))?;
    // A model from the Hub finds its tokenizer through its repo too, which a later step
    // given only the files cannot.
    let tokenizer = match &opts.tokenizer {
        Some(t) => t.clone(),
        None => {
            let gguf = crate::load::Model::from_parts(&fetched.parts).map_err(|e| HostError::Model(e.to_string()))?;
            let origin = source.origin(fetched.revision.as_deref());
            crate::tokenize::find(&fetched.parts[0], gguf.meta(), origin.as_ref(), &opts.fetch).map_err(|e| HostError::Model(e.to_string()))?
        }
    };
    add_parts(p, &fetched.parts, &AddOptions { tokenizer: Some(tokenizer), ..opts.clone() })
}

/// Purpose: This function adds a model already on disk to the process, and waits until
/// it is in memory.
///
/// Inputs:
/// - `p`: the process.
/// - `parts`: every part file of the model, part 1 first.
/// - `opts`: the add options.
///
/// Outputs: the model's status once it is in memory, or a [`HostError`].
pub(crate) fn add_parts(
    p: &Process,
    parts: &[PathBuf],
    opts: &AddOptions,
) -> Result<ModelStatus, HostError>
{
    let spec = spec(parts, opts)?;
    let body = serde_json::to_vec(&spec).map_err(|e| HostError::Io(e.to_string()))?;
    ask(p, Op::Add, &body, ADD_WAIT)?;
    loop {
        match state::model(&p.dir, &spec.name) {
            Ok(m) if m.state == State::Ready => return Ok(model_status(&p.dir, &m)),
            Ok(_) => std::thread::sleep(LOAD_POLL),
            Err(HostError::Refused(_)) => {
                state::running(&p.dir)?;
                return Err(HostError::Refused(format!("{} was removed before it finished loading", spec.name)));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Purpose: This function removes a model from the process.
///
/// Inputs:
/// - `p`: the process.
/// - `name`: the model's name.
///
/// Outputs: `Ok(())`, or a [`HostError`].
pub(crate) fn remove(
    p: &Process,
    name: &str,
) -> Result<(), HostError>
{
    ask(p, Op::Remove, name.as_bytes(), OTHER_WAIT)
}

/// Purpose: This function builds one model's tokenizer from its plan.
///
/// Inputs:
/// - `p`: the process.
/// - `name`: the model's name.
/// - `table_hash`: the table hash of the plan this program was compiled against.
///
/// Outputs: the tokenizer, or a [`HostError`].
#[cfg(feature = "tokenize")]
pub(crate) fn tokenizer(
    p: &Process,
    name: &str,
    table_hash: &str,
) -> Result<crate::tokenize::Tokenizer, HostError>
{
    let plan = state::plan(&p.dir, name)?;
    if plan.table_hash != table_hash {
        return Err(HostError::Refused(format!(
            "dovata holds {name} with a plan whose table hash is {}, and the program comes from a plan with table hash {table_hash}",
            plan.table_hash
        )));
    }
    let t = plan.tokenizer.ok_or_else(|| HostError::Record(format!("{name}'s plan carries no tokenizer")))?;
    crate::tokenize::Tokenizer::from_plan(&t).map_err(|e| HostError::Model(e.to_string()))
}

/// Purpose: This function maps one model's memory into this program, opening each part
/// file and shared memory object by the name the model's record gives, and registers
/// this program as one that mapped it.
///
/// Inputs:
/// - `p`: the process.
/// - `name`: the model's name.
/// - `table_hash`: the table hash of the plan this program's offsets came from.
///
/// Outputs: the [`Mapping`], or a [`HostError`].
pub(crate) fn map(
    p: &Process,
    name: &str,
    table_hash: &str,
) -> Result<Mapping, HostError>
{
    let m = state::model(&p.dir, name)?;
    if m.table_hash != table_hash {
        return Err(HostError::Refused(format!(
            "dovata holds {name} with a plan whose table hash is {}, and the program's offsets come from a plan with table hash {table_hash}; add the model with the options the program was compiled with",
            m.table_hash
        )));
    }
    if let State::Loading { loaded_bytes, total_bytes } = m.state {
        return Err(HostError::Refused(format!("{name} is still loading: {loaded_bytes} of {total_bytes} bytes")));
    }
    // A file changed in place changes the bytes under every mapping, and a shorter one
    // makes a program that reads past its new end crash with SIGBUS.
    let mut files = Vec::new();
    for f in &m.files {
        let io = |e: io::Error| HostError::Io(format!("{}: {e}", f.path.display()));
        let file = File::open(&f.path).map_err(io)?;
        let meta = file.metadata().map_err(io)?;
        if meta.len() != f.bytes || meta.mtime() != f.modified_sec || meta.mtime_nsec() != f.modified_nsec {
            return Err(HostError::Refused(format!(
                "{} changed on disk after dovata loaded it; remove the model with `dovata remove {name}` and add it again",
                f.path.display()
            )));
        }
        files.push(file);
    }
    let mut regions = Vec::new();
    for r in &m.regions {
        let shared: OwnedFd;
        let fd = match &r.open {
            Open::File { part } => files.get(*part).ok_or_else(|| HostError::Record(format!("region {} names part {part}, of {}", r.id, files.len())))?.as_fd(),
            Open::Shared { name: object } => {
                shared = memory::open_shared(object, r.writable).map_err(|e| {
                    HostError::Refused(format!("dovata no longer holds the memory of {name}'s region {} ({object}: {e}); add the model again", r.id))
                })?;
                shared.as_fd()
            }
        };
        let len = usize::try_from(r.bytes).map_err(|_| HostError::Io(format!("region {}: {} bytes do not fit in memory", r.id, r.bytes)))?;
        let size = memory::object_size(fd).map_err(|e| HostError::Io(format!("region {}: {e}", r.id)))?;
        if r.offset.checked_add(r.bytes).is_none_or(|end| end > size) {
            return Err(HostError::Refused(format!("region {} runs past the end of its {size}-byte file; the model changed on disk", r.id)));
        }
        let (map, delta) = Map::range(fd, r.offset, len, r.writable).map_err(|e| HostError::Io(format!("mapping region {}: {e}", r.id)))?;
        // SAFETY: `delta` is less than a page and the mapping is `delta + len` bytes
        // long, so the base lies inside it.
        let base = unsafe { std::ptr::NonNull::new_unchecked(map.as_ptr().as_ptr().add(delta)) };
        if r.align > 1 && !(base.as_ptr() as usize as u64).is_multiple_of(r.align) {
            return Err(HostError::Io(format!("region {} was mapped at {:p}, which is not aligned to {}", r.id, base.as_ptr(), r.align)));
        }
        regions.push(MappedRegion { id: r.id, kind: r.kind, base, len, writable: r.writable, _map: map });
    }
    // Status lists the programs that mapped a model, by the time each first did. A
    // program that cannot register still has its memory.
    let _ = std::fs::OpenOptions::new().create(true).append(true).open(state::clients_dir(&p.dir, name).join(std::process::id().to_string()));
    Ok(Mapping { table_hash: m.table_hash, regions })
}

/// Purpose: This function stops the process and waits until it has gone.
///
/// Inputs:
/// - `p`: the process.
///
/// Outputs: `Ok(())` once the process has released its lock, or a [`HostError`].
pub(crate) fn kill(
    p: &Process,
) -> Result<(), HostError>
{
    // A process that stops before its answer is read has done what was asked.
    match ask(p, Op::Stop, b"", OTHER_WAIT) {
        Ok(()) | Err(HostError::NotRunning) => {}
        Err(e) => return Err(e),
    }
    let start = Instant::now();
    while start.elapsed() < STOP_WAIT {
        if let Err(HostError::NotRunning) = state::running(&p.dir) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(HostError::Io(format!("dovata said it would stop, and still runs after {} s", STOP_WAIT.as_secs())))
}
