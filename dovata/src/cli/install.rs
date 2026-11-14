//! Author: Joe Ricks
//! File: src/cli/install.rs
//! Description: The `dovata setup` and `upgrade` commands, which install the llama.cpp
//! build dovata quantizes with and the newest dovata release.

use std::path::Path;
use std::process::Command;

use dovata::host;
use dovata::quantize::install::{self, LLAMA_BUILD};

use super::{out, Fail};

/// The GitHub repo dovata is released from.
const REPO: &str = "joericks1998/jade";

/// The programs a dovata release holds, at the top of its archive.
const BINARIES: [&str; 2] = ["dovata", "gguf-inspect"];

/// The help text for `dovata upgrade`.
pub(crate) const UPGRADE_HELP: &str = "\
usage: dovata upgrade

Install the newest dovata release over this one: the dovata and gguf-inspect programs,
checked against the release's checksums.txt first, then the llama.cpp build the new
dovata quantizes with. It asks for sudo only when it cannot write to dovata's own
directory. A dovata process that is running keeps the old version until it restarts.
";

/// Purpose: This function gives the help text for `dovata setup`.
///
/// Inputs: none.
///
/// Outputs: the text, which names the llama.cpp build.
fn setup_help() -> String
{
    format!(
        "\
usage: dovata setup

Install llama.cpp's build {LLAMA_BUILD} into ~/.dovata/llama.cpp. dovata quantizes a model
by running its llama-quantize program. The download is llama.cpp's own release, checked
against its SHA-256 before anything is installed. The install script and `dovata
upgrade` run this for you, and dovata runs it itself the first time it quantizes.
"
    )
}

/// Purpose: This function says whether the arguments ask for help.
///
/// Inputs:
/// - `args`: the arguments after the command.
///
/// Outputs: `true` when one of them is `help`, `--help` or `-h`.
fn wants_help(
    args: &[String],
) -> bool
{
    args.iter().any(|a| matches!(a.as_str(), "help" | "--help" | "-h"))
}

/// Purpose: This function refuses a command run under sudo.
///
/// Inputs:
/// - `command`: the command's name, such as `upgrade`.
///
/// Outputs: `Ok(())`, or a [`Fail`] when sudo started this program.
fn refuse_sudo(
    command: &str,
) -> Result<(), Fail>
{
    // Under sudo, llama.cpp would go in root's home, or in this user's home owned by
    // root, and neither is what the user runs. sudo sets SUDO_UID.
    if std::env::var_os("SUDO_UID").is_some() {
        return Err(Fail::Usage(format!(
            "run `dovata {command}` as yourself, without sudo. llama.cpp goes in your own ~/.dovata, and \
             `dovata upgrade` asks for sudo itself when dovata's directory needs it"
        )));
    }
    Ok(())
}

/// Purpose: This function runs `dovata setup`.
///
/// Inputs:
/// - `args`: the arguments after `setup`; there should be none.
///
/// Outputs: `Ok(())` once llama.cpp is installed, or a [`Fail`].
pub(crate) fn setup(
    args: &[String],
) -> Result<(), Fail>
{
    if wants_help(args) {
        out(&setup_help());
        return Ok(());
    }
    if !args.is_empty() {
        return Err(Fail::Usage(format!("`dovata setup` takes no arguments\n\n{}", setup_help())));
    }
    refuse_sudo("setup")?;
    if install::archive_for(std::env::consts::OS, std::env::consts::ARCH).is_none() {
        return Err(Fail::Run(
            "llama.cpp publishes no build dovata can install on this machine, which is not an Apple Silicon \
             Mac or x86_64 Linux. dovata runs a llama-quantize on PATH instead; build it from \
             https://github.com/ggml-org/llama.cpp"
                .into(),
        ));
    }
    let root = install::default_root()
        .ok_or_else(|| Fail::Run("HOME is not set, so there is no ~/.dovata to install llama.cpp in".into()))?;
    let program = install::install(&root).map_err(|e| Fail::Run(e.to_string()))?;
    out(&format!("llama.cpp {LLAMA_BUILD} is installed; dovata quantizes with {}\n", program.display()));
    Ok(())
}

