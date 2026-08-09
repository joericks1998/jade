const GITHUB_REPO: &str = "joericks1998/jade";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

use std::path::{Path, PathBuf};

#[derive(serde::Deserialize)]
struct GhRelease {
    tag_name: String,
    assets: Vec<GhAsset>,
}

#[derive(serde::Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

/// The release archive label for this platform. This matches the names
/// `release.yml` actually publishes (`jade-macos-arm64.tar.gz`,
/// `jade-linux-x86_64.tar.gz`) — which are NOT the `pkg::fetch::platform_tag`
/// values (`darwin-aarch64`/`linux-x86_64`); only these two are built.
pub(crate) fn archive_label() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("macos-arm64"),
        ("linux", "x86_64") => Some("linux-x86_64"),
        _ => None,
    }
}

/// Where the toolchain lives: the real binary, and the `lib/jade` tree beside
/// it that `install.sh` lays down.
///
/// Resolved through any symlink, because replacing or removing a symlink leaves
/// the file it pointed at behind — which on a `brew`-style layout is the whole
/// installation.
pub(crate) struct Layout {
    pub bin: PathBuf,
    pub lib: Option<PathBuf>,
}

pub(crate) fn layout() -> Result<Layout, String> {
    let bin = std::env::current_exe()
        .map(|p| p.canonicalize().unwrap_or(p))
        .map_err(|e| format!("could not determine the jade binary's path: {e}"))?;
    let lib = bin
        .parent()
        .and_then(|d| d.parent())
        .map(|prefix| prefix.join("lib").join("jade"))
        .filter(|p| p.is_dir());
    Ok(Layout { bin, lib })
}

/// The per-user data directory: cache, config, credentials, installed
/// providers. Deliberately separate from the toolchain — reinstalling should
/// not cost you your API key.
pub(crate) fn user_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".jade")).filter(|p| p.exists())
}

/// Ask before doing something irreversible. `yes` skips it; a non-interactive
/// stdin refuses rather than assuming consent, since a script that did not pass
/// `--yes` did not ask for this.
fn confirm(prompt: &str, yes: bool) -> bool {
    use std::io::{IsTerminal, Write};
    if yes {
        return true;
    }
    if !std::io::stdin().is_terminal() {
        eprintln!("refusing to proceed without a terminal to confirm at — pass --yes");
        return false;
    }
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// `jade uninstall [--purge] [--yes]` — remove the toolchain.
///
/// Keeps `~/.jade` unless `--purge`, because it holds credentials and installed
/// providers and losing those to a reinstall would be a nasty surprise.
pub fn run_uninstall(purge: bool, yes: bool) {
    let layout = match layout() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("uninstall: {e}");
            std::process::exit(1);
        }
    };

    // Everything is listed before anything is touched: this is irreversible,
    // and the paths are the only way to tell a real installation from a
    // `cargo build` tree you did not mean to delete.
    let mut targets: Vec<PathBuf> = vec![layout.bin.clone()];
    if let Some(lib) = &layout.lib {
        targets.push(lib.clone());
    }
    let data = user_dir();
    if purge && let Some(d) = &data {
        targets.push(d.clone());
    }

    println!("this will remove:");
    for t in &targets {
        println!("  {}", t.display());
    }
    if !purge && let Some(d) = &data {
        println!("\nkeeping {} (credentials, providers, cache)", d.display());
        println!("  pass --purge to remove that too");
    } else if purge {
        println!("\nincluding your stored API keys and installed providers");
    }

    if !confirm("\nremove them?", yes) {
        println!("nothing was removed");
        return;
    }

    let mut failed = false;
    for t in &targets {
        let r = if t.is_dir() { std::fs::remove_dir_all(t) } else { std::fs::remove_file(t) };
        match r {
            Ok(()) => println!("removed {}", t.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                // A permission error here is the common case for a
                // system-wide install, so name the fix rather than the errno.
                eprintln!("could not remove {}: {e}", t.display());
                failed = true;
            }
        }
    }

    if failed {
        eprintln!(
            "\nsome paths could not be removed — re-run with sudo if jade is installed system-wide"
        );
        std::process::exit(1);
    }
    println!("\njade is uninstalled. Reinstall any time with:");
    println!("  curl -fsSL https://raw.githubusercontent.com/{GITHUB_REPO}/main/install.sh | sh");
}

pub async fn run_upgrade() {
    upgrade_or_reinstall(false, false).await
}

/// `jade reinstall [--clean] [--yes]` — fetch and install the latest release
/// even when it is the version already running.
///
/// `upgrade` stops when it is already current, which is right for an upgrade
/// and useless when the reason you are here is that something is broken.
pub async fn run_reinstall(clean: bool, yes: bool) {
    if clean {
        let Some(dir) = user_dir() else {
            eprintln!("reinstall: no ~/.jade to clean");
            std::process::exit(1);
        };
        println!("this will remove {} (credentials, providers, cache)", dir.display());
        if !confirm("remove it?", yes) {
            println!("nothing was removed");
            return;
        }
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            eprintln!("reinstall: could not remove {}: {e}", dir.display());
            std::process::exit(1);
        }
        println!("removed {}", dir.display());
        println!("note: re-register your provider afterwards with `jade register`");
    }
    upgrade_or_reinstall(true, clean).await
}

