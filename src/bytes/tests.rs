use super::*;
use jade_runtime::trust::{JStr, TAINTED, TRUSTED};

fn blob(data: Vec<u8>, trust: u8) -> VmValue {
    make_bytes(data, trust)
}

#[test]
fn decode_round_trips_utf8() {
    let b = blob("héllo".as_bytes().to_vec(), TRUSTED);
    let out = bytes_decode(&[b]).expect("decodes");
    match out {
        VmValue::Str(s) => assert_eq!(s.as_str(), "héllo"),
        v => panic!("expected Str, got {v:?}"),
    }
}

/// Reporting beats corrupting. A caller that assumed the bytes were text needs
/// to hear that they were not, rather than getting replacement characters.
///
/// The variant matters as much as the message. `JadeError::Exception` means a
/// `raise` the program wrote, and the VM answers one by handing the catch block
/// `state.raised_exception` — which a built-in never fills in. So while this
/// raised an `Exception`, a caught `bytes.decode()` bound the bare string
/// "unknown exception" under `jade run` and a `RuntimeError` struct under
/// `jade build`: `e.message` worked on one engine and failed on the other.
#[test]
fn decoding_invalid_utf8_reports_rather_than_substituting() {
    let b = blob(vec![b'o', b'k', 0xFF, 0xFE], TRUSTED);
    let err = bytes_decode(&[b]).expect_err("invalid UTF-8 must raise");
    match err {
        JadeError::TypeError { message, .. } => {
            assert!(message.contains("not valid UTF-8"), "got: {message}");
            assert!(message.contains('2'), "names where it failed: {message}");
        }
        e => panic!("expected TypeError, got {e:?}"),
    }
}

/// Nothing a built-in raises may be `JadeError::Exception`, for the reason
/// spelled out above: the VM reads the caught value out of `raised_exception`,
/// which only a real `raise` sets. A blob is the type this was found on, so the
/// guard lives here.
#[test]
fn no_bytes_method_raises_the_variant_reserved_for_a_program_raise() {
    let cases: Vec<Result<VmValue>> = vec![
        bytes_decode(&[blob(vec![0xFF], TRUSTED)]),
        bytes_decode(&[VmValue::Int(1)]),
        bytes_len(&[VmValue::Int(1)]),
        bytes_slice(&[VmValue::Int(1)]),
    ];
    for case in cases {
        if let Err(e) = case {
            assert!(
                !matches!(e, JadeError::Exception { .. }),
                "a built-in must not raise the variant reserved for `raise`: {e:?}"
            );
        }
    }
}

/// The hole this closes: without trust on the blob, `fs.read_bytes(p).decode()`
/// would produce a clean string and walk past the check `fs.read(p)` cannot.
#[test]
fn trust_survives_a_decode() {
    let b = blob(b"rm -rf /".to_vec(), TAINTED);
    match bytes_decode(&[b]).expect("decodes") {
        VmValue::Str(s) => assert!(s.is_tainted(), "decoded text must stay tainted"),
        v => panic!("expected Str, got {v:?}"),
    }
}

#[test]
fn trust_survives_an_encode() {
    let s = VmValue::Str(JStr::tainted("rm -rf /"));
    match (crate::string::find_str_method("encode").unwrap().vm_impl)(&[s]) {
        Ok(VmValue::Bytes(b)) => assert!(b.lock().is_tainted(), "encoded octets must stay tainted"),
        other => panic!("expected tainted Bytes, got {other:?}"),
    }
}

/// A NUL is data, not a terminator, and 0xFF is not valid UTF-8. Both are the
/// reason `bytes` is not modelled as a string.
#[test]
fn a_blob_round_trips_through_a_nul_and_invalid_utf8() {
    let raw = vec![b'a', 0, b'b', 0xFF];
    let b = blob(raw.clone(), TRUSTED);
    match bytes_len(std::slice::from_ref(&b)).expect("len") {
        VmValue::Int(n) => assert_eq!(n, 4, "a NUL must not shorten the blob"),
        v => panic!("expected Int, got {v:?}"),
    }
    match b {
        VmValue::Bytes(o) => assert_eq!(o.lock().as_slice(), &raw[..]),
        v => panic!("expected Bytes, got {v:?}"),
    }
}