/// Purpose: This function runs `dovata upgrade`.
///
/// Inputs:
/// - `args`: the arguments after `upgrade`; there should be none.
///
/// Outputs: `Ok(())` once the newest release is installed, or when this is it; or a
/// [`Fail`].
pub(crate) fn upgrade(
    args: &[String],
) -> Result<(), Fail>
{
    if wants_help(args) {
        out(UPGRADE_HELP);
        return Ok(());
    }
    if !args.is_empty() {
        return Err(Fail::Usage(format!("`dovata upgrade` takes no arguments\n\n{UPGRADE_HELP}")));
    }
    refuse_sudo("upgrade")?;
    let archive = release_archive(std::env::consts::OS, std::env::consts::ARCH).ok_or_else(|| {
        Fail::Run(format!(
            "dovata releases programs for Apple Silicon Macs and x86_64 Linux only; build it from source: \
             https://github.com/{REPO}/tree/main/dovata"
        ))
    })?;
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| Fail::Run(format!("cannot find this dovata program's path: {e}")))?;
    let dir = exe.parent().ok_or_else(|| Fail::Run(format!("{} has no directory", exe.display())))?;
    if is_cargo_build(dir) {
        return Err(Fail::Run(format!(
            "this dovata is a cargo build, {}; pull the source and build it again instead",
            exe.display()
        )));
    }

    let current = env!("CARGO_PKG_VERSION");
    let releases = install::download(&format!("https://api.github.com/repos/{REPO}/releases?per_page=100"))
        .map_err(Fail::Run)?;
    let releases: serde_json::Value = serde_json::from_slice(&releases)
        .map_err(|e| Fail::Run(format!("GitHub's list of releases is not JSON: {e}")))?;
    let (tag, latest) = newest_release(&releases)
        .ok_or_else(|| Fail::Run(format!("found no dovata release in https://github.com/{REPO}/releases")))?;
    let latest_text = format!("{}.{}.{}", latest[0], latest[1], latest[2]);
    match parse_version(current).map(|c| latest.cmp(&c)) {
        Some(std::cmp::Ordering::Equal) => {
            out(&format!("dovata {current} is the newest release\n"));
            return setup(&[]);
        }
        Some(std::cmp::Ordering::Less) => {
            out(&format!(
                "this dovata, {current}, is newer than the newest release, {latest_text}, so there is nothing to \
                 upgrade to\n"
            ));
            return Ok(());
        }
        _ => {}
    }

    eprintln!("dovata: upgrading dovata {current} to {latest_text}");
    let base = format!("https://github.com/{REPO}/releases/download/{tag}");
    let sums = install::download(&format!("{base}/checksums.txt")).map_err(Fail::Run)?;
    let want = checksum_for(&String::from_utf8_lossy(&sums), archive)
        .ok_or_else(|| Fail::Run(format!("the {tag} release's checksums.txt has no line for {archive}")))?;
    let bytes = install::download(&format!("{base}/{archive}")).map_err(Fail::Run)?;
    let got = install::sha256_hex(&bytes);
    if got != want {
        return Err(Fail::Run(format!(
            "{archive} has SHA-256 {got}, but the release's checksums.txt says {want}; nothing was installed"
        )));
    }

    let work = install::work_dir(&std::env::temp_dir())
        .map_err(|e| Fail::Run(format!("cannot make a directory to unpack the download in: {e}")))?;
    let result = unpack_and_replace(&bytes, &work, dir, &tag);
    let _ = std::fs::remove_dir_all(&work);
    result?;
    out(&format!("dovata {latest_text} is installed in {}\n", dir.display()));

    // The new program installs the llama.cpp build it was released with.
    let set_up = Command::new(dir.join("dovata")).arg("setup").status();
    if !set_up.is_ok_and(|s| s.success()) {
        eprintln!("dovata: llama.cpp is not installed yet; dovata installs it the first time it quantizes");
    }

    match host::connect().and_then(|p| p.status()) {
        Ok(status) if status.version != latest_text => out(&format!(
            "\nThe dovata process, pid {}, still runs {}. `dovata kill` and `dovata start` restart it on {latest_text}; \
             then add its models again.\n",
            status.pid, status.version
        )),
        // A process this program cannot read is another dovata version, and the error
        // says how to stop it.
        Err(host::HostError::Record(why)) => out(&format!("\n{why}\n")),
        _ => {}
    }
    Ok(())
}

/// Purpose: This function unpacks a checked release archive and puts its programs in
/// place of the running ones.
///
/// Inputs:
/// - `bytes`: the archive.
/// - `work`: a private, empty directory to unpack it in.
/// - `dir`: the directory the running dovata is in.
/// - `tag`: the release's tag, for messages.
///
/// Outputs: `Ok(())` once both programs are replaced, or a [`Fail`].
fn unpack_and_replace(
    bytes: &[u8],
    work: &Path,
    dir: &Path,
    tag: &str,
) -> Result<(), Fail>
{
    install::untar(bytes, work).map_err(Fail::Run)?;
    for bin in BINARIES {
        if !work.join(bin).is_file() {
            return Err(Fail::Run(format!("the {tag} release has no {bin}; nothing was installed")));
        }
    }
    let writable = can_write(dir);
    if !writable {
        eprintln!("dovata: you cannot write to {}, so dovata uses sudo to replace its programs there", dir.display());
    }
    for bin in BINARIES {
        replace(&work.join(bin), &dir.join(bin), writable)?;
    }
    Ok(())
}

