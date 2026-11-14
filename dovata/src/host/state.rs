//! Author: Joe Ricks
//! File: src/host/state.rs
//! Description: The files the dovata process keeps in its run directory: a record of the
//! process, a record and a plan for every model it holds, the programs that mapped each
//! model, and the requests programs leave for it. Reading them takes no message to the
//! process, which is how a program learns what it holds and where the memory is.

use std::fs::{self, File};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::memplan::Plan;

use super::{check_name, run_dir, HeldModel, HostError, Process};

/// The version of the run directory's files. A program refuses files of another
/// version, so a library and a process of different dovata versions say so instead of
/// misreading each other.
pub const FORMAT: u32 = 1;

/// How long a program waits for the process's record when the process holds its lock
/// with no record: it writes the record just after taking the lock, and removes it just
/// before giving the lock back.
const RECORD_WAIT: Duration = Duration::from_millis(500);

/// Purpose: This struct holds the record the process writes once it takes requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProcessRecord {
    /// [`FORMAT`] in the process.
    pub(crate) format: u32,
    /// Its process id.
    pub(crate) pid: u32,
    /// The dovata version it runs.
    pub(crate) version: String,
    /// When it started, in seconds since 1970.
    pub(crate) started_unix: u64,
}

/// Purpose: This enum lists the requests a program leaves for the process.
#[cfg_attr(not(all(feature = "host", unix)), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    /// Add a model; the request holds its [`super::Spec`] as JSON.
    Add,
    /// Remove a model; the request holds its name.
    Remove,
    /// Stop the process; the request is empty.
    Stop,
}

/// Purpose: This struct holds one request the process took from its run directory.
#[cfg_attr(not(all(feature = "host", unix)), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Request {
    /// The request's id, which names its answer.
    pub(crate) id: String,
    /// What it asks for.
    pub(crate) op: Op,
    /// What it holds.
    pub(crate) body: Vec<u8>,
}

#[cfg_attr(not(all(feature = "host", unix)), allow(dead_code))]
impl Op {
    /// Purpose: This function gives the file extension a request of this kind has.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the extension.
    fn ext(self) -> &'static str
    {
        match self {
            Op::Add => "add",
            Op::Remove => "remove",
            Op::Stop => "stop",
        }
    }

    /// Purpose: This function reads a request's kind from its file extension.
    ///
    /// Inputs:
    /// - `ext`: the extension.
    ///
    /// Outputs: the kind, or `None` for a file that is not a request.
    fn from_ext(
        ext: &str,
    ) -> Option<Op>
    {
        match ext {
            "add" => Some(Op::Add),
            "remove" => Some(Op::Remove),
            "stop" => Some(Op::Stop),
            _ => None,
        }
    }
}

/// Purpose: This function gives the lock file, which the process holds for as long as
/// it runs and which names it.
///
/// Inputs:
/// - `dir`: the run directory.
///
/// Outputs: the path.
pub(crate) fn lock_path(
    dir: &Path,
) -> PathBuf
{
    dir.join("dovata.lock")
}

/// Purpose: This function gives the process's record.
///
/// Inputs:
/// - `dir`: the run directory.
///
/// Outputs: the path.
pub(crate) fn process_path(
    dir: &Path,
) -> PathBuf
{
    dir.join("process")
}

/// Purpose: This function gives the directory that holds one directory per model.
///
/// Inputs:
/// - `dir`: the run directory.
///
/// Outputs: the path.
pub(crate) fn models_dir(
    dir: &Path,
) -> PathBuf
{
    dir.join("models")
}

/// Purpose: This function gives one model's directory.
///
/// Inputs:
/// - `dir`: the run directory.
/// - `name`: the model's name, already checked.
///
/// Outputs: the path.
pub(crate) fn model_dir(
    dir: &Path,
    name: &str,
) -> PathBuf
{
    models_dir(dir).join(name)
}

