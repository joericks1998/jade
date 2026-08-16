use super::*;

fn get_int(v: VmValue) -> i64 {
    match v {
        VmValue::Int(n) => n,
        other => panic!("expected Int, got {:?}", other),
    }
}

#[test]
fn now_returns_positive_int() {
    let n = get_int(time_now(&[]).unwrap());
    // any time after 2020-01-01 (1577836800) proves the clock is sane
    assert!(n > 1_577_836_800, "unix seconds too small: {}", n);
}

#[test]
fn now_ms_returns_positive_int() {
    let n = get_int(time_now_ms(&[]).unwrap());
    assert!(n > 1_577_836_800_000, "unix millis too small: {}", n);
}

#[test]
fn now_ms_is_larger_than_now_seconds() {
    // millis value is ~1000x the seconds value; strictly greater in magnitude
    let secs = get_int(time_now(&[]).unwrap());
    let ms = get_int(time_now_ms(&[]).unwrap());
    assert!(ms > secs);
}

#[test]
fn now_is_non_decreasing() {
    let a = get_int(time_now_ms(&[]).unwrap());
    let b = get_int(time_now_ms(&[]).unwrap());
    assert!(b >= a, "time went backwards: {} then {}", a, b);
}

#[test]
fn now_arity_error() {
    let err = time_now(&[VmValue::Int(1)]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 0, got: 1, .. }));
}

#[test]
fn now_ms_arity_error() {
    let err = time_now_ms(&[VmValue::Int(1)]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 0, got: 1, .. }));
}

#[test]
fn sleep_zero_returns_nil() {
    // non-positive duration is a no-op; must return nil without blocking
    assert!(matches!(time_sleep(&[VmValue::Int(0)]).unwrap(), VmValue::Nil));
    assert!(matches!(time_sleep(&[VmValue::Float(-1.0)]).unwrap(), VmValue::Nil));
}

#[test]
fn sleep_tiny_duration_returns_nil() {
    // a millisecond-scale sleep just to exercise the positive branch
    let out = time_sleep(&[VmValue::Float(0.001)]).unwrap();
    assert!(matches!(out, VmValue::Nil));
}

#[test]
fn sleep_type_error() {
    let err = time_sleep(&[VmValue::Str("x".to_string().into())]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

#[test]
fn sleep_arity_error() {
    let err = time_sleep(&[]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 1, got: 0, .. }));
}

#[test]
fn local_type_error_on_non_str() {
    let err = time_local(&[VmValue::Int(1)]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

#[test]
fn local_arity_error() {
    let err = time_local(&[]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 1, got: 0, .. }));
}

#[test]
fn local_nil_tz_returns_nonempty_str() {
    // nil tz uses the system default; `date` should produce a non-empty line.
    match time_local(&[VmValue::Nil]).unwrap() {
        VmValue::Str(s) => assert!(!s.is_empty(), "date output was empty"),
        other => panic!("expected Str, got {:?}", other),
    }
}

// ── v1.3.24: monotonic, utc, parts, stamp ─────────────────────────────────────
//
// The calendar arithmetic itself is tested in `jade_runtime::timef`, against
// dates checked with `date -u`. What is tested here is the VM's wrapper: the
// value types it hands back, and the errors it raises on bad input.

fn get_str(v: VmValue) -> String {
    match v {
        VmValue::Str(s) => s.to_string(),
        other => panic!("expected Str, got {:?}", other),
    }
}

#[test]
fn monotonic_returns_float() {
    match time_monotonic(&[]).unwrap() {
        VmValue::Float(f) => assert!(f >= 0.0, "monotonic was negative: {}", f),
        other => panic!("expected Float, got {:?}", other),
    }
}

#[test]
fn monotonic_arity_error() {
    let err = time_monotonic(&[VmValue::Int(1)]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 0, got: 1, .. }));
}

#[test]
fn utc_formats_the_epoch() {
    assert_eq!(get_str(time_utc(&[VmValue::Int(0)]).unwrap()), "1970-01-01T00:00:00Z");
}

/// Unlike `time.local`, which is a subprocess's output, this is computed in
/// process from an int — so it must not carry taint into `sh`.
#[test]
fn utc_is_trusted() {
    match time_utc(&[VmValue::Int(0)]).unwrap() {
        VmValue::Str(s) => assert!(!s.is_tainted(), "time.utc must not be tainted"),
        other => panic!("expected Str, got {:?}", other),
    }
}

#[test]
fn utc_rejects_a_non_int() {
    let err = time_utc(&[VmValue::Str("now".to_string().into())]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

#[test]
fn parts_has_all_eight_fields() {
    match time_parts(&[VmValue::Int(1_786_889_002)]).unwrap() {
        VmValue::Dict(d) => {
            assert_eq!(d.len(), 8);
            assert!(matches!(d.get("year"), Some(VmValue::Int(2026))));
            assert!(matches!(d.get("month"), Some(VmValue::Int(8))));
            assert!(matches!(d.get("day"), Some(VmValue::Int(16))));
            assert!(matches!(d.get("weekday"), Some(VmValue::Int(0))));
            assert!(matches!(d.get("yearday"), Some(VmValue::Int(228))));
        }
        other => panic!("expected Dict, got {:?}", other),
    }
}

#[test]
fn parts_arity_error() {
    let err = time_parts(&[]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 1, got: 0, .. }));
}

#[test]
fn stamp_defaults_the_time_of_day_to_midnight() {
    let three =
        get_int(time_stamp(&[VmValue::Int(2026), VmValue::Int(8), VmValue::Int(16)]).unwrap());
    let six = get_int(
        time_stamp(&[
            VmValue::Int(2026),
            VmValue::Int(8),
            VmValue::Int(16),
            VmValue::Int(0),
            VmValue::Int(0),
            VmValue::Int(0),
        ])
        .unwrap(),
    );
    assert_eq!(three, six);
}

#[test]
fn stamp_round_trips_through_parts() {
    let ts = 1_786_889_002;
    match time_parts(&[VmValue::Int(ts)]).unwrap() {
        VmValue::Dict(d) => {
            let field = |k: &str| match d.get(k) {
                Some(VmValue::Int(n)) => VmValue::Int(*n),
                other => panic!("missing {}: {:?}", k, other),
            };
            let back = time_stamp(&[
                field("year"),
                field("month"),
                field("day"),
                field("hour"),
                field("minute"),
                field("second"),
            ])
            .unwrap();
            assert_eq!(get_int(back), ts);
        }
        other => panic!("expected Dict, got {:?}", other),
    }
}

#[test]
fn stamp_arity_errors_outside_three_to_six() {
    let too_few = time_stamp(&[VmValue::Int(2026), VmValue::Int(8)]).unwrap_err();
    assert!(matches!(too_few, JadeError::ArityMismatch { got: 2, .. }));
    let seven: Vec<VmValue> = (0..7).map(VmValue::Int).collect();
    let too_many = time_stamp(&seven).unwrap_err();
    assert!(matches!(too_many, JadeError::ArityMismatch { got: 7, .. }));
}

#[test]
fn stamp_rejects_a_non_int_field() {
    let err =
        time_stamp(&[VmValue::Int(2026), VmValue::Int(8), VmValue::Str("16".to_string().into())])
            .unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}
