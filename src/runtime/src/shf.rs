//! `std::sh` — the single implementation of the `sh` stdlib, shared by both
//! engines. Neutral `pub fn` cores (exec/run/output) run via `sh -c` and return
//! `Result<_, String>` where the `Err` is the full Jade error message. The VM
//! (`src/sh/mod.rs`) maps `Err` to `JadeError::IoError`; the AOT wrappers record a
//! thread-local pending error that a C forwarder throws.
//!
//! `exec` RAISES on a non-zero exit (the VM's contract — the canonical one), so
//! the AOT now raises there too. exec/run are code-execution sinks; the tainted-
//! input refusal stays in the C forwarder.

use core::ffi::c_char;
use std::cell::Cell;
use std::process::Command;

use crate::string::{self, TAINTED, TRUSTED};
use crate::cstr;

// ── Neutral cores (used by both engines) ──────────────────────────────────────

/// `sh.exec(cmd)` — run via `sh -c`, return trimmed stdout. `Err` (a full message)
/// on spawn failure or a non-zero exit (stderr included).
pub fn exec(cmd: &str) -> Result<String, String> {
    let out = Command::new("sh")
        .args(["-c", cmd])
        .output()
        .map_err(|e| format!("sh.exec: could not spawn shell: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string())
    } else {
        Err(format!(
            "sh.exec: command exited with code {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// `sh.run(cmd)` — run via `sh -c` inheriting stdio, return the exit code. `Err`
/// on spawn failure.
pub fn run(cmd: &str) -> Result<i64, String> {
    Command::new("sh")
        .args(["-c", cmd])
        .status()
        .map(|s| s.code().unwrap_or(-1) as i64)
        .map_err(|e| format!("sh.run: could not spawn shell: {e}"))
}

/// `sh.output(cmd)` — run via `sh -c`, capture all streams → `(stdout, stderr,
/// code)`. `Err` on spawn failure.
pub fn output(cmd: &str) -> Result<(String, String, i64), String> {
    let out = Command::new("sh")
        .args(["-c", cmd])
        .output()
        .map_err(|e| format!("sh.output: could not spawn shell: {e}"))?;
    Ok((
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1) as i64,
    ))
}

// ── AOT C-ABI wrappers (pending-error channel; forwarders in common.c throw) ───

thread_local! {
    static PENDING: Cell<*mut c_char> = const { Cell::new(core::ptr::null_mut()) };
}

fn set_err(msg: &str) {
    let s = cstr::emit(msg.as_bytes(), TRUSTED);
    PENDING.with(|p| {
        let old = p.replace(s);
        if !old.is_null() {
            string::free_str(old as *mut u8);
        }
    });
}

/// Drain the pending sh error (a tagged string the caller owns), or null.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_sh_take_error() -> *mut c_char {
    PENDING.with(|p| p.replace(core::ptr::null_mut()))
}

/// `sh.exec` core (the forwarder refuses a tainted cmd, then throws any pending
/// error). Returns trimmed stdout as a TAINTED string; on error, "" + pending.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_sh_exec_impl(cmd: *const c_char) -> *mut c_char {
    match exec(unsafe { cstr::borrow(cmd) }) {
        Ok(s) => cstr::emit(s.as_bytes(), TAINTED),
        Err(m) => {
            set_err(&m);
            cstr::emit(b"", TAINTED)
        }
    }
}

/// `sh.run` core — exit code, or -1 + pending on spawn failure.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_sh_run_impl(cmd: *const c_char) -> i64 {
    match run(unsafe { cstr::borrow(cmd) }) {
        Ok(c) => c,
        Err(m) => {
            set_err(&m);
            -1
        }
    }
}