#[test]
fn slice_clamps_rather_than_raising() {
    let b = blob(vec![1, 2, 3, 4], TRUSTED);
    match bytes_slice(&[b.clone(), VmValue::Int(2), VmValue::Int(99)]).expect("slice") {
        VmValue::Bytes(o) => assert_eq!(o.lock().as_slice(), &[3, 4]),
        v => panic!("expected Bytes, got {v:?}"),
    }
    // An inverted range is empty, not a panic.
    match bytes_slice(&[b, VmValue::Int(3), VmValue::Int(1)]).expect("slice") {
        VmValue::Bytes(o) => assert!(o.lock().is_empty()),
        v => panic!("expected Bytes, got {v:?}"),
    }
}

#[test]
fn slice_keeps_the_trust_of_its_source() {
    let b = blob(vec![1, 2, 3], TAINTED);
    match bytes_slice(&[b, VmValue::Int(0), VmValue::Int(2)]).expect("slice") {
        VmValue::Bytes(o) => assert!(o.lock().is_tainted(), "a slice of tainted data is tainted"),
        v => panic!("expected Bytes, got {v:?}"),
    }
}

// ── std::bytes ────────────────────────────────────────────────────────────────

fn ints(vals: &[i64]) -> VmValue {
    crate::builtins::make_array(vals.iter().map(|n| VmValue::Int(*n)).collect())
}

fn octets(v: &VmValue) -> Vec<u8> {
    match v {
        VmValue::Bytes(b) => b.lock().as_slice().to_vec(),
        other => panic!("expected bytes, got {other:?}"),
    }
}

#[test]
fn zeros_builds_a_trusted_buffer_of_the_asked_length() {
    let b = pkg_zeros(&[VmValue::Int(3)]).expect("three octets");
    assert_eq!(octets(&b), vec![0, 0, 0]);
    match &b {
        VmValue::Bytes(o) => assert!(!o.lock().is_tainted(), "the program wrote these itself"),
        v => panic!("expected bytes, got {v:?}"),
    }
    assert_eq!(octets(&pkg_zeros(&[VmValue::Int(0)]).expect("zero is a length")), Vec::<u8>::new());
    assert!(pkg_zeros(&[VmValue::Int(-1)]).is_err());
    assert!(pkg_zeros(&[VmValue::Str("3".into())]).is_err(), "a length is an int");
}

/// The whole reason the package exists: every octet below is out of reach of
/// `str.encode()`. A zero terminates a Jade string, and anything above 127
/// encodes as two octets rather than one.
#[test]
fn from_ints_builds_octets_a_string_could_not_carry() {
    let b = pkg_from_ints(&[ints(&[0, 255, 128, 10])]).expect("all octets");
    assert_eq!(octets(&b), vec![0, 255, 128, 10]);
    assert_eq!(octets(&pkg_from_ints(&[ints(&[])]).expect("empty")), Vec::<u8>::new());
}

#[test]
fn from_ints_names_the_element_it_refused() {
    let e = pkg_from_ints(&[ints(&[1, 300, 3])]).expect_err("300 is not an octet");
    let JadeError::TypeError { message, .. } = e else { panic!("expected a type error") };
    assert!(message.contains('1'), "names the position: {message}");
    assert!(message.contains("300"), "names the value: {message}");

    let bad = crate::builtins::make_array(vec![VmValue::Int(1), VmValue::Str("x".into())]);
    let e = pkg_from_ints(&[bad]).expect_err("a str is not an int");
    let JadeError::TypeError { message, .. } = e else { panic!("expected a type error") };
    assert!(message.contains('1'), "names the position: {message}");

    assert!(pkg_from_ints(&[VmValue::Int(5)]).is_err(), "the argument must be an array");
}

