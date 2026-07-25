use super::*;
use jade_runtime::provider::LIB_EXT;
use std::sync::Mutex;

// These mutate process-global env (`HOME`, `JADE_*`); serialize them so parallel
// test threads don't stomp each other. Env mutation is `unsafe` in edition 2024
// (not thread-safe); the lock is what makes it sound, confined to these helpers.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn set_env(key: &str, val: &str) {
    unsafe { std::env::set_var(key, val) };
}
fn unset_env(key: &str) {
    unsafe { std::env::remove_var(key) };
}
fn restore_env(key: &str, prev: Option<String>) {
    match prev {
        Some(v) => set_env(key, &v),
        None => unset_env(key),
    }
}

struct TmpDir(PathBuf);
impl TmpDir {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("jade-reg-{}-{}", std::process::id(), label));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TmpDir(dir)
    }
}
impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn touch(path: &Path) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, b"lib").unwrap();
}

/// Set up an isolated HOME plus a source directory of installable providers.
/// Returns (home, providers_source) as live temp dirs (removed on drop).
fn isolate(label: &str, provider_names: &[&str]) -> (TmpDir, TmpDir) {
    let home = TmpDir::new(&format!("{label}-home"));
    let src = TmpDir::new(&format!("{label}-src"));
    for name in provider_names {
        touch(&src.0.join(format!("{name}.{LIB_EXT}")));
    }
    set_env("HOME", home.0.to_str().unwrap());
    set_env(ENV_PROVIDERS_DIR, src.0.to_str().unwrap());
    unset_env("JADE_PROVIDER_ACTIVE"); // derive the slot from HOME
    (home, src)
}

fn deisolate(prev_home: Option<String>, prev_src: Option<String>) {
    restore_env(ENV_PROVIDERS_DIR, prev_src);
    restore_env("HOME", prev_home);
    unset_env("JADE_PROVIDER_ACTIVE");
}

// ── credential envelope ───────────────────────────────────────────────────────

#[test]
fn envelope_escapes_metacharacters() {
    let out = String::from_utf8(envelope("a\"b\\c\nd")).unwrap();
    assert!(!out.contains('\n'));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["api_key"], "a\"b\\c\nd");
}

#[test]
fn credential_env_var_is_uppercase_suffixed() {
    assert_eq!(credential_env_var("anthropic"), "ANTHROPIC_API_KEY");
    assert_eq!(credential_env_var("openai"), "OPENAI_API_KEY");
}

// ── discovery ─────────────────────────────────────────────────────────────────

#[test]
fn installed_finds_libs_and_ignores_others() {
    let _guard = ENV_LOCK.lock().unwrap();
    let (prev_home, prev_src) = (std::env::var("HOME").ok(), std::env::var(ENV_PROVIDERS_DIR).ok());
    let (_home, src) = isolate("disco", &["anthropic", "openai"]);
    touch(&src.0.join("README.md")); // wrong extension → ignored

    let names: Vec<String> = installed().into_iter().map(|p| p.name).collect();
    assert!(names.contains(&"anthropic".to_string()));
    assert!(names.contains(&"openai".to_string()));
    assert!(!names.iter().any(|n| n == "README"));

    deisolate(prev_home, prev_src);
}

// ── activation ────────────────────────────────────────────────────────────────

#[test]
fn activate_installs_into_pool_and_slot_with_credential() {
    let _guard = ENV_LOCK.lock().unwrap();
    let (prev_home, prev_src) = (std::env::var("HOME").ok(), std::env::var(ENV_PROVIDERS_DIR).ok());
    let (_home, _src) = isolate("act", &["anthropic"]);

    assert!(active_provider().is_none());
    store_credential("anthropic", "sk-1").unwrap();
    activate("anthropic").unwrap();

    assert_eq!(active_provider().as_deref(), Some("anthropic"));
    // Landed in the pool…
    assert!(pool_dir().join(format!("anthropic.{LIB_EXT}")).exists());
    // …and in the active slot, with the materialized credential.
    assert!(active_dir().join(format!("anthropic.{LIB_EXT}")).exists());
    assert_eq!(
        std::fs::read(active_dir().join("config.json")).unwrap(),
        envelope("sk-1")
    );

    deisolate(prev_home, prev_src);
}