/// Purpose: This function puts a new program in place of an old one, so that whoever
/// starts it sees either the old file or the new one, never half of one.
///
/// Inputs:
/// - `new`: the new program.
/// - `dest`: where it goes; a file there is replaced.
/// - `writable`: whether this user can write to `dest`'s directory. When not, the copy
///   and the rename run under sudo.
///
/// Outputs: `Ok(())`, or a [`Fail`].
fn replace(
    new: &Path,
    dest: &Path,
    writable: bool,
) -> Result<(), Fail>
{
    let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("dovata");
    let staged = dest.with_file_name(format!(".{name}-upgrade-{}", std::process::id()));
    let failed = |what: String| Fail::Run(format!("cannot install {}: {what}", dest.display()));
    if writable {
        use std::os::unix::fs::PermissionsExt;
        let staged_ok = std::fs::copy(new, &staged)
            .and_then(|_| std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)))
            .and_then(|()| std::fs::rename(&staged, dest));
        if let Err(e) = staged_ok {
            let _ = std::fs::remove_file(&staged);
            return Err(failed(e.to_string()));
        }
        return Ok(());
    }
    sudo(&["install".as_ref(), "-m".as_ref(), "755".as_ref(), new.as_os_str(), staged.as_os_str()]).map_err(failed)?;
    sudo(&["mv".as_ref(), "-f".as_ref(), staged.as_os_str(), dest.as_os_str()]).map_err(failed)
}

/// Purpose: This function runs one command under sudo, which asks for a password on
/// the terminal when it needs one.
///
/// Inputs:
/// - `args`: the command and its arguments.
///
/// Outputs: `Ok(())` when it succeeds, or a message saying why not.
fn sudo(
    args: &[&std::ffi::OsStr],
) -> Result<(), String>
{
    let status = Command::new("sudo").args(args).status().map_err(|e| format!("cannot run sudo: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("sudo {} failed with {status}", args[0].to_string_lossy()))
    }
}

/// Purpose: This function checks whether this user can make files in a directory.
///
/// Inputs:
/// - `dir`: the directory.
///
/// Outputs: `true` when a file could be made there; it is removed again.
fn can_write(
    dir: &Path,
) -> bool
{
    let probe = dir.join(format!(".dovata-upgrade-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Purpose: This function names the dovata release archive for a machine.
///
/// Inputs:
/// - `os`: the operating system, as `std::env::consts::OS` names it.
/// - `arch`: the processor, as `std::env::consts::ARCH` names it.
///
/// Outputs: the archive's name, the one `dovata-release.yml` publishes, or `None` for a
/// machine dovata releases no programs for.
fn release_archive(
    os: &str,
    arch: &str,
) -> Option<&'static str>
{
    match (os, arch) {
        ("macos", "aarch64") => Some("dovata-macos-arm64.tar.gz"),
        ("linux", "x86_64") => Some("dovata-linux-x86_64.tar.gz"),
        _ => None,
    }
}

/// Purpose: This function reads a version such as `3.2.1`.
///
/// Inputs:
/// - `text`: the version.
///
/// Outputs: its three numbers, or `None` when it is not three numbers joined by dots.
fn parse_version(
    text: &str,
) -> Option<[u64; 3]>
{
    let mut parts = text.split('.').map(|p| p.parse::<u64>().ok());
    let version = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some(version)
}

/// Purpose: This function finds the newest dovata release in GitHub's list of the
/// repo's releases.
///
/// Inputs:
/// - `releases`: the list, as `GET /repos/{repo}/releases` answers it.
///
/// Outputs: the release's tag and version, or `None` when there is no dovata release.
/// Drafts, prereleases and the compiler's releases are skipped.
fn newest_release(
    releases: &serde_json::Value,
) -> Option<(String, [u64; 3])>
{
    releases
        .as_array()?
        .iter()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false) && !r["prerelease"].as_bool().unwrap_or(false))
        .filter_map(|r| {
            let tag = r["tag_name"].as_str()?;
            Some((tag.to_owned(), parse_version(tag.strip_prefix("dovata-v")?)?))
        })
        .max_by_key(|(_, version)| *version)
}

/// Purpose: This function finds a file's SHA-256 in a `checksums.txt`.
///
/// Inputs:
/// - `sums`: the file's text, one `<sha256>  <name>` per line, as `sha256sum` writes it.
/// - `name`: the file to find.
///
/// Outputs: the SHA-256 in lowercase hex, or `None` when no line names the file.
fn checksum_for(
    sums: &str,
    name: &str,
) -> Option<String>
{
    sums.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let (sum, file) = (fields.next()?, fields.next()?);
        (file.trim_start_matches('*') == name && sum.len() == 64).then(|| sum.to_ascii_lowercase())
    })
}

