//! The trust model — one set of rules for both engines.
//!
//! Jade tracks whether a string came from outside the program. A value read
//! from a shell command, a file, the network, an LLM, or stdin is **tainted**;
//! anything derived purely from source literals is **trusted**. Tainted values
//! are refused at sinks that would execute or fetch them, so a program cannot
//! be talked into running a command it was handed.
//!
//! Compiled code has enforced this from the start, carrying the trust byte in
//! the string's header (`string::trust_of`). **The interpreter did not track
//! trust at all**, so the same program behaved differently:
//!
//! ```text
//!     let untrusted = sh.exec("echo whoami")   // tainted: came from outside
//!     sh.exec(untrusted)                       // a code-execution sink
//!
//!     jade run    -> executed it
//!     jade build  -> refused it
//! ```
//!
//! That is why the rules live here rather than in either engine: propagation
//! and the refusal are decided once, and the VM's `JStr` and the AOT string
//! header are two carriers of the same byte.
//!
//! ## Propagation
//!
//! Trust combines with [`combine`] — the *most* tainted input wins. A derived
//! value is trusted only if everything it came from was. This has to hold for
//! every operation that builds a string from others (concatenation, f-string
//! interpolation, `trim`/`replace`/`split`, JSON parsing), or the model is
//! trivially escaped by `sh.exec("" + untrusted)`.

/// Derived purely from program source.
pub const TRUSTED: u8 = 0;
/// Derived, however indirectly, from outside the program.
pub const TAINTED: u8 = 1;

/// Whether a trust byte marks a value as untrusted.
#[inline]
pub fn is_tainted(trust: u8) -> bool {
    trust != TRUSTED
}

/// Combine the trust of two inputs feeding one derived value.
///
/// Taint is contagious: the result is trusted only if both inputs were. Written
/// as a max so adding a third trust level later (say, a distinction between
/// "from a file" and "from the network") keeps the same meaning.
#[inline]
pub fn combine(a: u8, b: u8) -> u8 {
    if a > b { a } else { b }
}

/// Combine the trust of any number of inputs. Empty → [`TRUSTED`], since a
/// value built from nothing external is not tainted.
pub fn combine_all(trusts: impl IntoIterator<Item = u8>) -> u8 {
    trusts.into_iter().fold(TRUSTED, combine)
}

/// The message shown when a tainted value reaches `sink_name`.
///
/// Byte-identical to the C runtime's `jrt_refuse_if_tainted`, because
/// backend-parity diffs output: the wording is a contract, not a cosmetic.
pub fn refusal_message(sink_name: &str) -> String {
    format!(
        "refused tainted string in {sink_name} — value derived from an \
         untrusted source (LLM, network, file, stdin) and cannot flow \
         to a code-execution sink"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taint_is_contagious() {
        assert_eq!(combine(TRUSTED, TRUSTED), TRUSTED);
        assert_eq!(combine(TRUSTED, TAINTED), TAINTED);
        assert_eq!(combine(TAINTED, TRUSTED), TAINTED);
        assert_eq!(combine(TAINTED, TAINTED), TAINTED);
    }

    // A value built from nothing external is trusted — otherwise an empty
    // f-string or a zero-argument join would come out tainted.
    #[test]
    fn nothing_external_is_trusted() {
        assert_eq!(combine_all(std::iter::empty()), TRUSTED);
    }

    #[test]
    fn one_tainted_input_taints_the_result() {
        assert_eq!(combine_all([TRUSTED, TRUSTED, TAINTED, TRUSTED]), TAINTED);
        assert_eq!(combine_all([TRUSTED, TRUSTED]), TRUSTED);
    }

    #[test]
    fn only_trusted_is_untainted() {
        assert!(!is_tainted(TRUSTED));
        assert!(is_tainted(TAINTED));
    }

    // The wording is compared by the backend-parity gate, so it is pinned.
    #[test]
    fn the_refusal_names_the_sink_and_the_reason() {
        let m = refusal_message("sh.exec(cmd)");
        assert!(m.starts_with("refused tainted string in sh.exec(cmd) — "));
        assert!(m.contains("untrusted source (LLM, network, file, stdin)"));
        assert!(m.ends_with("cannot flow to a code-execution sink"));
    }
}

/// A Jade string as the interpreter holds it: the text plus its trust byte.
///
/// The compiled runtime carries trust in an 8-byte header before the character
/// data; the interpreter carries it here. Two carriers, one meaning — the
/// combining and refusal rules above are shared, which is the point.
///
/// It derefs to `str`, so code that only *reads* a string is unaffected. Only
/// the places that build one have to say where its trust came from, which is
/// exactly where the decision belongs.
/// Equality, ordering and hashing are on the **text only**, deliberately — see
/// the impls below. Trust governs where a value may flow, not what it is.
#[derive(Clone, Debug, Default)]
pub struct JStr {
    text: String,
    trust: u8,
}

// Two strings with the same characters are the same value however they were
// obtained. This is not a convenience: Jade programs compare strings and use
// them as dict keys, so if trust took part in equality then a tainted key would
// fail to find the entry a trusted one inserted, and `x == "yes"` would depend
// on where `x` came from. Hash must agree with Eq for exactly that reason.
impl PartialEq for JStr {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}
impl Eq for JStr {}

impl core::hash::Hash for JStr {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.text.hash(state);
    }
}

