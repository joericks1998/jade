//! `std::sh` runtime module — the AOT C-ABI surface, ported from the C leaf
//! `runtime_lib/sh/sh.c`. exec/run via `sh -c` (matching the VM's `sh_pkg.rs`
//! and the already-Rust `jrt_coll_sh_output`). All captured output is TAINTED.
//!
//! exec/run are code-execution sinks that refuse tainted input — the only throw
//! — so that check stays in the C forwarder (`jrt_refuse_if_tainted`); these
//! impls never raise (spawn failure → "" / -1, matching the C leaf).

use core::ffi::c_char;
use std::process::Command;

use crate::string::{self, TAINTED};
use crate::sys::strlen;

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

/// `sh.exec(cmd)` core — run via `sh -c`, return trimmed stdout as a TAINTED
/// string (empty on spawn failure). The forwarder refuses a tainted `cmd` first.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_sh_exec_impl(cmd: *const c_char) -> *mut c_char {
    let out = Command::new("sh").args(["-c", unsafe { cstr(cmd) }]).output();
    let bytes = match out {
        Ok(o) => {
            let mut v = o.stdout;
            while matches!(v.last(), Some(b'\n' | b'\r')) {
                v.pop();
            }
            v
        }
        Err(_) => Vec::new(),
    };
    unsafe { mk_str(&bytes, TAINTED) }
}

/// `sh.run(cmd)` core — run via `sh -c`, discard output, return the exit code
/// (`status.code().unwrap_or(-1)`; -1 on signal/spawn failure). The forwarder
/// refuses a tainted `cmd` first.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_sh_run_impl(cmd: *const c_char) -> i64 {
    match Command::new("sh").args(["-c", unsafe { cstr(cmd) }]).status() {
        Ok(s) => s.code().unwrap_or(-1) as i64,
        Err(_) => -1,
    }
}
