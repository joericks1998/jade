//! Author: Joe Ricks
//! File: src/quantize/install.rs
//! Description: Installs llama.cpp's own release build, which holds the llama-quantize
//! program dovata runs, into ~/.dovata/llama.cpp.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use super::exec::PROGRAM;
use super::QuantError;

/// The llama.cpp build dovata installs, which is llama.cpp's release v0.9.0. Its
/// `llama-quantize` offers the same types as v0.8.0's, which the type table was checked
/// against.
pub const LLAMA_BUILD: &str = "b12110";

/// The largest file dovata downloads here. llama.cpp's archives are about 20 MiB, and
/// dovata's own about 10 MiB.
const MAX_DOWNLOAD: u64 = 512 << 20;

/// Purpose: This struct names one machine's llama.cpp archive and its SHA-256.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Archive {
    /// The machine, in words, such as `macOS on Apple Silicon`.
    pub machine: &'static str,
    /// The file's name in llama.cpp's release.
    pub name: &'static str,
    /// The file's SHA-256 in hex, as GitHub reports it for the release.
    pub sha256: &'static str,
}

/// The archive for Apple Silicon Macs.
const MACOS_ARM64: Archive = Archive {
    machine: "macOS on Apple Silicon",
    name: "llama-b12110-bin-macos-arm64.tar.gz",
    sha256: "4f5b8de12e0b9516658ca6a5764c6ffb3d7c659c3dd538556b1413c7b4050662",
};

/// The archive for x86_64 Linux. llama.cpp builds it on Ubuntu, and it needs glibc 2.34
/// or newer.
const LINUX_X86_64: Archive = Archive {
    machine: "x86_64 Linux",
    name: "llama-b12110-bin-ubuntu-x64.tar.gz",
    sha256: "368227edcc229be5f9bed16a2223d79b5e41d0ac2e19bcf5825e82291c060e55",
};

/// Purpose: This function picks the llama.cpp archive for a machine.
///
/// Inputs:
/// - `os`: the operating system, as `std::env::consts::OS` names it.
/// - `arch`: the processor, as `std::env::consts::ARCH` names it.
///
/// Outputs: the archive, or `None` for a machine dovata releases no programs for.
pub fn archive_for(
    os: &str,
    arch: &str,
) -> Option<Archive>
{
    match (os, arch) {
        ("macos", "aarch64") => Some(MACOS_ARM64),
        ("linux", "x86_64") => Some(LINUX_X86_64),
        _ => None,
    }
}

/// Purpose: This function gives the directory dovata installs llama.cpp into.
///
/// Inputs: none; it reads `HOME`.
///
/// Outputs: `~/.dovata/llama.cpp`, or `None` when `HOME` is not set.
pub fn default_root() -> Option<PathBuf>
{
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join(".dovata").join("llama.cpp"))
}

/// Purpose: This function finds the pinned build's `llama-quantize` in a directory.
///
/// Inputs:
/// - `root`: the directory, such as [`default_root`].
///
/// Outputs: `<root>/b12110/llama-quantize`, or `None` when it is not there.
pub fn installed(
    root: &Path,
) -> Option<PathBuf>
{
    installed_build(root, LLAMA_BUILD)
}

/// Purpose: This function finds one build's `llama-quantize` in a directory.
///
/// Inputs:
/// - `root`: the directory.
/// - `build`: the build, such as `b12110`.
///
/// Outputs: the program's path, or `None` when it is not there.
fn installed_build(
    root: &Path,
    build: &str,
) -> Option<PathBuf>
{
    Some(root.join(build).join(PROGRAM)).filter(|p| p.is_file())
}

