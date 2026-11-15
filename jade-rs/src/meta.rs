//! The description of a package, built while the package compiles.
//!
//! Each exported function and struct adds one record to the built library, in a
//! section of its own: `__DATA,__jade_meta` on macOS, `.jade_meta` elsewhere. A record
//! is the eight bytes `JADEMETA`, the length of what follows as a little-endian `u32`,
//! and a JSON object. The Jade compiler reads every record out of the staticlib it
//! builds (`compiler/src/rustpkg/meta.rs`), which is how it learns what each function
//! takes and returns without a header.
//!
//! The JSON is assembled by `const fn`s from each type's [`Type::JADE`] constant, so a
//! type alias or a nested type is described as what rustc resolved it to, and the
//! description cannot disagree with the code it sits beside. This is UniFFI's
//! approach; `design/v2-rust-packages.md` says what was taken from it.
//!
//! [`Type::JADE`]: crate::convert::Type::JADE

/// The record format. The compiler refuses a record of another format, naming the
/// `jade-rs` version it reads. Bump it when a record's fields, a type's spelling or
/// a C form in `c.rs` changes.
pub const FORMAT: usize = 1;

/// The bytes every record starts with.
const MAGIC: &[u8; 8] = b"JADEMETA";

/// The longest description one type or record may have.
const CAP: usize = 4096;

/// Purpose: This struct builds text at compile time: a type's Jade spelling, or a
/// whole record.
///
/// Every method takes and returns the builder by value, so a description reads as one
/// chain inside a `const` item.
#[derive(Clone, Copy)]
pub struct Meta {
    buf: [u8; CAP],
    len: usize,
}

impl Meta {
    /// Purpose: This function starts an empty description.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the builder.
    pub const fn new() -> Self {
        Meta { buf: [0; CAP], len: 0 }
    }

    /// Purpose: This function appends text.
    ///
    /// Inputs:
    /// - `s`: the text.
    ///
    /// Outputs: the builder. A description longer than 4096 bytes stops the build.
    pub const fn str(self, s: &str) -> Self {
        self.bytes(s.as_bytes())
    }

    /// Purpose: This function appends another description, such as a field's type.
    ///
    /// Inputs:
    /// - `m`: the description.
    ///
    /// Outputs: the builder.
    pub const fn meta(self, m: &Meta) -> Self {
        let (head, _) = m.buf.split_at(m.len);
        self.bytes(head)
    }

    /// Purpose: This function appends a number in decimal.
    ///
    /// Inputs:
    /// - `n`: the number.
    ///
    /// Outputs: the builder.
    pub const fn num(self, n: usize) -> Self {
        let mut digits = [0u8; 20];
        let mut i = digits.len();
        let mut n = n;
        loop {
            i -= 1;
            digits[i] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        let (_, tail) = digits.split_at(i);
        self.bytes(tail)
    }

    /// Purpose: This function appends `true` or `false`.
    ///
    /// Inputs:
    /// - `b`: the value.
    ///
    /// Outputs: the builder.
    pub const fn bool(self, b: bool) -> Self {
        self.str(if b { "true" } else { "false" })
    }

    /// Purpose: This function appends raw bytes; every other method comes here.
    const fn bytes(mut self, b: &[u8]) -> Self {
        if self.len + b.len() > CAP {
            panic!("a jade::export description is longer than 4096 bytes; split the struct");
        }
        let mut i = 0;
        while i < b.len() {
            self.buf[self.len] = b[i];
            self.len += 1;
            i += 1;
        }
        self
    }

    /// Purpose: This function says how many bytes the record of this description
    /// takes: the magic, the length and the JSON.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the size, for the static that holds the record.
    pub const fn record_len(&self) -> usize {
        MAGIC.len() + 4 + self.len
    }

    /// Purpose: This function writes the record of this description.
    ///
    /// Inputs: none; `N` is the static's length, which must be
    /// [`Meta::record_len`].
    ///
    /// Outputs: the record's bytes.
    pub const fn record<const N: usize>(&self) -> [u8; N] {
        if N != self.record_len() {
            panic!("a jade::export record was given the wrong length");
        }
        let mut out = [0u8; N];
        let mut i = 0;
        while i < MAGIC.len() {
            out[i] = MAGIC[i];
            i += 1;
        }
        let len = (self.len as u32).to_le_bytes();
        let mut k = 0;
        while k < 4 {
            out[i] = len[k];
            i += 1;
            k += 1;
        }
        let mut j = 0;
        while j < self.len {
            out[i] = self.buf[j];
            i += 1;
            j += 1;
        }
        out
    }

    /// Purpose: This function reads the description back as text, for tests.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the text, or an empty string if it is not UTF-8, which a description
    /// built from Rust names and string literals always is.
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl Default for Meta {
    fn default() -> Self {
        Meta::new()
    }
}
