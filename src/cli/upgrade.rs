use std::os::unix::fs::PermissionsExt;

const GITHUB_REPO: &str = "joericks1998/jade-os";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Platform tag embedded in release asset names, e.g. `jade-darwin-aarch64`.
fn platform_tag() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos",   "aarch64") => Some("darwin-aarch64"),
        ("macos",   "x86_64")  => Some("darwin-x86_64"),
        ("linux",   "x86_64")  => Some("linux-x86_64"),
        ("linux",   "aarch64") => Some("linux-aarch64"),
        _ => None,
    }
}

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

pub async fn run_upgrade() {
    let platform = match platform_tag() {
        Some(p) => p,
        None => {
            eprintln!(
                "upgrade: unsupported platform ({}/{})",
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            std::process::exit(1);
        }
    };

    println!("checking for updates...");

    let client = reqwest::Client::builder()
        .user_agent(format!("jade/{}", CURRENT_VERSION))
        .build()
        .expect("http client");

    let url = format!(
        "https://api.github.com/repos/{}/releases/latest",
        GITHUB_REPO
    );

    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("upgrade: could not reach GitHub: {}", e);
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
            eprintln!("upgrade: could not parse GitHub response: {}", e);
            std::process::exit(1);
        }
    };

    // Strip leading 'v' from tag so we can compare to semver in Cargo.toml.
    let latest = release.tag_name.trim_start_matches('v');

    if latest == CURRENT_VERSION {
        println!("jade {} is already up to date", CURRENT_VERSION);
        return;
    }

    // Find the asset for this platform.
    let asset_name = format!("jade-{}", platform);
    let asset = release.assets.iter().find(|a| a.name == asset_name);
    let asset = match asset {
        Some(a) => a,
        None => {
            eprintln!(
                "upgrade: no binary for {} in release {} (asset '{}' not found)",
                platform, release.tag_name, asset_name
            );
            std::process::exit(1);
        }
    };

    println!(
        "upgrading jade {} → {} ...",
        CURRENT_VERSION, latest
    );

    // Download into a temp file next to the current exe so the rename is atomic.
    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("upgrade: could not determine current binary path: {}", e);
            std::process::exit(1);
        }
    };

    // Resolve symlinks so we replace the real file, not just the link.
    let current_exe = current_exe.canonicalize().unwrap_or(current_exe);
    let exe_dir = current_exe.parent().unwrap_or(std::path::Path::new("/usr/local/bin"));
    let tmp_path = exe_dir.join(format!(".jade-upgrade-{}", std::process::id()));

    let bytes = match client.get(&asset.browser_download_url).send().await {
        Ok(r) => match r.bytes().await {
            Ok(b) => b,
            Err(e) => { eprintln!("upgrade: download failed: {}", e); std::process::exit(1); }
        },
        Err(e) => { eprintln!("upgrade: download failed: {}", e); std::process::exit(1); }
    };

    if let Err(e) = std::fs::write(&tmp_path, &bytes) {
        eprintln!("upgrade: could not write to {}: {}", tmp_path.display(), e);
        eprintln!("         try: sudo jade upgrade");
        std::process::exit(1);
    }

    // Make executable.
    if let Err(e) = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755)) {
        let _ = std::fs::remove_file(&tmp_path);
        eprintln!("upgrade: could not set permissions: {}", e);
        std::process::exit(1);
    }

    // Atomic rename — replaces the running binary.
    if let Err(e) = std::fs::rename(&tmp_path, &current_exe) {
        let _ = std::fs::remove_file(&tmp_path);
        eprintln!("upgrade: could not replace binary: {}", e);
        eprintln!("         try: sudo jade upgrade");
        std::process::exit(1);
    }

    println!("jade {} installed at {}", latest, current_exe.display());
}