/// Purpose: This function installs the pinned llama.cpp build into a directory, unless
/// it is there already.
///
/// Inputs:
/// - `root`: the directory, such as [`default_root`].
///
/// Outputs: the path of its `llama-quantize`, or a [`QuantError`]: `NotInstalled` on a
/// machine llama.cpp publishes no build for here, or `Install` when the download fails.
pub fn install(
    root: &Path,
) -> Result<PathBuf, QuantError>
{
    if let Some(program) = installed(root) {
        return Ok(program);
    }
    let archive = archive_for(std::env::consts::OS, std::env::consts::ARCH).ok_or(QuantError::NotInstalled)?;
    let url = format!("https://github.com/ggml-org/llama.cpp/releases/download/{LLAMA_BUILD}/{}", archive.name);
    eprintln!("dovata: downloading llama.cpp {LLAMA_BUILD} for {}, which holds the {PROGRAM} program", archive.machine);
    let bytes = download(&url).map_err(QuantError::Install)?;
    unpack(&bytes, archive.sha256, LLAMA_BUILD, root).map_err(QuantError::Install)
}

/// Purpose: This function checks a llama.cpp archive's SHA-256 and unpacks it into a
/// directory of its own, then removes every other build there.
///
/// Inputs:
/// - `bytes`: the archive, a `.tar.gz` holding a `llama-<build>` directory.
/// - `sha256`: the SHA-256 it must have, in hex.
/// - `build`: the build, such as `b12110`.
/// - `root`: the directory the build goes in, as `<root>/<build>`.
///
/// Outputs: the path of the build's `llama-quantize`, or a message saying why nothing
/// was installed.
pub(crate) fn unpack(
    bytes: &[u8],
    sha256: &str,
    build: &str,
    root: &Path,
) -> Result<PathBuf, String>
{
    let got = sha256_hex(bytes);
    if got != sha256 {
        return Err(format!("the download has SHA-256 {got}, not {sha256}, so it is not the file llama.cpp published"));
    }
    std::fs::create_dir_all(root).map_err(|e| format!("cannot make {}: {e}", root.display()))?;
    let work = work_dir(root).map_err(|e| format!("cannot make a directory in {}: {e}", root.display()))?;
    let result = unpack_in(bytes, build, root, &work);
    let _ = std::fs::remove_dir_all(&work);
    let program = result?;
    remove_other_builds(root, build);
    Ok(program)
}

/// Purpose: This function unpacks a checked llama.cpp archive in a work directory and
/// moves its build into place.
///
/// Inputs:
/// - `bytes`: the archive.
/// - `build`: the build.
/// - `root`: the directory the build goes in.
/// - `work`: an empty directory inside `root`, removed afterwards by the caller of this
///   function, [`unpack`].
///
/// Outputs: the path of the build's `llama-quantize`, or a message saying why not.
fn unpack_in(
    bytes: &[u8],
    build: &str,
    root: &Path,
    work: &Path,
) -> Result<PathBuf, String>
{
    untar(bytes, work)?;
    let unpacked = work.join(format!("llama-{build}"));
    if !unpacked.join(PROGRAM).is_file() {
        return Err(format!("the archive has no llama-{build}/{PROGRAM}"));
    }
    // The program finds its libraries in its own directory, so the build stays whole.
    // A rename into place means no other dovata ever sees half of it.
    match std::fs::rename(&unpacked, root.join(build)) {
        Ok(()) => {}
        // Another dovata installed the same build first.
        Err(_) if installed_build(root, build).is_some() => {}
        Err(e) => return Err(format!("cannot move it into {}: {e}", root.join(build).display())),
    }
    installed_build(root, build).ok_or_else(|| format!("{PROGRAM} is not in {} after unpacking", root.join(build).display()))
}

