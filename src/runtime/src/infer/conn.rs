//! The daemon connection: one socket, opened lazily, serialized by a mutex.
//!
//! One fd is opened on first use and held for the process lifetime. The
//! alternative — a socket per `?p` — costs a connect round trip per prompt,
//! which a pipeline like `data |> f |> g |> h` pays at every stage.
//!
//! Requests are serialized: the protocol interleaves nothing, so two concurrent
//! requests on one fd would read each other's frames. Spawned Jade tasks share
//! this connection and queue behind the lock.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::{Mutex, OnceLock};

use super::frame::{self, FrameKind};
use super::InferError;

/// Ceilings on what one request may accumulate. The daemon is a separate
/// process that may be buggy or hostile; without these it can hang or OOM the
/// program by streaming forever.
const MAX_RESPONSE_BYTES: usize = 64 << 20; // 64 MiB
const MAX_FRAMES: u64 = 4_000_000;

/// How the response body is assembled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Accumulate TOKEN frames; ignore JSON. Used by generation.
    Tokens,
    /// Accumulate JSON frames; ignore TOKEN. Used by structured ops like health.
    Json,
}

impl Mode {
    /// Whether a frame of this kind contributes to the response body. The other
    /// kind is read and dropped rather than left in the stream — its bytes are
    /// already counted in the frame header, so skipping the read would desync
    /// the next frame.
    fn accumulates(self, kind: FrameKind) -> bool {
        matches!(
            (self, kind),
            (Mode::Tokens, FrameKind::Token) | (Mode::Json, FrameKind::Json)
        )
    }
}

/// A completed exchange.
#[derive(Debug)]
pub struct Response {
    pub body: Vec<u8>,
    pub tokens_used: u64,
}

/// The socket path: `JADE_LLM_SOCK` if set and non-empty, else
/// `$HOME/.jade/llm.sock`. `HOME` unset falls back to `/root`, which is where a
/// daemon started by a system service manager puts it.
pub fn sock_path() -> String {
    match std::env::var("JADE_LLM_SOCK") {
        Ok(s) if !s.is_empty() => s,
        _ => {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
            format!("{home}/.jade/llm.sock")
        }
    }
}

/// The process-wide connection, plus the model name from the latest META frame.
pub struct Conn {
    stream: Mutex<Option<UnixStream>>,
    /// What the daemon reported it is serving. Read by `__model__`.
    reported_model: Mutex<String>,
    path: String,
}

static CONN: OnceLock<Conn> = OnceLock::new();

/// The process-wide connection, created (but not yet connected) on first use.
pub fn shared() -> &'static Conn {
    CONN.get_or_init(|| Conn {
        stream: Mutex::new(None),
        reported_model: Mutex::new(String::new()),
        path: sock_path(),
    })
}

impl Conn {
    /// A connection to `path`, not opened until the first request.
    ///
    /// Compiled binaries use [`shared`] — one connection for the process,
    /// requests queued behind its lock. The VM builds one of these per request
    /// instead, because it runs `async` prompts concurrently and a single
    /// serialized connection would turn those back into a sequence. That is a
    /// difference in connection *policy*, which each engine picks; the framing
    /// and accumulation below are the same either way, and those were what had
    /// drifted.
    pub fn new(path: impl Into<String>) -> Self {
        Conn {
            stream: Mutex::new(None),
            reported_model: Mutex::new(String::new()),
            path: path.into(),
        }
    }

