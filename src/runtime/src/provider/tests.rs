//! Tests for the provider driver's pure pieces: the frame-decoding sink (fed
//! encoded [`Frame`]s straight through the C callback, exactly as a provider
//! would) and the active-slot path logic. Driving a real `.so` is an end-to-end
//! concern covered outside the unit tests.

use super::*;
use std::sync::Mutex;

// Path tests mutate `HOME`/`JADE_PROVIDER_ACTIVE`; serialize them (env mutation
// is process-global and `unsafe` in edition 2024).
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
        let dir = std::env::temp_dir().join(format!("jade-drv-{}-{}", std::process::id(), label));
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

// ── the frame-decoding sink ───────────────────────────────────────────────────

fn deliver(sink: &mut Sink, frame: &Frame) {
    let bytes = frame.encode();
    frame_callback(sink as *mut Sink as *mut c_void, bytes.as_ptr(), bytes.len());
}

fn empty_sink<'a>() -> Sink<'a> {
    Sink {
        body: Vec::new(),
        tokens_used: 0,
        error: None,
        decode_error: None,
        done: false,
        on_token: None,
    }
}

#[test]
fn tokens_accumulate_then_done_carries_count() {
    let mut sink = empty_sink();
    deliver(&mut sink, &Frame::Token("foo".into()));
    deliver(&mut sink, &Frame::Token(" bar".into()));
    deliver(&mut sink, &Frame::Done { tokens_used: 7 });

    assert_eq!(sink.body, b"foo bar");
    assert_eq!(sink.tokens_used, 7);
    assert!(sink.done);
    assert!(sink.error.is_none() && sink.decode_error.is_none());
}

#[test]
fn error_frame_is_captured() {
    let mut sink = empty_sink();
    deliver(&mut sink, &Frame::Token("partial".into()));
    deliver(&mut sink, &Frame::Error("model overloaded".into()));
    assert_eq!(sink.error.as_deref(), Some("model overloaded"));
    assert!(!sink.done);
}

#[test]
fn meta_and_json_are_ignored() {
    let mut sink = empty_sink();
    deliver(&mut sink, &Frame::Meta { provider: "anthropic".into() });
    deliver(&mut sink, &Frame::Token("hi".into()));
    deliver(&mut sink, &Frame::Json(r#"{"ok":true}"#.into()));
    deliver(&mut sink, &Frame::Done { tokens_used: 1 });
    assert_eq!(sink.body, b"hi");
    assert!(sink.done);
}

#[test]
fn malformed_frame_sets_decode_error_and_halts() {
    let mut sink = empty_sink();
    deliver(&mut sink, &Frame::Token("before".into()));
    let bad = [0x01u8, 0x05]; // truncated header
    frame_callback(&mut sink as *mut Sink as *mut c_void, bad.as_ptr(), bad.len());
    assert!(sink.decode_error.is_some());
    deliver(&mut sink, &Frame::Token("after".into()));
    assert_eq!(sink.body, b"before");
}

#[test]
fn on_token_forwards_each_token() {
    let mut seen: Vec<Vec<u8>> = Vec::new();
    {
        let mut forward = |b: &[u8]| seen.push(b.to_vec());
        let mut sink = Sink {
            body: Vec::new(),
            tokens_used: 0,
            error: None,
            decode_error: None,
            done: false,
            on_token: Some(&mut forward),
        };
        deliver(&mut sink, &Frame::Token("a".into()));
        deliver(&mut sink, &Frame::Token("b".into()));
        deliver(&mut sink, &Frame::Done { tokens_used: 2 });
        assert_eq!(sink.body, b"ab");
    }
    assert_eq!(seen, vec![b"a".to_vec(), b"b".to_vec()]);
}

#[test]
fn null_ctx_is_a_safe_noop() {
    let bytes = Frame::Token("x".into()).encode();
    frame_callback(core::ptr::null_mut(), bytes.as_ptr(), bytes.len());
}

// ── active-slot resolution ────────────────────────────────────────────────────

#[test]
fn active_slot_resolves_the_single_so_and_config() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = TmpDir::new("active");
    let prev = std::env::var("JADE_PROVIDER_ACTIVE").ok();
    set_env("JADE_PROVIDER_ACTIVE", dir.0.to_str().unwrap());

    // Empty slot → nothing active.
    assert!(!is_active());
    assert!(active_lib_path().is_none());
    assert!(active_config().is_empty());

    // Drop in one provider lib + a config blob.
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