/// Purpose: This function gives one model's record, a [`HeldModel`].
///
/// Inputs:
/// - `dir`: the run directory.
/// - `name`: the model's name, already checked.
///
/// Outputs: the path.
pub(crate) fn record_path(
    dir: &Path,
    name: &str,
) -> PathBuf
{
    model_dir(dir, name).join("model")
}

/// Purpose: This function gives one model's plan.
///
/// Inputs:
/// - `dir`: the run directory.
/// - `name`: the model's name, already checked.
///
/// Outputs: the path.
pub(crate) fn plan_path(
    dir: &Path,
    name: &str,
) -> PathBuf
{
    model_dir(dir, name).join("plan")
}

/// Purpose: This function gives the directory that holds one empty file per program
/// that mapped a model, named for its process id.
///
/// Inputs:
/// - `dir`: the run directory.
/// - `name`: the model's name, already checked.
///
/// Outputs: the path.
pub(crate) fn clients_dir(
    dir: &Path,
    name: &str,
) -> PathBuf
{
    model_dir(dir, name).join("clients")
}

/// Purpose: This function gives the directory programs leave requests in.
///
/// Inputs:
/// - `dir`: the run directory.
///
/// Outputs: the path.
pub(crate) fn requests_dir(
    dir: &Path,
) -> PathBuf
{
    dir.join("requests")
}