/// Purpose: This function removes the builds an older dovata installed.
///
/// Inputs:
/// - `root`: the directory the builds are in.
/// - `keep`: the build to keep.
///
/// Outputs: none. A build that cannot be removed stays; it only takes disk space.
fn remove_other_builds(
    root: &Path,
    keep: &str,
)
{
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let is_build = name.len() > 1 && name.starts_with('b') && name[1..].bytes().all(|c| c.is_ascii_digit());
        if is_build && name != keep && entry.path().is_dir() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Purpose: This function downloads a file over HTTPS into memory.
///
/// Inputs:
/// - `url`: the file's address.
///
/// Outputs: the file's bytes, or a message saying why there are none. `dovata upgrade`
/// downloads dovata's releases with it too.
pub fn download(
    url: &str,
) -> Result<Vec<u8>, String>
{
    let agent = ureq::AgentBuilder::new().user_agent(concat!("dovata/", env!("CARGO_PKG_VERSION"))).build();
    let response = agent.get(url).call().map_err(|e| format!("cannot download {url}: {e}"))?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_DOWNLOAD + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot download {url}: {e}"))?;
    if bytes.len() as u64 > MAX_DOWNLOAD {
        return Err(format!("{url} is larger than the {} MiB dovata downloads", MAX_DOWNLOAD >> 20));
    }
    Ok(bytes)
}

/// Purpose: This function gives the SHA-256 of some bytes in hex.
///
/// Inputs:
/// - `bytes`: the bytes.
///
/// Outputs: 64 lowercase hex digits.
pub fn sha256_hex(
    bytes: &[u8],
) -> String
{
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Purpose: This function unpacks a `.tar.gz` archive with the system's `tar` program.
///
/// Inputs:
/// - `bytes`: the archive.
/// - `into`: the directory to unpack it in. The archive is written there first, as
///   `download.tar.gz`, and removed after.
///
/// Outputs: `Ok(())`, or a message saying why the archive did not unpack.
pub fn untar(
    bytes: &[u8],
    into: &Path,
) -> Result<(), String>
{
    let archive = into.join("download.tar.gz");
    std::fs::write(&archive, bytes).map_err(|e| format!("cannot write {}: {e}", archive.display()))?;
    let out = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(into)
        .output()
        .map_err(|e| format!("cannot run tar: {e}"))?;
    let _ = std::fs::remove_file(&archive);
    if !out.status.success() {
        return Err(format!("tar could not unpack the download: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

/// Purpose: This function makes a new, empty directory that only this user can open.
///
/// Inputs:
/// - `parent`: the directory to make it in.
///
/// Outputs: the new directory's path, or the error from making it.
pub fn work_dir(
    parent: &Path,
) -> std::io::Result<PathBuf>
{
    // A name nobody can guess, made with `create_dir`, which fails rather than accept
    // a directory that is already there, and private from the moment it exists. In a
    // shared directory such as /tmp, another user then cannot put files in it between
    // dovata checking a download and installing it.
    let mut last = None;
    for _ in 0..8 {
        let dir = parent.join(format!(".dovata-{}", random_hex()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no name was free")))
}

/// Purpose: This function gives 32 random hex digits for a file name.
///
/// Inputs: none; it reads `/dev/urandom`.
///
/// Outputs: the digits. Without `/dev/urandom` they come from the clock and the process
/// id, which [`work_dir`] tolerates, since a guessed name makes it fail, not misbehave.
fn random_hex() -> String
{
    let mut bytes = [0u8; 16];
    let read = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes));
    if read.is_err() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        bytes = (nanos ^ (u128::from(std::process::id()) << 64)).to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quantize::fixture::scratch_dir;

    /// Purpose: This function makes a `.tar.gz` holding one directory with some files.
    ///
    /// Inputs:
    /// - `dir`: a scratch directory to build it in.
    /// - `top`: the directory the archive holds, such as `llama-b1`.
    /// - `files`: the files in it.
    ///
    /// Outputs: the archive's bytes.
    fn archive(
        dir: &Path,
        top: &str,
        files: &[&str],
    ) -> Vec<u8>
    {
        let src = dir.join("src");
        std::fs::create_dir_all(src.join(top)).unwrap();
        for f in files {
            std::fs::write(src.join(top).join(f), b"#!/bin/sh\n").unwrap();
        }
        let out = dir.join("a.tar.gz");
        let ok = Command::new("tar").arg("-czf").arg(&out).arg("-C").arg(&src).arg(top).status().unwrap();
        assert!(ok.success());
        let _ = std::fs::remove_dir_all(&src);
        std::fs::read(out).unwrap()
    }

    #[test]
    fn each_released_machine_has_an_archive_of_the_pinned_build()
    {
        assert_eq!(archive_for("macos", "aarch64"), Some(MACOS_ARM64));
        assert_eq!(archive_for("linux", "x86_64"), Some(LINUX_X86_64));
        assert_eq!(archive_for("macos", "x86_64"), None, "dovata releases no Intel Mac programs");
        assert_eq!(archive_for("linux", "aarch64"), None);
        for a in [MACOS_ARM64, LINUX_X86_64] {
            assert!(a.name.starts_with(&format!("llama-{LLAMA_BUILD}-bin-")), "{}", a.name);
            assert_eq!(a.sha256.len(), 64);
            assert!(a.sha256.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        }
    }

    #[test]
    fn a_checked_archive_is_installed_as_its_build()
    {
        let dir = scratch_dir("install-checked");
        let bytes = archive(&dir, "llama-b1", &[PROGRAM, "libllama.0.dylib"]);
        let root = dir.join("llama.cpp");
        let program = unpack(&bytes, &sha256_hex(&bytes), "b1", &root).unwrap();
        assert_eq!(program, root.join("b1").join(PROGRAM));
        assert!(root.join("b1").join("libllama.0.dylib").is_file(), "the libraries stay beside the program");
        let left: Vec<_> = std::fs::read_dir(&root).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left, vec![std::ffi::OsString::from("b1")], "the work directory is removed");
        // A second install of the same build finds it there.
        assert_eq!(unpack(&bytes, &sha256_hex(&bytes), "b1", &root).unwrap(), program);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_archive_with_the_wrong_checksum_installs_nothing()
    {
        let dir = scratch_dir("install-wrong-sum");
        let bytes = archive(&dir, "llama-b1", &[PROGRAM]);
        let root = dir.join("llama.cpp");
        let err = unpack(&bytes, &"0".repeat(64), "b1", &root).unwrap_err();
        assert!(err.contains("not the file llama.cpp published"), "{err}");
        assert!(!root.exists(), "nothing is written before the checksum matches");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_archive_without_the_program_installs_nothing()
    {
        let dir = scratch_dir("install-no-program");
        let bytes = archive(&dir, "llama-b1", &["llama-cli"]);
        let root = dir.join("llama.cpp");
        let err = unpack(&bytes, &sha256_hex(&bytes), "b1", &root).unwrap_err();
        assert!(err.contains(&format!("no llama-b1/{PROGRAM}")), "{err}");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0, "neither the build nor the work directory stays");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_build_removes_older_builds_and_nothing_else()
    {
        let dir = scratch_dir("install-old-builds");
        let root = dir.join("llama.cpp");
        std::fs::create_dir_all(root.join("b0")).unwrap();
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::write(root.join("b7"), b"a file, not a build").unwrap();
        let bytes = archive(&dir, "llama-b1", &[PROGRAM]);
        unpack(&bytes, &sha256_hex(&bytes), "b1", &root).unwrap();
        assert!(!root.join("b0").exists());
        assert!(root.join("b1").is_dir());
        assert!(root.join("notes").is_dir());
        assert!(root.join("b7").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn work_directories_are_new_and_private()
    {
        let dir = scratch_dir("install-work-dir");
        let a = work_dir(&dir).unwrap();
        let b = work_dir(&dir).unwrap();
        assert_ne!(a, b);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&a).unwrap().permissions().mode() & 0o777, 0o700);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_sha256_is_lowercase_hex()
    {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
