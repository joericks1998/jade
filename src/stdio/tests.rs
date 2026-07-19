use super::*;

#[test]
fn broken_pipe_is_recognised() {
    let epipe = std::io::Error::new(ErrorKind::BrokenPipe, "closed");
    assert_eq!(epipe.kind(), ErrorKind::BrokenPipe);
    // Non-EPIPE errors must not be mistaken for a closed pipe; if they were,
    // `exit_if_broken_pipe` would exit(0) and silently swallow real failures.
    let other = std::io::Error::new(ErrorKind::PermissionDenied, "nope");
    assert_ne!(other.kind(), ErrorKind::BrokenPipe);
}

#[test]
fn write_str_handles_ordinary_output() {
    // Smoke test: writing to a live stdout must neither panic nor exit.
    write_str("");
    flush();
}

#[test]
fn recognises_the_macro_broken_pipe_panic() {
    // The exact message std's print macros produce when the reader is gone.
    assert!(is_broken_pipe_message(
        "failed printing to stdout: Broken pipe (os error 32)"
    ));
}

#[test]
fn does_not_swallow_unrelated_panics() {
    // The hook exits(0) on a match, so a false positive here would silently
    // turn a real crash into a successful run.
    for message in [
        "index out of bounds: the len is 3 but the index is 7",
        "called `Option::unwrap()` on a `None` value",
        "attempt to divide by zero",
        "failed printing to stdout: Permission denied (os error 13)",
        "",
    ] {
        assert!(
            !is_broken_pipe_message(message),
            "wrongly treated as a broken pipe: {message:?}"
        );
    }
}
