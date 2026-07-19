//! `std::time` runtime module — the AOT C-ABI surface, ported from the C leaf
//! `runtime_lib/time/time.c`. now/now_ms/sleep plus a timezone-aware local().
//! None raise, so no C forwarder. The VM's `time_pkg.rs` is the source of truth.

use core::ffi::c_char;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::string::{self, TAINTED};
use crate::sys::strlen;

/// Allocate a fresh tagged string holding `bytes` with `trust`; returns `char*`.
unsafe fn mk_str(bytes: &[u8], trust: u8) -> *mut c_char {
    let out = string::new(bytes.len(), trust);
    if !bytes.is_empty() {
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
    }
    out as *mut c_char
}

unsafe fn cstr(p: *const c_char) -> &'static str {
    if p.is_null() {
        return "";
    }
    unsafe {
        let n = strlen(p as *const u8);
        core::str::from_utf8(core::slice::from_raw_parts(p as *const u8, n)).unwrap_or("")
    }
}

/// Duration since the Unix epoch (0 if the clock is before it).
fn since_epoch() -> Duration {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO)
}

/// `time.now()` — whole seconds since the Unix epoch.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_time_now() -> i64 {
    since_epoch().as_secs() as i64
}

/// `time.now_ms()` — milliseconds since the Unix epoch.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_time_now_ms() -> i64 {
    since_epoch().as_millis() as i64
}

/// `time.sleep(secs)` — block for `secs` seconds (fractional supported, matching
/// the VM's `Duration::from_secs_f64`). Non-positive durations are a no-op.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_time_sleep(secs: f64) {
    if secs > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(secs));
    }
}

/// `time.local(tz)` — the current local time formatted `%a %b %e %H:%M:%S %Z %Y`,
/// in timezone `tz` (empty → the process default). Shells out to `date` with a
/// `TZ` env override, exactly like the VM's `time_pkg.rs`. External output →
/// TAINTED. Never raises: a spawn failure yields an empty string (matching the C
/// leaf's non-raising contract). No subprocess-free `strftime` path — `date`
/// keeps AOT byte-identical to the VM.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_time_local(tz: *const c_char) -> *mut c_char {
    let tz = unsafe { cstr(tz) };
    let mut cmd = std::process::Command::new("date");
    if tz.is_empty() {
        cmd.env_remove("TZ");
    } else {
        cmd.env("TZ", tz);
    }
    cmd.arg("+%a %b %e %H:%M:%S %Z %Y");
    let s = match cmd.output() {
        Ok(o) if o.status.success() => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            while s.ends_with('\n') || s.ends_with('\r') {
                s.pop();
            }
            s
        }
        _ => String::new(),
    };
    unsafe { mk_str(s.as_bytes(), TAINTED) }
}
