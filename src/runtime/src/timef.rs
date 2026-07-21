//! `std::time` — the single implementation of the `time` stdlib, shared by both
//! engines. Neutral `pub fn` cores (now/now_ms/sleep/local) are called by the VM
//! (`src/time/mod.rs`) and the AOT `#[no_mangle]` wrappers below. `local` shells
//! to `date` (matching the VM exactly, incl. not checking exit status), returning
//! `Err` only on spawn failure (the VM raises; the AOT wrapper falls back to "").

use core::ffi::c_char;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::string::TAINTED;
use crate::cstr;

// ── Neutral cores (used by both engines) ──────────────────────────────────────

/// Whole seconds since the Unix epoch.
pub fn now() -> i64 {
    since_epoch().as_secs() as i64
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    since_epoch().as_millis() as i64
}

/// Block for `secs` seconds (non-positive → no-op).
pub fn sleep(secs: f64) {
    if secs > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(secs));
    }
}

/// The current local time formatted `%a %b %e %H:%M:%S %Z %Y` in timezone `tz`
/// (empty → process default), via `date`. `Err` only if `date` cannot be spawned;
/// the exit status is ignored (matching the VM).
pub fn local(tz: &str) -> std::io::Result<String> {
    let mut cmd = std::process::Command::new("date");
    cmd.arg("+%a %b %e %H:%M:%S %Z %Y");
    if !tz.is_empty() {
        cmd.env("TZ", tz);
    }
    let out = cmd.output()?;
    Ok(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string())
}

/// Duration since the Unix epoch (0 if the clock is before it).
fn since_epoch() -> Duration {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO)
}

/// `time.now()` — whole seconds since the Unix epoch.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_time_now() -> i64 {
    now()
}

/// `time.now_ms()` — milliseconds since the Unix epoch.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_time_now_ms() -> i64 {
    now_ms()
}

/// `time.sleep(secs)` — block for `secs` seconds (non-positive → no-op).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_time_sleep(secs: f64) {
    sleep(secs);
}

/// `time.local(tz)` — formatted local time (TAINTED); empty string on a `date`
/// spawn failure (the AOT has no raise path here).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_time_local(tz: *const c_char) -> *mut c_char {
    let s = local(unsafe { cstr::borrow(tz) }).unwrap_or_default();
    cstr::emit(s.as_bytes(), TAINTED)
}
