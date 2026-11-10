use super::*;

// ── sha256_hex ────────────────────────────────────────────────────────────────

#[test]
fn sha256_hex_matches_known_digest() {
    // The canonical empty-input SHA-256, so a broken hasher can't pass by
    // agreeing with itself.
    assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn sha256_hex_is_lowercase_and_64_chars() {
    let hex = sha256_hex(b"jade");
    assert_eq!(hex.len(), 64);
    assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
}

#[test]
fn sha256_hex_distinguishes_content() {
    assert_ne!(sha256_hex(b"a"), sha256_hex(b"b"));
}

// ── Platform tags ─────────────────────────────────────────────────────────────

#[test]
fn platform_tag_is_listed_as_supported() {
    // Whatever this host reports must be resolvable at install time; a tag
    // platform_tag can return but SUPPORTED_PLATFORMS omits would produce locks
    // that cannot be installed on the machine that wrote them.
    if let Some(tag) = platform_tag() {
        assert!(
            SUPPORTED_PLATFORMS.contains(&tag),
            "platform_tag() returned {tag:?}, which is missing from SUPPORTED_PLATFORMS"
        );
    }
}

#[test]
fn supported_platforms_are_unique() {
    let mut seen = std::collections::HashSet::new();
    for p in SUPPORTED_PLATFORMS {
        assert!(seen.insert(p), "duplicate platform tag: {p}");
    }
}

// ── URL templating ────────────────────────────────────────────────────────────

#[test]
fn expand_platform_substitutes_the_placeholder() {
    assert_eq!(
        expand_platform("https://x/tok-{platform}.so", "linux-x86_64"),
        "https://x/tok-linux-x86_64.so"
    );
}

#[test]
fn expand_platform_replaces_every_occurrence() {
    assert_eq!(
        expand_platform("https://x/{platform}/tok-{platform}.so", "darwin-aarch64"),
        "https://x/darwin-aarch64/tok-darwin-aarch64.so"
    );
}

#[test]
fn expand_platform_leaves_a_plain_url_alone() {
    // A dependency that ships one build for one platform is legitimate.
    let url = "https://x/tok.so";
    assert_eq!(expand_platform(url, "linux-x86_64"), url);
}
