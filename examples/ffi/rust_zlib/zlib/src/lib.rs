//! zlib for Jade: compress, decompress and checksum bytes.
//!
//! `libz-sys` declares zlib's C functions; this crate calls them with the buffers
//! and lengths they need and exports a safe function for each, which
//! `#[jade::export]` makes callable from a Jade program. A zlib status that is not
//! `Z_OK` comes back as an `Err`, which the program catches as a raise.

use libz_sys as z;

/// Purpose: This function names a zlib status, for an error message.
fn status(code: i32) -> String {
    match code {
        z::Z_MEM_ERROR => "zlib ran out of memory".to_string(),
        z::Z_BUF_ERROR => "zlib's output buffer was too small".to_string(),
        z::Z_DATA_ERROR => "the data is not zlib-compressed, or is damaged".to_string(),
        z::Z_STREAM_ERROR => "the compression level must be 0 to 9".to_string(),
        other => format!("zlib failed with status {other}"),
    }
}

/// Compresses bytes at a level from 0, none, to 9, smallest.
#[jade::export]
pub fn compress(data: &[u8], level: i32) -> Result<Vec<u8>, String> {
    // SAFETY: `compressBound` only computes a size.
    let mut cap = unsafe { z::compressBound(data.len() as z::uLong) };
    let mut out = vec![0u8; cap as usize];
    // SAFETY: `out` holds `cap` writable bytes and `data` its own length; zlib
    // writes the compressed length back through `cap`.
    let code = unsafe {
        z::compress2(out.as_mut_ptr(), &mut cap, data.as_ptr(), data.len() as z::uLong, level)
    };
    if code != z::Z_OK {
        return Err(status(code));
    }
    out.truncate(cap as usize);
    Ok(out)
}

/// Decompresses what `compress` made, growing the output until it fits.
#[jade::export]
pub fn decompress(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut size = (data.len() * 4).max(64);
    loop {
        let mut out = vec![0u8; size];
        let mut len = size as z::uLong;
        // SAFETY: as in `compress`, with `len` the room in `out`.
        let code = unsafe {
            z::uncompress(out.as_mut_ptr(), &mut len, data.as_ptr(), data.len() as z::uLong)
        };
        match code {
            z::Z_OK => {
                out.truncate(len as usize);
                return Ok(out);
            }
            z::Z_BUF_ERROR if size < 1 << 30 => size *= 4,
            other => return Err(status(other)),
        }
    }
}

/// The CRC-32 of some bytes, as zlib computes it.
#[jade::export]
pub fn crc32(data: &[u8]) -> u32 {
    // SAFETY: zlib reads `data.len()` bytes from `data`.
    unsafe { z::crc32(0, data.as_ptr(), data.len() as z::uInt) as u32 }
}

/// The version of the zlib linked in, such as `1.3.1`.
#[jade::export]
pub fn version() -> String {
    // SAFETY: zlibVersion returns a pointer to a static NUL-terminated string.
    unsafe { std::ffi::CStr::from_ptr(z::zlibVersion()).to_string_lossy().into_owned() }
}