/// Purpose: This function checks whether a directory is a cargo build's output, such
/// as `target/debug`.
///
/// Inputs:
/// - `dir`: the directory the running program is in.
///
/// Outputs: `true` for `target/debug`, `target/release`, and the same under a target
/// triple, such as `target/aarch64-apple-darwin/release`.
fn is_cargo_build(
    dir: &Path,
) -> bool
{
    let profile = dir.file_name().and_then(|n| n.to_str());
    let in_target = dir.ancestors().skip(1).take(2).any(|a| a.file_name().is_some_and(|n| n == "target"));
    matches!(profile, Some("debug" | "release")) && in_target
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_released_machine_has_an_archive()
    {
        assert_eq!(release_archive("macos", "aarch64"), Some("dovata-macos-arm64.tar.gz"));
        assert_eq!(release_archive("linux", "x86_64"), Some("dovata-linux-x86_64.tar.gz"));
        assert_eq!(release_archive("macos", "x86_64"), None);
        assert_eq!(release_archive("windows", "x86_64"), None);
    }

    #[test]
    fn versions_are_three_numbers()
    {
        assert_eq!(parse_version("3.2.1"), Some([3, 2, 1]));
        assert_eq!(parse_version("3.10.0"), Some([3, 10, 0]));
        for bad in ["", "3", "3.2", "3.2.1.4", "3.2.x", "v3.2.1"] {
            assert_eq!(parse_version(bad), None, "{bad:?}");
        }
        assert!(parse_version("3.10.0") > parse_version("3.9.9"), "numbers, not text");
    }

    #[test]
    fn the_newest_dovata_release_skips_the_compiler_drafts_and_prereleases()
    {
        let releases = serde_json::json!([
            {"tag_name": "v2.0.2", "draft": false, "prerelease": false},
            {"tag_name": "dovata-v3.3.0", "draft": true, "prerelease": false},
            {"tag_name": "dovata-v3.2.2", "draft": false, "prerelease": true},
            {"tag_name": "dovata-v3.2.1", "draft": false, "prerelease": false},
            {"tag_name": "dovata-v3.10.0", "draft": false, "prerelease": false},
            {"tag_name": "dovata-v3.2.0", "draft": false, "prerelease": false},
            {"tag_name": "dovata-vnext", "draft": false, "prerelease": false},
        ]);
        assert_eq!(newest_release(&releases), Some(("dovata-v3.10.0".to_owned(), [3, 10, 0])));
        assert_eq!(newest_release(&serde_json::json!([{"tag_name": "v2.0.1"}])), None);
        assert_eq!(newest_release(&serde_json::json!({"message": "rate limited"})), None);
    }

    #[test]
    fn checksums_are_found_by_file_name()
    {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums = format!("{a}  dovata-linux-x86_64.tar.gz\n{b} *dovata-macos-arm64.tar.gz\n");
        assert_eq!(checksum_for(&sums, "dovata-linux-x86_64.tar.gz"), Some(a));
        assert_eq!(checksum_for(&sums, "dovata-macos-arm64.tar.gz"), Some("b".repeat(64)));
        assert_eq!(checksum_for(&sums, "dovata-linux"), None, "a whole name, not a prefix");
        assert_eq!(checksum_for("short  dovata-linux-x86_64.tar.gz", "dovata-linux-x86_64.tar.gz"), None);
    }

    #[test]
    fn a_cargo_build_is_not_upgraded_in_place()
    {
        assert!(is_cargo_build(Path::new("/src/jade/target/debug")));
        assert!(is_cargo_build(Path::new("/src/jade/target/release")));
        assert!(is_cargo_build(Path::new("/src/jade/target/aarch64-apple-darwin/release")));
        assert!(!is_cargo_build(Path::new("/usr/local/bin")));
        assert!(!is_cargo_build(Path::new("/opt/release")));
        assert!(!is_cargo_build(Path::new("/home/me/.cargo/bin")));
    }

    #[test]
    fn a_writable_directory_gets_the_new_program_in_place()
    {
        let dir = std::env::temp_dir().join(format!("dovata-upgrade-replace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("new"), b"new").unwrap();
        std::fs::write(dir.join("dovata"), b"old").unwrap();
        assert!(can_write(&dir));
        replace(&dir.join("new"), &dir.join("dovata"), true).unwrap_or_else(|_| panic!("replace failed"));
        assert_eq!(std::fs::read(dir.join("dovata")).unwrap(), b"new");
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.join("dovata")).unwrap().permissions().mode() & 0o777, 0o755);
        }
        let mut left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        left.sort();
        assert_eq!(left, vec![std::ffi::OsString::from("dovata"), "new".into()], "no staged file is left");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