impl PartialOrd for JStr {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for JStr {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.text.cmp(&other.text)
    }
}

impl JStr {
    /// A string derived from program source.
    pub fn trusted(text: impl Into<String>) -> Self {
        JStr { text: text.into(), trust: TRUSTED }
    }

    /// A string from outside the program — a shell command's output, a file, a
    /// network response, stdin.
    pub fn tainted(text: impl Into<String>) -> Self {
        JStr { text: text.into(), trust: TAINTED }
    }

    /// A string carrying an explicit trust byte, for propagating an existing one.
    pub fn with_trust(text: impl Into<String>, trust: u8) -> Self {
        JStr { text: text.into(), trust }
    }

    pub fn trust(&self) -> u8 {
        self.trust
    }

    pub fn is_tainted(&self) -> bool {
        is_tainted(self.trust)
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn into_string(self) -> String {
        self.text
    }

    /// Derive a new string from this one, keeping its trust. For `trim`,
    /// `upper`, `replace`, slicing — anything whose output is made of this
    /// string's characters and so is exactly as trustworthy.
    pub fn derive(&self, text: impl Into<String>) -> Self {
        JStr { text: text.into(), trust: self.trust }
    }
}

// ── JChar ─────────────────────────────────────────────────────────────────────

/// A Unicode scalar carrying a trust byte, the char analogue of [`JStr`].
///
/// A char exists mostly because it comes *out* of a string, and a character of
/// a tainted string is exactly as untrustworthy as the string was. Without the
/// trust byte here, `tainted[0] + "x"` would produce a clean string and a loop
/// rebuilding a string character by character would launder it silently — the
/// kind of hole that passes every test, since the trust fixtures only ever
/// exercise whole strings.
///
/// In a compiled binary the same flag rides in bit 63 of the char immediate,
/// clear of the 21-bit scalar in bits 5.. and clear of the tag in the low five.
#[derive(Debug, Clone, Copy)]
pub struct JChar {
    ch: char,
    trust: u8,
}

impl PartialEq for JChar {
    /// Trust is provenance, not identity: two spellings of the same character
    /// are the same character whatever they were derived from.
    fn eq(&self, other: &Self) -> bool {
        self.ch == other.ch
    }
}
impl Eq for JChar {}
impl PartialOrd for JChar {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for JChar {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.ch.cmp(&other.ch)
    }
}

impl JChar {
    /// A char from program source.
    pub fn trusted(ch: char) -> Self {
        JChar { ch, trust: TRUSTED }
    }
    /// A char carrying an explicit trust byte, for propagating an existing one.
    pub fn with_trust(ch: char, trust: u8) -> Self {
        JChar { ch, trust }
    }
    pub fn ch(&self) -> char {
        self.ch
    }
    pub fn trust(&self) -> u8 {
        self.trust
    }
    pub fn is_tainted(&self) -> bool {
        is_tainted(self.trust)
    }
}

impl core::fmt::Display for JChar {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.ch)
    }
}

impl core::ops::Deref for JStr {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}

impl core::fmt::Display for JStr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.text)
    }
}

/// Untrusted by default would be safer, but wrong: this is the conversion used
/// throughout the compiler for strings built from *source* — literals, error
/// messages, type names. Anything from outside the program is constructed with
/// [`JStr::tainted`] at the boundary that reads it.
impl From<String> for JStr {
    fn from(text: String) -> Self {
        JStr::trusted(text)
    }
}

impl From<&str> for JStr {
    fn from(text: &str) -> Self {
        JStr::trusted(text)
    }
}

impl PartialEq<str> for JStr {
    fn eq(&self, other: &str) -> bool {
        self.text == other
    }
}

impl PartialEq<&str> for JStr {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

#[cfg(test)]
mod jstr_tests {
    use super::*;

    #[test]
    fn a_derived_string_keeps_the_trust_it_came_from() {
        let t = JStr::tainted("  from outside  ");
        assert!(t.derive(t.trim()).is_tainted(), "trim must not launder taint");
        let s = JStr::trusted("literal");
        assert!(!s.derive(s.to_uppercase()).is_tainted());
    }

    // Equality compares text only. Two strings with the same characters are the
    // same value regardless of where they came from — trust governs where a
    // value may *flow*, not what it *is*.
    #[test]
    fn trust_does_not_affect_equality() {
        assert_eq!(JStr::tainted("x"), JStr::trusted("x"));
    }

    #[test]
    fn it_reads_as_a_str() {
        let s = JStr::tainted("hello");
        assert_eq!(s.len(), 5);
        assert!(s.starts_with("he"));
        assert_eq!(s.as_str(), "hello");
        assert_eq!(s, "hello");
    }

    // Strings built in the compiler come from source, so this is the safe
    // default for the conversion; external input is tainted at the boundary.
    #[test]
    fn conversion_from_a_rust_string_is_trusted() {
        assert!(!JStr::from("literal".to_string()).is_tainted());
    }
}