#[test]
fn switching_leaves_exactly_one_active() {
    let _guard = ENV_LOCK.lock().unwrap();
    let (prev_home, prev_src) = (std::env::var("HOME").ok(), std::env::var(ENV_PROVIDERS_DIR).ok());
    let (_home, _src) = isolate("switch", &["anthropic", "openai"]);

    activate("anthropic").unwrap();
    activate("openai").unwrap();

    assert_eq!(active_provider().as_deref(), Some("openai"));
    let libs: Vec<String> = std::fs::read_dir(active_dir())
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some(LIB_EXT))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(libs, vec![format!("openai.{LIB_EXT}")]);

    deisolate(prev_home, prev_src);
}

#[test]
fn env_only_key_is_not_written_to_the_slot() {
    let _guard = ENV_LOCK.lock().unwrap();
    let (prev_home, prev_src) = (std::env::var("HOME").ok(), std::env::var(ENV_PROVIDERS_DIR).ok());
    let var = credential_env_var("anthropic");
    let prev_var = std::env::var(&var).ok();
    let (_home, _src) = isolate("envonly", &["anthropic"]);
    set_env(&var, "sk-env"); // key in env, never stored

    activate("anthropic").unwrap();

    // Active, but no credential written to disk — the provider reads its env var.
    assert_eq!(active_provider().as_deref(), Some("anthropic"));
    assert!(!active_dir().join("config.json").exists());

    restore_env(&var, prev_var);
    deisolate(prev_home, prev_src);
}

#[test]
fn activate_unknown_provider_errors() {
    let _guard = ENV_LOCK.lock().unwrap();
    let (prev_home, prev_src) = (std::env::var("HOME").ok(), std::env::var(ENV_PROVIDERS_DIR).ok());
    let (_home, _src) = isolate("unknown", &["anthropic"]);

    assert!(activate("openai").is_err());

    deisolate(prev_home, prev_src);
}

#[test]
fn storing_key_for_active_provider_refreshes_the_slot() {
    let _guard = ENV_LOCK.lock().unwrap();
    let (prev_home, prev_src) = (std::env::var("HOME").ok(), std::env::var(ENV_PROVIDERS_DIR).ok());
    let (_home, _src) = isolate("refresh", &["anthropic"]);

    store_credential("anthropic", "sk-old").unwrap();
    activate("anthropic").unwrap();
    assert_eq!(std::fs::read(active_dir().join("config.json")).unwrap(), envelope("sk-old"));

    // Updating the active provider's key propagates to the slot immediately.
    store_credential("anthropic", "sk-new").unwrap();
    assert_eq!(std::fs::read(active_dir().join("config.json")).unwrap(), envelope("sk-new"));

    deisolate(prev_home, prev_src);
}

// ── credentials ───────────────────────────────────────────────────────────────

#[cfg(unix)]
#[test]
fn stored_credential_is_owner_only_even_over_loose_file() {
    use std::os::unix::fs::PermissionsExt;
    let _guard = ENV_LOCK.lock().unwrap();
    let (prev_home, prev_src) = (std::env::var("HOME").ok(), std::env::var(ENV_PROVIDERS_DIR).ok());
    let (_home, _src) = isolate("perm", &[]);

    let path = credential_path("anthropic");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"stale").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    store_credential("anthropic", "sk-secret").unwrap();

    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::read(&path).unwrap(), envelope("sk-secret"));

    deisolate(prev_home, prev_src);
}

#[test]
fn has_credential_and_remove() {
    let _guard = ENV_LOCK.lock().unwrap();
    let (prev_home, prev_src) = (std::env::var("HOME").ok(), std::env::var(ENV_PROVIDERS_DIR).ok());
    let var = credential_env_var("anthropic");
    let prev_var = std::env::var(&var).ok();
    let (_home, _src) = isolate("cred", &[]);
    unset_env(&var);

    assert!(!has_credential("anthropic"));
    store_credential("anthropic", "sk-1").unwrap();
    assert!(has_credential("anthropic"));
    remove_credential("anthropic").unwrap();
    assert!(!has_credential("anthropic"));

    set_env(&var, "sk-env");
    assert!(has_credential("anthropic")); // env alone is enough

    restore_env(&var, prev_var);
    deisolate(prev_home, prev_src);
}