/// Purpose: This function gives the time now.
///
/// Inputs: none.
///
/// Outputs: seconds since 1970.
#[cfg_attr(not(all(feature = "host", unix)), allow(dead_code))]
pub(crate) fn now() -> u64
{
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Purpose: This function makes a name no other file this program writes has, for a
/// temporary file or a request.
///
/// Inputs: none.
///
/// Outputs: the process id, the time in nanoseconds and a counter, joined by dashes.
fn unique() -> String
{
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{}-{nanos}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Purpose: This function writes a file so that a reader sees either the old file or the
/// whole new one, never part of it: it writes a temporary file beside it and renames it
/// into place.
///
/// Inputs:
/// - `path`: the file.
/// - `bytes`: what it should hold.
///
/// Outputs: `Ok(())`, or the system's error.
pub(crate) fn write_atomic(
    path: &Path,
    bytes: &[u8],
) -> io::Result<()>
{
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = path.with_file_name(format!(".{name}.{}", unique()));
    let written = File::create(&tmp).and_then(|mut f| f.write_all(bytes)).and_then(|()| fs::rename(&tmp, path));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// Purpose: This function writes a value as JSON, all at once.
///
/// Inputs:
/// - `path`: the file.
/// - `value`: the value.
///
/// Outputs: `Ok(())`, or a [`HostError::Io`].
#[cfg_attr(not(all(feature = "host", unix)), allow(dead_code))]
pub(crate) fn write_json<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), HostError>
{
    let text = serde_json::to_vec(value).map_err(|e| HostError::Io(format!("{}: {e}", path.display())))?;
    write_atomic(path, &text).map_err(|e| HostError::Io(format!("{}: {e}", path.display())))
}

/// Purpose: This function reads a JSON file into a value.
///
/// Inputs:
/// - `path`: the file.
///
/// Outputs: the value; `Ok(None)` when there is no such file; or a [`HostError`] when
/// the file cannot be read or is not in the expected form.
pub(crate) fn read_json<T: DeserializeOwned>(
    path: &Path,
) -> Result<Option<T>, HostError>
{
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(HostError::Io(format!("{}: {e}", path.display()))),
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|e| {
        HostError::Record(format!("{} is not a file this dovata can read ({e}); the dovata that wrote it may be another version", path.display()))
    })
}

/// Purpose: This function finds the running dovata process, in the run directory
/// `DOVATA_RUN_DIR` or `~/.dovata/run` names. It reads files only, so it works in every
/// build.
///
/// Inputs: none.
///
/// Outputs: the [`Process`]; [`HostError::NotRunning`] when none runs; or
/// [`HostError::Record`] when the one that runs is another dovata version.
pub fn connect() -> Result<Process, HostError>
{
    connect_in(&run_dir()?)
}

/// Purpose: This function finds the dovata process running in a given run directory.
///
/// Inputs:
/// - `dir`: the run directory.
///
/// Outputs: as for [`connect`].
pub(crate) fn connect_in(
    dir: &Path,
) -> Result<Process, HostError>
{
    let record = running(dir)?;
    Ok(Process { dir: dir.to_path_buf(), pid: record.pid })
}

/// Purpose: This function reads the record of the process running in a run directory.
/// The process holds the lock file for as long as it runs, and the system releases it
/// when the process exits, however it exits; so a lock nobody holds means nothing runs,
/// whatever files a killed process left behind.
///
/// Inputs:
/// - `dir`: the run directory.
///
/// Outputs: the [`ProcessRecord`], or a [`HostError`] as for [`connect`].
pub(crate) fn running(
    dir: &Path,
) -> Result<ProcessRecord, HostError>
{
    let lock_path = lock_path(dir);
    let start = Instant::now();
    loop {
        if !lock_held(&lock_path)? {
            return Err(HostError::NotRunning);
        }
        match read_json::<ProcessRecord>(&process_path(dir))? {
            Some(r) if r.format == FORMAT => return Ok(r),
            Some(r) => {
                return Err(HostError::Record(format!(
                    "dovata {} is running, pid {}, and keeps {} in format {}, while this program reads format {FORMAT}; use the same dovata version",
                    r.version,
                    r.pid,
                    dir.display(),
                    r.format
                )))
            }
            // A process starting or stopping holds the lock with no record for a moment.
            None if start.elapsed() < RECORD_WAIT => std::thread::sleep(Duration::from_millis(10)),
            None => {
                let pid = fs::read_to_string(&lock_path).ok().and_then(|t| t.trim().parse::<u32>().ok()).unwrap_or(0);
                return Err(HostError::Record(format!(
                    "a dovata older than 3.2.2 is running, pid {pid}, which hands out memory over a socket this program no longer uses; stop it with `kill {pid}` and start dovata again"
                )));
            }
        }
    }
}

/// Purpose: This function says whether a process holds the run directory's lock. A
/// shared lock is refused only while the process holds its own; one this function is
/// given goes back when the file closes at its end.
///
/// Inputs:
/// - `lock_path`: the lock file.
///
/// Outputs: `true` while the lock is held, or a [`HostError::Io`].
fn lock_held(
    lock_path: &Path,
) -> Result<bool, HostError>
{
    let lock = match File::open(lock_path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(HostError::Io(format!("{}: {e}", lock_path.display()))),
    };
    match lock.try_lock_shared() {
        Ok(()) => Ok(false),
        Err(fs::TryLockError::WouldBlock) => Ok(true),
        Err(fs::TryLockError::Error(e)) => Err(HostError::Io(format!("{}: {e}", lock_path.display()))),
    }
}

/// Purpose: This function lists the names of the shared memory every model record in a
/// run directory gives, reading only those names, so a record this dovata cannot read
/// whole, such as one a killed process of another version left, still gives them.
///
/// Inputs:
/// - `dir`: the run directory.
///
/// Outputs: the names.
#[cfg_attr(not(all(feature = "host", unix)), allow(dead_code))]
pub(crate) fn shared_names_in(
    dir: &Path,
) -> Vec<String>
{
    let Ok(entries) = fs::read_dir(models_dir(dir)) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Ok(bytes) = fs::read(entry.path().join("model")) else { continue };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { continue };
        for r in v["regions"].as_array().into_iter().flatten() {
            if let Some(name) = r["open"]["name"].as_str().filter(|n| n.starts_with("/dovata.") && !n[1..].contains('/')) {
                out.push(name.to_owned());
            }
        }
    }
    out
}