async fn upgrade_or_reinstall(force: bool, _cleaned: bool) {
    let label = match archive_label() {
        Some(l) => l,
        None => {
            eprintln!(
                "upgrade: no prebuilt binary for {}/{} — build from source",
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            std::process::exit(1);
        }
    };

    println!("checking for updates...");

    let client = reqwest::Client::builder()
        .user_agent(format!("jade/{CURRENT_VERSION}"))
        .build()
        .expect("http client");

    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("upgrade: could not reach GitHub: {e}");
            std::process::exit(1);
        }
    };

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        println!("no releases published yet");
        return;
    }
    if !resp.status().is_success() {
        eprintln!("upgrade: GitHub API returned {}", resp.status());
        std::process::exit(1);
    }

    let release: GhRelease = match resp.json().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("upgrade: could not parse GitHub response: {e}");
            std::process::exit(1);
        }
    };

    let latest = release.tag_name.trim_start_matches('v');
    if latest == CURRENT_VERSION && !force {
        println!("jade {CURRENT_VERSION} is already up to date");
        return;
    }

    let asset_name = format!("jade-{label}.tar.gz");
    let asset = match release.assets.iter().find(|a| a.name == asset_name) {
        Some(a) => a,
        None => {
            eprintln!(
                "upgrade: no binary for {label} in release {} (asset '{asset_name}' not found)",
                release.tag_name
            );
            std::process::exit(1);
        }
    };

    if force && latest == CURRENT_VERSION {
        println!("reinstalling jade {latest} ...");
    } else {
        println!("upgrading jade {CURRENT_VERSION} → {latest} ...");
    }

    // Resolve the real binary (through any symlink) so we replace the file, and
    // derive the toolchain layout `<prefix>/bin/jade` + `<prefix>/lib/jade`
    // that install.sh lays down.
    let current_exe = match std::env::current_exe() {
        Ok(p) => p.canonicalize().unwrap_or(p),
        Err(e) => {
            eprintln!("upgrade: could not determine current binary path: {e}");
            std::process::exit(1);
        }
    };
    let exe_dir = current_exe.parent().unwrap_or_else(|| Path::new("/usr/local/bin"));
    let lib_dir = exe_dir.parent().map(|prefix| prefix.join("lib").join("jade"));

    // Download the tarball and extract it into a scratch dir.
    let bytes = match client.get(&asset.browser_download_url).send().await {
        Ok(r) => match r.bytes().await {
            Ok(b) => b,
            Err(e) => {
                eprintln!("upgrade: download failed: {e}");
                std::process::exit(1);
            }
        },
        Err(e) => {
            eprintln!("upgrade: download failed: {e}");
            std::process::exit(1);
        }
    };

    let work = std::env::temp_dir().join(format!("jade-upgrade-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    if let Err(e) = std::fs::create_dir_all(&work) {
        eprintln!("upgrade: could not create temp dir: {e}");
        std::process::exit(1);
    }
    let tarball = work.join("jade.tar.gz");
    if let Err(e) = std::fs::write(&tarball, &bytes) {
        cleanup(&work);
        eprintln!("upgrade: could not write download: {e}");
        std::process::exit(1);
    }

    let status =
        std::process::Command::new("tar").arg("-xzf").arg(&tarball).arg("-C").arg(&work).status();
    match status {
        Ok(s) if s.success() => {}
        _ => {
            cleanup(&work);
            eprintln!("upgrade: could not extract {asset_name}");
            std::process::exit(1);
        }
    }

    let new_bin = work.join("jade");
    if !new_bin.exists() {
        cleanup(&work);
        eprintln!("upgrade: archive did not contain a jade binary");
        std::process::exit(1);
    }

    // Replace the binary atomically (temp file beside it → rename), then refresh
    // the runtime archives + bundled providers `jade build` needs.
    install_binary(&new_bin, &current_exe, &work);
    if let Some(lib_dir) = &lib_dir {
        let src_lib = work.join("lib");
        if src_lib.is_dir() {
            install_tree(&src_lib, lib_dir, &work);
        }
    }

    cleanup(&work);
    println!("jade {latest} installed at {}", current_exe.display());
}

fn cleanup(work: &Path) {
    let _ = std::fs::remove_dir_all(work);
}

/// Fail with the standard "not writable" guidance and exit.
fn fail_perm(work: &Path, what: &str, e: &std::io::Error) -> ! {
    cleanup(work);
    eprintln!("upgrade: could not {what}: {e}");
    eprintln!("         the install dir may need elevated permissions — try: sudo jade upgrade");
    std::process::exit(1);
}

/// Atomically replace `dest` with `new_bin`: copy to a sibling temp file (same
/// filesystem, so the rename is atomic), mark it executable, then rename over.
fn install_binary(new_bin: &Path, dest: &Path, work: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let dir = dest.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(".jade-upgrade-{}", std::process::id()));
    if let Err(e) = std::fs::copy(new_bin, &tmp) {
        fail_perm(work, &format!("write {}", tmp.display()), &e);
    }
    if let Err(e) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)) {
        let _ = std::fs::remove_file(&tmp);
        fail_perm(work, "set permissions", &e);
    }
    if let Err(e) = std::fs::rename(&tmp, dest) {
        let _ = std::fs::remove_file(&tmp);
        fail_perm(work, "replace the binary", &e);
    }
}

/// Mirror the extracted `lib/` tree into `<prefix>/lib/jade/` (runtime archives
/// and bundled providers), overwriting in place.
fn install_tree(src: &Path, dst: &Path, work: &Path) {
    let entries = match std::fs::read_dir(src) {
        Ok(e) => e,
        Err(_) => return,
    };
    if let Err(e) = std::fs::create_dir_all(dst) {
        fail_perm(work, &format!("create {}", dst.display()), &e);
    }
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name() else { continue };
        let target = dst.join(name);
        if path.is_dir() {
            install_tree(&path, &target, work);
        } else if let Err(e) = std::fs::copy(&path, &target) {
            fail_perm(work, &format!("write {}", target.display()), &e);
        }
    }
}