#[test]
fn concat_joins_two_blobs_and_leaves_both_alone() {
    let a = blob(vec![1, 2], TRUSTED);
    let b = blob(vec![3], TRUSTED);
    let out = pkg_concat(&[a.clone(), b.clone()]).expect("joins");
    assert_eq!(octets(&out), vec![1, 2, 3]);
    assert_eq!(octets(&a), vec![1, 2], "the left input is unchanged");
    assert_eq!(octets(&b), vec![3], "the right input is unchanged");
    assert!(pkg_concat(&[a, VmValue::Int(1)]).is_err(), "both arguments must be blobs");
}

/// Tainted on either side taints the result. The other choice would make
/// concatenation a laundering path past the check in `sh.exec`.
#[test]
fn concat_keeps_the_stricter_trust() {
    let clean = blob(vec![1], TRUSTED);
    let dirty = blob(vec![2], TAINTED);
    for pair in [[clean.clone(), dirty.clone()], [dirty.clone(), clean.clone()]] {
        match pkg_concat(&pair).expect("joins") {
            VmValue::Bytes(o) => assert!(o.lock().is_tainted(), "a tainted input taints the join"),
            v => panic!("expected bytes, got {v:?}"),
        }
    }
    match pkg_concat(&[clean.clone(), clean]).expect("joins") {
        VmValue::Bytes(o) => assert!(!o.lock().is_tainted(), "two clean inputs stay clean"),
        v => panic!("expected bytes, got {v:?}"),
    }
}

/// `bytes.concat(b, b)` is a legal program, and `parking_lot::Mutex` is not
/// reentrant: taking two guards on one blob would hang the process with no
/// panic and no message. This test hangs rather than fails if that returns.
#[test]
fn concat_of_a_blob_with_itself_does_not_deadlock() {
    let b = blob(vec![7, 8], TRUSTED);
    let out = pkg_concat(&[b.clone(), b]).expect("a blob may be joined to itself");
    assert_eq!(octets(&out), vec![7, 8, 7, 8]);
}

/// The deadlock the reentrant guard above did *not* cover: two threads joining
/// the same pair of blobs in opposite orders. Holding both guards at once takes
/// them in argument order, so each thread ends up waiting on what the other
/// holds, and `parking_lot` parks with no timeout and no message.
///
/// It hangs rather than fails if that returns, which is what the watchdog is
/// for: the assertion has to run on this thread, because a hung worker can
/// never report anything.
#[test]
fn two_threads_joining_one_pair_in_opposite_orders_do_not_deadlock() {
    use std::sync::mpsc;

    let a = blob(vec![1, 2], TRUSTED);
    let b = blob(vec![3, 4], TRUSTED);
    let (tx, rx) = mpsc::channel();

    for (l, r) in [(a.clone(), b.clone()), (b, a)] {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for _ in 0..20_000 {
                let _ = pkg_concat(&[l.clone(), r.clone()]);
            }
            let _ = tx.send(());
        });
    }
    drop(tx);

    for _ in 0..2 {
        rx.recv_timeout(std::time::Duration::from_secs(30))
            .expect("bytes.concat deadlocked: two threads took its two locks in argument order");
    }
}

/// Two blobs that each fit in the header's `u32` can add up to one that does
/// not, and the compiled backend reads `len()` off that header while the VM
/// reads the vector. Refusing is what keeps the two answering the same thing.
///
/// Checked through the length arithmetic rather than by allocating 4 GiB.
#[test]
fn concat_refuses_a_result_past_the_header_limit() {
    use jade_runtime::bytesf::{MAX_LEN, joined_len};
    assert!(joined_len(MAX_LEN, 1).is_err());
    assert_eq!(joined_len(2, 3).expect("small"), 5);
}

/// A blob is reference-semantic now, so two names for one buffer see one write.
/// Cloning a `VmValue::Bytes` is a refcount bump and never a copy.
#[test]
fn a_cloned_blob_shares_one_buffer() {
    let b = blob(vec![0, 0], TRUSTED);
    let alias = b.clone();
    let VmValue::Bytes(arc) = &b else { panic!("expected bytes") };
    jade_runtime::bytesf::set(&mut arc.lock(), 0, 9).expect("in range");
    assert_eq!(octets(&alias), vec![9, 0], "the alias sees the write");
}