    pub fn reported_model(&self) -> String {
        self.reported_model.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// Close the socket. The next request reconnects.
    pub fn shutdown(&self) {
        *self.stream.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Send `req` (a JSON body; the 4-byte length prefix is added here) and
    /// accumulate the response.
    ///
    /// `on_token` fires per TOKEN frame in [`Mode::Tokens`], before the token is
    /// appended, so a caller can stream output while the body accumulates.
    pub fn request(
        &self,
        req: &[u8],
        mode: Mode,
        mut on_token: Option<&mut dyn FnMut(&[u8])>,
    ) -> Result<Response, InferError> {
        let mut guard = self.stream.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            *guard = Some(self.connect()?);
        }
        // A transport failure leaves the connection in an unknown state — a
        // half-written request, or frames from the failed exchange still queued.
        // Drop it so the next request starts clean rather than reading this
        // request's tail as its own response.
        match self.exchange(guard.as_mut().expect("just connected"), req, mode, &mut on_token) {
            Ok(resp) => Ok(resp),
            Err(e) => {
                *guard = None;
                Err(e)
            }
        }
    }

    /// Connect, retrying briefly: a program started alongside the daemon can
    /// reach its first prompt before the daemon has bound the socket.
    fn connect(&self) -> Result<UnixStream, InferError> {
        let mut last = None;
        for attempt in 0..3 {
            match UnixStream::connect(&self.path) {
                Ok(s) => return Ok(s),
                Err(e) => {
                    last = Some(e);
                    if attempt < 2 {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
            }
        }
        Err(InferError::Connect {
            path: self.path.clone(),
            source: last.expect("loop runs at least once"),
        })
    }

    fn exchange(
        &self,
        stream: &mut UnixStream,
        req: &[u8],
        mode: Mode,
        on_token: &mut Option<&mut dyn FnMut(&[u8])>,
    ) -> Result<Response, InferError> {
        let len = u32::try_from(req.len()).map_err(|_| InferError::RequestTooLarge(req.len()))?;
        stream.write_all(&len.to_le_bytes()).map_err(InferError::Transport)?;
        stream.write_all(req).map_err(InferError::Transport)?;
        stream.flush().map_err(InferError::Transport)?;

        let mut body = Vec::new();
        let mut payload = Vec::new();
        let mut tokens_used = 0u64;
        let mut frames = 0u64;

        loop {
            frames += 1;
            if frames > MAX_FRAMES {
                return Err(InferError::FrameLimit);
            }

            let kind = frame::read_frame(stream, &mut payload)?;

            match kind {
                FrameKind::Token | FrameKind::Json => {
                    if !mode.accumulates(kind) {
                        continue; // already consumed from the stream; just drop it
                    }
                    if body.len() + payload.len() > MAX_RESPONSE_BYTES {
                        return Err(InferError::ResponseTooLarge);
                    }
                    if kind == FrameKind::Token && !payload.is_empty() {
                        if let Some(cb) = on_token.as_mut() {
                            cb(&payload);
                        }
                    }
                    body.extend_from_slice(&payload);
                }
                FrameKind::Done => {
                    tokens_used = frame::done_tokens(&payload)?;
                    break;
                }
                FrameKind::Error => {
                    return Err(InferError::Daemon(frame::text(&payload)?.to_owned()));
                }
                FrameKind::Meta => {
                    let model = frame::text(&payload)?.to_owned();
                    *self.reported_model.lock().unwrap_or_else(|e| e.into_inner()) = model;
                }
            }
        }

        Ok(Response { body, tokens_used })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixListener;

    fn encode(tag: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![tag];
        v.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    /// A one-shot daemon that reads a request and replies with `frames`.
    /// Returns the socket path and the request bytes it received.
    fn serve(frames: Vec<u8>) -> (Conn, std::thread::JoinHandle<Vec<u8>>) {
        let dir = std::env::temp_dir().join(format!("jrt-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // Unique per test: the listener path must not collide across threads.
        let path = dir.join(format!("s{:p}.sock", &frames as *const _));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind");

        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            let mut lenbuf = [0u8; 4];
            sock.read_exact(&mut lenbuf).expect("read len");
            let mut req = vec![0u8; u32::from_le_bytes(lenbuf) as usize];
            sock.read_exact(&mut req).expect("read body");
            sock.write_all(&frames).expect("write frames");
            req
        });

        let conn = Conn::new(path.to_string_lossy().into_owned());
        (conn, handle)
    }

    #[test]
    fn accumulates_tokens_and_records_the_model() {
        let mut wire = Vec::new();
        wire.extend(encode(ovata_infer_protocol::response::tag::META, b"qwen3-coder"));
        wire.extend(encode(ovata_infer_protocol::response::tag::TOKEN, b"4"));
        wire.extend(encode(ovata_infer_protocol::response::tag::TOKEN, b"2"));
        wire.extend(encode(ovata_infer_protocol::response::tag::DONE, &99u64.to_le_bytes()));
        let (conn, server) = serve(wire);

        let resp = conn.request(br#"{"prompt":"x"}"#, Mode::Tokens, None).unwrap();

        assert_eq!(resp.body, b"42");
        assert_eq!(resp.tokens_used, 99);
        assert_eq!(conn.reported_model(), "qwen3-coder");

        let req = server.join().unwrap();
        assert_eq!(req, br#"{"prompt":"x"}"#, "length prefix framed the body exactly");
    }

    #[test]
    fn on_token_fires_per_token_in_order() {
        let mut wire = Vec::new();
        wire.extend(encode(ovata_infer_protocol::response::tag::TOKEN, b"a"));
        wire.extend(encode(ovata_infer_protocol::response::tag::TOKEN, b"b"));
        wire.extend(encode(ovata_infer_protocol::response::tag::DONE, &0u64.to_le_bytes()));
        let (conn, server) = serve(wire);

        let mut seen: Vec<String> = Vec::new();
        let mut cb = |t: &[u8]| seen.push(String::from_utf8_lossy(t).into_owned());
        let resp = conn.request(b"{}", Mode::Tokens, Some(&mut cb)).unwrap();

        assert_eq!(seen, ["a", "b"]);
        assert_eq!(resp.body, b"ab");
        server.join().unwrap();
    }

    /// The two modes share one accumulation path; each must ignore the other's
    /// frames without losing sync on the frames it does want.
    #[test]
    fn json_mode_ignores_tokens_and_token_mode_ignores_json() {
        let mut wire = Vec::new();
        wire.extend(encode(ovata_infer_protocol::response::tag::TOKEN, b"noise"));
        wire.extend(encode(ovata_infer_protocol::response::tag::JSON, br#"{"status":"ok"}"#));
        wire.extend(encode(ovata_infer_protocol::response::tag::TOKEN, b"more noise"));
        wire.extend(encode(ovata_infer_protocol::response::tag::DONE, &0u64.to_le_bytes()));
        let (conn, server) = serve(wire.clone());

        let resp = conn.request(b"{}", Mode::Json, None).unwrap();
        assert_eq!(resp.body, br#"{"status":"ok"}"#);
        server.join().unwrap();

        let (conn2, server2) = serve(wire);
        let resp2 = conn2.request(b"{}", Mode::Tokens, None).unwrap();
        assert_eq!(resp2.body, b"noisemore noise");
        server2.join().unwrap();
    }

    #[test]
    fn an_error_frame_becomes_a_daemon_error() {
        let (conn, server) = serve(encode(ovata_infer_protocol::response::tag::ERROR, b"model not loaded"));
        let err = conn.request(b"{}", Mode::Tokens, None).unwrap_err();
        assert!(matches!(err, InferError::Daemon(ref m) if m == "model not loaded"));
        server.join().unwrap();
    }

    /// A failed exchange must not leave a poisoned fd behind: the next request
    /// would otherwise read the dead connection instead of reconnecting.
    #[test]
    fn a_failed_exchange_drops_the_connection() {
        let (conn, server) = serve(encode(ovata_infer_protocol::response::tag::ERROR, b"boom"));
        assert!(conn.request(b"{}", Mode::Tokens, None).is_err());
        assert!(
            conn.stream.lock().unwrap().is_none(),
            "connection dropped so the next request reconnects"
        );
        server.join().unwrap();
    }

    #[test]
    fn connect_failure_names_the_path() {
        let conn = Conn::new("/nonexistent/jade-test.sock");
        match conn.request(b"{}", Mode::Tokens, None) {
            Err(InferError::Connect { path, .. }) => {
                assert_eq!(path, "/nonexistent/jade-test.sock")
            }
            other => panic!("expected a connect error, got {other:?}"),
        }
    }

    #[test]
    fn sock_path_prefers_the_env_override() {
        // Guarded: these are process-wide. Run serially within this one test.
        let saved = std::env::var("JADE_LLM_SOCK").ok();

        unsafe { std::env::set_var("JADE_LLM_SOCK", "/custom/llm.sock") };
        assert_eq!(sock_path(), "/custom/llm.sock");

        // An empty override is treated as unset, not as an empty path.
        unsafe { std::env::set_var("JADE_LLM_SOCK", "") };
        unsafe { std::env::set_var("HOME", "/home/j") };
        assert_eq!(sock_path(), "/home/j/.jade/llm.sock");

        match saved {
            Some(v) => unsafe { std::env::set_var("JADE_LLM_SOCK", v) },
            None => unsafe { std::env::remove_var("JADE_LLM_SOCK") },
        }
    }
}
