//! Stdout writes that survive a closed pipe.
//!
//! Rust ignores `SIGPIPE`, so writing to a pipe whose reader has already exited
//! — `jade run app.jde | head -3` — makes the write return `EPIPE`, and the
//! `print!`/`println!` macros *panic* on a failed write. The user sees a Rust
//! panic and a backtrace for what is ordinary shell usage.
//!
//! The usual Unix fix is to restore `SIG_DFL` for `SIGPIPE` at startup so the
//! process dies quietly like `yes` or `cat`. That is deliberately **not** what
//! this does: Jade also writes to sockets (the build daemon, the LLM daemon,
//! `std::uhttp`), and under `SIG_DFL` a peer disconnecting would kill the whole
//! process instead of surfacing a normal `io::Error` that those call sites
//! already handle. Restoring the default signal would trade a visible stdout
//! panic for a silent, much harder-to-debug death on socket I/O.
//!
//! So the handling is scoped to the stdout boundary: a broken pipe means the
//! consumer stopped reading, which is a successful end of output, not an error.

use std::io::{ErrorKind, Write};

/// Write `s` to stdout, exiting quietly if the reader has gone away.
///
/// Does not flush — matching `print!`, so piped output stays block-buffered and
/// throughput is unaffected. Use [`flush`] where output must appear immediately
/// (prompts, streaming).
pub fn write_str(s: &str) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    if let Err(e) = lock.write_all(s.as_bytes()) {
        exit_if_broken_pipe(&e);
        // Any other write error is reported the way the macros would have, but
        // without unwinding through the VM.
        let _ = writeln!(std::io::stderr(), "jade: error writing to stdout: {e}");
    }
}

/// Flush stdout, exiting quietly if the reader has gone away.
pub fn flush() {
    if let Err(e) = std::io::stdout().flush() {
        exit_if_broken_pipe(&e);
    }
}

/// Write `s` and flush immediately — for output that must not sit in the buffer.
pub fn write_str_flush(s: &str) {
    write_str(s);
    flush();
}

/// A closed downstream pipe is a normal end of output, so exit successfully
/// rather than unwinding or reporting an error the user did not cause.
fn exit_if_broken_pipe(e: &std::io::Error) {
    if e.kind() == ErrorKind::BrokenPipe {
        std::process::exit(0);
    }
}

/// Install a panic hook that turns a broken-pipe panic into a quiet exit.
///
/// The runtime print path calls [`write_str`] and never needs this. The CLI is
/// the reason it exists: `print!`/`println!` panic on a failed write and are used
/// in ~100 places (help text, `jade env`, `jade fmt`, the REPL, the test runner,
/// the `--verbose` globals dump). Rewriting every one would be churn that the
/// next `println!` silently undoes, so this backstops all of them at once.
///
/// Detection is by panic message, because the macros discard the underlying
/// `io::Error` and panic with a formatted string — there is no error kind left to
/// inspect. That is a slightly loose match, so it is deliberately narrow: only a
/// panic naming a broken pipe is swallowed, and everything else reaches the
/// normal hook with its message and backtrace intact.
pub fn install_broken_pipe_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        if is_broken_pipe_message(message) {
            std::process::exit(0);
        }
        default_hook(info);
    }));
}

/// Whether a panic message describes a closed downstream pipe.
///
/// Split out from the hook so the matching can be tested directly — a
/// `PanicHookInfo` cannot be constructed outside a real panic.
fn is_broken_pipe_message(message: &str) -> bool {
    message.contains("Broken pipe") || message.contains("os error 32")
}

#[cfg(test)]
mod tests;
