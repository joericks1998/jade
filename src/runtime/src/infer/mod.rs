//! The inference daemon client — one implementation for both engines.
//!
//! This talked to the daemon twice over: `runtime_aot/ipc/ipc.c` for compiled
//! binaries and `src/llm/jaded.rs` for the VM, in two languages, with no shared
//! definition of the format between them. They had already drifted:
//!
//! ```text
//!                       ipc.c                    jaded.rs
//!   short DONE payload  reports 0 tokens         rejects as malformed
//!   invalid UTF-8       passes through           rejects
//!   connection          persistent, one fd       reconnects per request
//!   size/frame ceiling  64 MiB / 4M frames       none
//! ```
//!
//! Every row is a behaviour difference between `jade run` and `jade build` on
//! the same program, and the last one meant the DoS ceilings guarding against a
//! hostile daemon existed only on the compiled path. None of it was chosen;
//! it is what two copies of one protocol do over time.
//!
//! The backend-parity harness could not have caught any of it either — it
//! compares stdout, and these diverge only against a daemon misbehaving in
//! specific ways.
//!
//! ## Layout
//!
//! * [`frame`] — decoding response frames off the wire. Pure, no I/O of its own.
//! * [`conn`] — the socket: lazy connect, serialized requests, accumulation.
//! * `ffi` — the `jrt_ipc_*` C entry points `runtime_aot/infer/infer.c` calls.
//!
//! Compiled binaries reach this through the C ABI; the VM links the same code
//! as an rlib and uses [`conn`] directly.

pub mod conn;
pub mod frame;
mod ffi;

pub use conn::{sock_path, Mode, Response};

/// Why an exchange with the daemon failed.
///
/// The two engines report these differently and legitimately so: a compiled
/// binary has no interpreter to unwind into, so `ffi` prints and exits, while
/// the VM maps these into a catchable `JadeError` carrying a source span. That
/// difference is a property of the caller, not the protocol, which is why it
/// lives at the edges rather than in here.
#[derive(Debug)]
pub enum InferError {
    /// The socket could not be opened after retries.
    Connect {
        path: String,
        source: std::io::Error,
    },
    /// A read or write failed mid-exchange, including a daemon that hung up.
    Transport(std::io::Error),
    /// The daemon sent an ERROR frame. The payload is its message.
    Daemon(String),
    /// A frame tag we do not recognise — the stream is out of sync.
    UnknownFrameType(u8),
    /// A frame's payload was not the shape its type requires.
    Malformed(&'static str),
    /// A text frame's payload was not UTF-8.
    InvalidUtf8,
    /// The daemon streamed past the accumulated-bytes ceiling.
    ResponseTooLarge,
    /// The daemon streamed past the frame-count ceiling.
    FrameLimit,
    /// The request body exceeded the 4-byte length prefix.
    RequestTooLarge(usize),
}

impl std::fmt::Display for InferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect { path, source } => write!(
                f,
                "cannot connect to inference daemon at {path} — is jaded running? ({source})"
            ),
            Self::Transport(e) => write!(f, "inference daemon connection failed: {e}"),
            Self::Daemon(msg) => write!(f, "inference error from the daemon: {msg}"),
            Self::UnknownFrameType(t) => write!(f, "unknown frame type {t:#04x} from the daemon"),
            Self::Malformed(what) => write!(f, "malformed frame from the daemon: {what}"),
            Self::InvalidUtf8 => write!(f, "the daemon sent invalid UTF-8 in a text frame"),
            Self::ResponseTooLarge => write!(f, "daemon response exceeded the size limit"),
            Self::FrameLimit => write!(f, "daemon response exceeded the frame limit"),
            Self::RequestTooLarge(n) => write!(f, "request body of {n} bytes is too large to frame"),
        }
    }
}

impl std::error::Error for InferError {}
