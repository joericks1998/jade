//! Response frame decoding.
//!
//! Wire layout, daemon → client:
//!
//! ```text
//! [1 byte: type][2 bytes LE: payload_len][payload_len bytes: payload]
//!
//! 0x01 TOKEN   UTF-8 token text
//! 0x02 DONE    8-byte LE u64 tokens_used
//! 0x03 ERROR   UTF-8 error message
//! 0x04 META    UTF-8 model name (sent first)
//! 0x05 JSON    UTF-8 structured payload (e.g. the health report)
//! ```
//!
//! A normal response is `META → TOKEN* → DONE`, or `ERROR` in place of the tail.
//!
//! ## Why the payload is borrowed, not owned
//!
//! [`read_frame`] fills a caller-owned scratch buffer rather than returning a
//! `String` per frame. A token stream is one frame *per token*, so an owned
//! payload means an allocation per token on the hot path — which the C
//! implementation this replaces avoided by reading straight into its
//! accumulation buffer. Keeping that property is why `FrameKind` carries no
//! data: the kind says how to read the scratch, and only the frames a caller
//! actually keeps get copied.

use std::io::Read;

use super::InferError;

pub const TYPE_TOKEN: u8 = 0x01;
pub const TYPE_DONE: u8 = 0x02;
pub const TYPE_ERROR: u8 = 0x03;
pub const TYPE_META: u8 = 0x04;
pub const TYPE_JSON: u8 = 0x05;

/// Which frame arrived. The payload lands in the caller's scratch buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    Token,
    Done,
    Error,
    Meta,
    Json,
}

impl FrameKind {
    fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            TYPE_TOKEN => Some(FrameKind::Token),
            TYPE_DONE => Some(FrameKind::Done),
            TYPE_ERROR => Some(FrameKind::Error),
            TYPE_META => Some(FrameKind::Meta),
            TYPE_JSON => Some(FrameKind::Json),
            _ => None,
        }
    }
}

/// Read exactly one frame, leaving its payload in `payload`.
///
/// `payload` is cleared first; on return it holds exactly the frame body.
/// An unknown tag is refused rather than skipped — the stream is a sequence of
/// length-delimited frames from a single writer, so a tag we don't recognise
/// means we have lost sync, and continuing would misread the next header.
pub fn read_frame<R: Read>(r: &mut R, payload: &mut Vec<u8>) -> Result<FrameKind, InferError> {
    let mut hdr = [0u8; 3];
    r.read_exact(&mut hdr).map_err(InferError::Transport)?;

    let kind = FrameKind::from_tag(hdr[0]).ok_or(InferError::UnknownFrameType(hdr[0]))?;
    let len = u16::from_le_bytes([hdr[1], hdr[2]]) as usize;

    payload.clear();
    payload.resize(len, 0);
    r.read_exact(payload).map_err(InferError::Transport)?;

    Ok(kind)
}

/// The `tokens_used` count from a DONE payload.
///
/// The daemon always writes exactly 8 bytes (its `Frame::encode` has no other
/// path), so a short payload means a daemon that is not speaking this protocol.
/// The C implementation accepted any length and silently reported 0 tokens for
/// a short one, which turned a broken daemon into a plausible-looking answer.
pub fn done_tokens(payload: &[u8]) -> Result<u64, InferError> {
    let bytes: [u8; 8] = payload
        .try_into()
        .map_err(|_| InferError::Malformed("DONE payload must be exactly 8 bytes"))?;
    Ok(u64::from_le_bytes(bytes))
}

/// Decode a payload as UTF-8.
///
/// Text frames carry model names, error messages, and JSON — all of which the
/// daemon builds from Rust `String`s, so invalid UTF-8 here means a corrupted
/// or foreign writer, not a legitimate encoding difference.
pub fn text(payload: &[u8]) -> Result<&str, InferError> {
    std::str::from_utf8(payload).map_err(|_| InferError::InvalidUtf8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build one wire frame, the way the daemon's `Frame::encode` does.
    fn encode(tag: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![tag];
        v.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn reads_a_token_frame() {
        let wire = encode(TYPE_TOKEN, b"hello");
        let mut buf = Vec::new();
        let kind = read_frame(&mut &wire[..], &mut buf).unwrap();
        assert_eq!(kind, FrameKind::Token);
        assert_eq!(text(&buf).unwrap(), "hello");
    }

    #[test]
    fn reads_a_stream_of_frames_in_order() {
        let mut wire = Vec::new();
        wire.extend(encode(TYPE_META, b"qwen3"));
        wire.extend(encode(TYPE_TOKEN, b"foo"));
        wire.extend(encode(TYPE_TOKEN, b" bar"));
        wire.extend(encode(TYPE_DONE, &10u64.to_le_bytes()));

        let mut r = &wire[..];
        let mut buf = Vec::new();
        let mut got = String::new();

        assert_eq!(read_frame(&mut r, &mut buf).unwrap(), FrameKind::Meta);
        assert_eq!(text(&buf).unwrap(), "qwen3");
        assert_eq!(read_frame(&mut r, &mut buf).unwrap(), FrameKind::Token);
        got.push_str(text(&buf).unwrap());
        assert_eq!(read_frame(&mut r, &mut buf).unwrap(), FrameKind::Token);
        got.push_str(text(&buf).unwrap());
        assert_eq!(read_frame(&mut r, &mut buf).unwrap(), FrameKind::Done);
        assert_eq!(done_tokens(&buf).unwrap(), 10);

        assert_eq!(got, "foo bar");
    }

    #[test]
    fn an_empty_payload_is_a_valid_frame() {
        let wire = encode(TYPE_TOKEN, b"");
        let mut buf = vec![0xAA; 16]; // scratch must be cleared, not appended to
        assert_eq!(read_frame(&mut &wire[..], &mut buf).unwrap(), FrameKind::Token);
        assert!(buf.is_empty());
    }

    #[test]
    fn a_truncated_payload_is_a_transport_error() {
        let mut wire = encode(TYPE_TOKEN, b"hello");
        wire.truncate(5); // header plus two of five payload bytes
        let mut buf = Vec::new();
        assert!(matches!(
            read_frame(&mut &wire[..], &mut buf),
            Err(InferError::Transport(_))
        ));
    }

    #[test]
    fn an_unknown_tag_is_refused_rather_than_skipped() {
        let wire = encode(0x7f, b"whatever");
        let mut buf = Vec::new();
        assert!(matches!(
            read_frame(&mut &wire[..], &mut buf),
            Err(InferError::UnknownFrameType(0x7f))
        ));
    }

    /// The C implementation accepted a short DONE and reported 0 tokens, so a
    /// daemon that truncated the count looked like one that used no tokens.
    #[test]
    fn a_short_done_payload_is_malformed_not_zero() {
        assert!(matches!(
            done_tokens(&[1, 2, 3]),
            Err(InferError::Malformed(_))
        ));
        assert_eq!(done_tokens(&7u64.to_le_bytes()).unwrap(), 7);
    }

    #[test]
    fn invalid_utf8_in_a_text_frame_is_rejected() {
        assert!(matches!(text(b"ok\xffbad"), Err(InferError::InvalidUtf8)));
    }
}