/// Purpose: This function reads the record of every model the process holds.
///
/// Inputs:
/// - `dir`: the run directory.
///
/// Outputs: the models, sorted by name, or a [`HostError`].
pub(crate) fn models(
    dir: &Path,
) -> Result<Vec<HeldModel>, HostError>
{
    let root = models_dir(dir);
    let entries = match fs::read_dir(&root) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(HostError::Io(format!("{}: {e}", root.display()))),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
        if check_name(&name).is_err() {
            continue;
        }
        // A model being added has a directory before it has a record.
        if let Some(m) = read_json::<HeldModel>(&record_path(dir, &name))? {
            out.push(checked(m)?);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Purpose: This function reads the record of one model the process holds.
///
/// Inputs:
/// - `dir`: the run directory.
/// - `name`: the model's name.
///
/// Outputs: the [`HeldModel`], or [`HostError::Refused`] naming the models the process
/// holds.
pub(crate) fn model(
    dir: &Path,
    name: &str,
) -> Result<HeldModel, HostError>
{
    if check_name(name).is_ok() {
        if let Some(m) = read_json::<HeldModel>(&record_path(dir, name))? {
            return checked(m);
        }
    }
    Err(not_held(dir, name))
}

/// Purpose: This function reads one model's whole plan.
///
/// Inputs:
/// - `dir`: the run directory.
/// - `name`: the model's name.
///
/// Outputs: the [`Plan`], or [`HostError::Refused`] naming the models the process holds.
pub(crate) fn plan(
    dir: &Path,
    name: &str,
) -> Result<Plan, HostError>
{
    if check_name(name).is_ok() {
        if let Some(p) = read_json::<Plan>(&plan_path(dir, name))? {
            return Ok(p);
        }
    }
    Err(not_held(dir, name))
}

/// Purpose: This function refuses a model record of another format.
///
/// Inputs:
/// - `m`: the record.
///
/// Outputs: the record, or a [`HostError::Record`].
fn checked(
    m: HeldModel,
) -> Result<HeldModel, HostError>
{
    if m.format != FORMAT {
        return Err(HostError::Record(format!(
            "the record of {} is in format {}, and this program reads format {FORMAT}; use the same dovata version",
            m.name, m.format
        )));
    }
    Ok(m)
}

/// Purpose: This function says the process holds no model of a name, and which models
/// it holds.
///
/// Inputs:
/// - `dir`: the run directory.
/// - `name`: the name asked for.
///
/// Outputs: a [`HostError::Refused`].
pub(crate) fn not_held(
    dir: &Path,
    name: &str,
) -> HostError
{
    let names: Vec<String> = models(dir).unwrap_or_default().into_iter().map(|m| m.name).collect();
    let held = if names.is_empty() { "no models".to_owned() } else { names.join(", ") };
    HostError::Refused(format!("dovata holds no model named {name}; it holds {held}"))
}

/// Purpose: This function gives the name of the shared memory object that holds one
/// allocated region of a model. The name is the same every time the same model is added
/// with the same plan to a process in the same run directory, so a program compiled
/// against it keeps working after dovata restarts; a different plan or run directory
/// gives a different name.
///
/// Inputs:
/// - `dir`: the run directory.
/// - `model`: the model's name.
/// - `table_hash`: the plan's table hash.
/// - `region`: the region's id.
///
/// Outputs: a name such as `/dovata.1f2e3d4c5b6a7980.2`, at most 30 bytes, within the 31
/// macOS allows.
#[cfg_attr(not(all(feature = "host", unix)), allow(dead_code))]
pub(crate) fn shared_name(
    dir: &Path,
    model: &str,
    table_hash: &str,
    region: u16,
) -> String
{
    // The directory as the system names it, so `~/.dovata/run/` with a slash or through
    // a link gives the same names.
    let dir = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut h = Sha256::new();
    #[cfg(unix)]
    h.update(dir.as_os_str().as_bytes());
    #[cfg(not(unix))]
    h.update(dir.to_string_lossy().as_bytes());
    h.update([0]);
    h.update(model.as_bytes());
    h.update([0]);
    h.update(table_hash.as_bytes());
    let digest = h.finalize();
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("/dovata.{hex}.{region}")
}

/// Purpose: This function leaves a request for the process in its run directory.
///
/// Inputs:
/// - `dir`: the run directory.
/// - `op`: what the request asks for.
/// - `body`: what it holds.
///
/// Outputs: the request's id, or a [`HostError::Io`].
#[cfg_attr(not(all(feature = "host", unix)), allow(dead_code))]
pub(crate) fn leave_request(
    dir: &Path,
    op: Op,
    body: &[u8],
) -> Result<String, HostError>
{
    let id = unique();
    let path = requests_dir(dir).join(format!("{id}.{}", op.ext()));
    write_atomic(&path, body).map_err(|e| HostError::Io(format!("{}: {e}", path.display())))?;
    Ok(id)
}

/// Purpose: This function takes every request waiting in the run directory, renaming
/// each so it is taken once.
///
/// Inputs:
/// - `dir`: the run directory.
///
/// Outputs: the requests, oldest first by id.
#[cfg_attr(not(all(feature = "host", unix)), allow(dead_code))]
pub(crate) fn take_requests(
    dir: &Path,
) -> Vec<Request>
{
    let root = requests_dir(dir);
    let Ok(entries) = fs::read_dir(&root) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let file = entry.file_name();
        let Some(file) = file.to_str() else { continue };
        // Temporary files start with a dot; answers and taken requests have other
        // extensions.
        let Some((id, ext)) = file.rsplit_once('.') else { continue };
        let Some(op) = Op::from_ext(ext) else { continue };
        if id.is_empty() || id.starts_with('.') {
            continue;
        }
        let taken = root.join(format!("{id}.taken"));
        if fs::rename(entry.path(), &taken).is_err() {
            continue;
        }
        let body = fs::read(&taken).unwrap_or_default();
        out.push(Request { id: id.to_owned(), op, body });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Purpose: This function answers a request the process took.
///
/// Inputs:
/// - `dir`: the run directory.
/// - `id`: the request's id.
/// - `result`: `Ok(())`, or why the request was refused.
///
/// Outputs: none. A program that has stopped waiting leaves the answer behind, and the
/// next start of the process removes it.
#[cfg_attr(not(all(feature = "host", unix)), allow(dead_code))]
pub(crate) fn answer(
    dir: &Path,
    id: &str,
    result: Result<(), String>,
)
{
    let root = requests_dir(dir);
    let written = match &result {
        Ok(()) => write_atomic(&root.join(format!("{id}.done")), b""),
        Err(why) => write_atomic(&root.join(format!("{id}.error")), why.as_bytes()),
    };
    if let Err(e) = written {
        eprintln!("dovata: could not answer request {id}: {e}");
    }
    let _ = fs::remove_file(root.join(format!("{id}.taken")));
}

/// Purpose: This function reads the process's answer to a request, and removes it.
///
/// Inputs:
/// - `dir`: the run directory.
/// - `id`: the request's id.
///
/// Outputs: `None` while there is no answer yet; else `Ok(())`, or why the request was
/// refused.
#[cfg_attr(not(all(feature = "host", unix)), allow(dead_code))]
pub(crate) fn read_answer(
    dir: &Path,
    id: &str,
) -> Option<Result<(), String>>
{
    let root = requests_dir(dir);
    let done = root.join(format!("{id}.done"));
    if fs::remove_file(&done).is_ok() {
        return Some(Ok(()));
    }
    let error = root.join(format!("{id}.error"));
    let why = fs::read_to_string(&error).ok()?;
    let _ = fs::remove_file(&error);
    Some(Err(why))
}

/// Purpose: This function takes back a request the process has not taken yet.
///
/// Inputs:
/// - `dir`: the run directory.
/// - `id`: the request's id.
/// - `op`: what it asked for.
///
/// Outputs: none.
#[cfg_attr(not(all(feature = "host", unix)), allow(dead_code))]
pub(crate) fn withdraw(
    dir: &Path,
    id: &str,
    op: Op,
)
{
    let _ = fs::remove_file(requests_dir(dir).join(format!("{id}.{}", op.ext())));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_memory_names_fit_and_differ_by_plan_and_directory()
    {
        let a = shared_name(Path::new("/home/u/.dovata/run"), "qwen3-8b", "ab", 65535);
        assert!(a.starts_with("/dovata.") && a.len() <= 31 && !a[1..].contains('/'), "{a}");
        assert_eq!(a, shared_name(Path::new("/home/u/.dovata/run"), "qwen3-8b", "ab", 65535));
        assert_ne!(a, shared_name(Path::new("/home/u/.dovata/run"), "qwen3-8b", "cd", 65535));
        assert_ne!(a, shared_name(Path::new("/home/v/.dovata/run"), "qwen3-8b", "ab", 65535));
        assert_ne!(a, shared_name(Path::new("/home/u/.dovata/run"), "qwen3-4b", "ab", 65535));
    }

    #[test]
    fn requests_are_taken_once_and_answered()
    {
        let dir = std::env::temp_dir().join(format!("dvs-{}", unique()));
        fs::create_dir_all(requests_dir(&dir)).unwrap();
        let add = leave_request(&dir, Op::Add, b"{}").unwrap();
        let stop = leave_request(&dir, Op::Stop, b"").unwrap();
        fs::write(requests_dir(&dir).join("x.unknown"), b"").unwrap();
        let taken = take_requests(&dir);
        let got: Vec<(&str, Op, &[u8])> = taken.iter().map(|r| (r.id.as_str(), r.op, r.body.as_slice())).collect();
        assert_eq!(got, [(add.as_str(), Op::Add, b"{}".as_slice()), (stop.as_str(), Op::Stop, b"".as_slice())]);
        assert!(take_requests(&dir).is_empty(), "a request is taken once");
        assert_eq!(read_answer(&dir, &add), None);
        answer(&dir, &add, Err("no".into()));
        answer(&dir, &stop, Ok(()));
        assert_eq!(read_answer(&dir, &add), Some(Err("no".into())));
        assert_eq!(read_answer(&dir, &stop), Some(Ok(())));
        assert_eq!(read_answer(&dir, &stop), None, "an answer is read once");
        let left = leave_request(&dir, Op::Remove, b"m").unwrap();
        withdraw(&dir, &left, Op::Remove);
        assert!(take_requests(&dir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_runs_without_a_held_lock()
    {
        let dir = std::env::temp_dir().join(format!("dvs-{}", unique()));
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(running(&dir), Err(HostError::NotRunning));
        // A lock file left by a killed process, and its record, mean nothing.
        fs::write(lock_path(&dir), b"12345").unwrap();
        write_json(&process_path(&dir), &ProcessRecord { format: FORMAT, pid: 12345, version: "x".into(), started_unix: 0 }).unwrap();
        assert_eq!(running(&dir), Err(HostError::NotRunning));
        // A held lock with no record is a dovata from before the run directory had one.
        let held = File::options().read(true).write(true).open(lock_path(&dir)).unwrap();
        held.lock().unwrap();
        let _ = fs::remove_file(process_path(&dir));
        assert!(matches!(running(&dir), Err(HostError::Record(m)) if m.contains("older than 3.2.2") && m.contains("kill 12345")));
        write_json(&process_path(&dir), &ProcessRecord { format: FORMAT + 1, pid: 12345, version: "9.0.0".into(), started_unix: 0 }).unwrap();
        assert!(matches!(running(&dir), Err(HostError::Record(m)) if m.contains("dovata 9.0.0")));
        write_json(&process_path(&dir), &ProcessRecord { format: FORMAT, pid: 12345, version: "x".into(), started_unix: 0 }).unwrap();
        assert_eq!(connect_in(&dir).map(|p| p.pid), Ok(12345));
        drop(held);
        let _ = fs::remove_dir_all(&dir);
    }
}
