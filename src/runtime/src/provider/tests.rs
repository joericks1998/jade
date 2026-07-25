//! Tests for active-slot resolution. Loading/driving a real provider `.so` is an
//! end-to-end concern (the VM via `crate::native`, AOT via the C runtime).

use super::*;
use std::path::PathBuf;
use std::sync::Mutex;

// These mutate `HOME`/`JADE_PROVIDER_ACTIVE`; serialize them (env mutation is
// process-global and `unsafe` in edition 2024).
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
        let dir = std::env::temp_dir().join(format!("jade-slot-{}-{}", std::process::id(), label));
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

#[test]
fn active_slot_resolves_the_single_so_and_config() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = TmpDir::new("active");
    let prev = std::env::var("JADE_PROVIDER_ACTIVE").ok();
    set_env("JADE_PROVIDER_ACTIVE", dir.0.to_str().unwrap());

    assert!(!is_active());
    assert!(active_lib_path().is_none());
    assert!(active_config().is_empty());

    let lib = dir.0.join(format!("anthropic.{LIB_EXT}"));
    std::fs::write(&lib, b"").unwrap();
    std::fs::write(dir.0.join("config.json"), br#"{"api_key":"sk-x"}"#).unwrap();

    assert!(is_active());
    assert_eq!(active_lib_path().as_deref(), Some(lib.as_path()));
    assert_eq!(active_config(), br#"{"api_key":"sk-x"}"#.to_vec());

    restore_env("JADE_PROVIDER_ACTIVE", prev);
}

#[test]
fn a_dot_so_is_discovered_even_where_lib_ext_is_dylib() {
    // Providers ship as `.so` on every platform; discovery must find one in the
    // slot regardless of this platform's canonical LIB_EXT.
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = TmpDir::new("so-active");
    let prev = std::env::var("JADE_PROVIDER_ACTIVE").ok();
    set_env("JADE_PROVIDER_ACTIVE", dir.0.to_str().unwrap());

    let lib = dir.0.join("anthropic.so");
    std::fs::write(&lib, b"").unwrap();
    assert!(is_active());
    assert_eq!(active_lib_path().as_deref(), Some(lib.as_path()));

    restore_env("JADE_PROVIDER_ACTIVE", prev);
}
