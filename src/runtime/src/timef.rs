//! `std::time` — the single implementation of the `time` stdlib, shared by both
//! engines. Neutral `pub fn` cores are called by the VM (`src/time/mod.rs`) and
//! by the AOT `#[no_mangle]` wrappers below. `local` shells to `date` (matching
//! the VM exactly, incl. not checking exit status), returning `Err` only on
//! spawn failure (the VM raises; the AOT wrapper falls back to "").
//!
//! There are two clocks here and they answer different questions. `now`/`now_ms`
//! read the *wall* clock, which is what you want for a timestamp and what you
//! must not use to measure a duration — NTP and a manual clock change both move
//! it, backwards included. `monotonic` never moves backwards and has no meaning
//! on its own; only the difference between two readings does.
//!
//! Calendar conversion (`parts`/`stamp`/`utc`) is plain integer arithmetic on
//! the proleptic Gregorian calendar, using Howard Hinnant's `civil_from_days`
//! algorithm. It is UTC-only and deliberately so: a local calendar needs the
//! IANA database, which is a dependency this crate does not carry. `local` is
//! the local-time answer, and it is a formatted string rather than fields.

use core::ffi::c_char;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::coll::DictObj;
use crate::cstr;
use crate::string::{TAINTED, TRUSTED};
use crate::value::JadeValue;

type W = i64;

/// Seconds in a day, the one magic number the calendar math leans on.
const DAY: i64 = 86_400;

// ── Neutral cores (used by both engines) ──────────────────────────────────────

/// Whole seconds since the Unix epoch.
pub fn now() -> i64 {
    since_epoch().as_secs() as i64
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    since_epoch().as_millis() as i64
}

/// Block for `secs` seconds (non-positive → no-op).
pub fn sleep(secs: f64) {
    if secs > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(secs));
    }
}

/// The current local time formatted `%a %b %e %H:%M:%S %Z %Y` in timezone `tz`
/// (empty → process default), via `date`. `Err` only if `date` cannot be spawned;
/// the exit status is ignored (matching the VM).
pub fn local(tz: &str) -> std::io::Result<String> {
    let mut cmd = std::process::Command::new("date");
    cmd.arg("+%a %b %e %H:%M:%S %Z %Y");
    if !tz.is_empty() {
        cmd.env("TZ", tz);
    }
    let out = cmd.output()?;
    Ok(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string())
}

/// Duration since the Unix epoch (0 if the clock is before it).
fn since_epoch() -> Duration {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO)
}

// ── Monotonic clock ───────────────────────────────────────────────────────────

/// The origin the first `monotonic()` call fixes. `Instant` has no public epoch,
/// so the process supplies one; every later reading is measured from it.
static ORIGIN: OnceLock<Instant> = OnceLock::new();

/// Seconds since an arbitrary fixed point in this process, as a float.
///
/// Unlike [`now`] this never jumps: it is the clock to subtract two readings of
/// when you want to know how long something took. The absolute value means
/// nothing and is not comparable across processes.
pub fn monotonic() -> f64 {
    ORIGIN.get_or_init(Instant::now).elapsed().as_secs_f64()
}

// ── Calendar conversion (UTC, proleptic Gregorian) ────────────────────────────

/// A timestamp broken into calendar fields, all UTC.
///
/// `weekday` is 0 = Sunday through 6 = Saturday, and `yearday` is 1 through 366
/// — the same two conventions `date +%w` and `date +%j` print, so a program can
/// move between [`local`] and these fields without learning a second numbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeParts {
    pub year: i64,
    pub month: i64,
    pub day: i64,
    pub hour: i64,
    pub minute: i64,
    pub second: i64,
    pub weekday: i64,
    pub yearday: i64,
}

impl TimeParts {
    /// The fields in dict order. Both engines build their dict from this, which
    /// is what keeps the key order identical rather than merely similar.
    pub fn fields(&self) -> [(&'static str, i64); 8] {
        [
            ("year", self.year),
            ("month", self.month),
            ("day", self.day),
            ("hour", self.hour),
            ("minute", self.minute),
            ("second", self.second),
            ("weekday", self.weekday),
            ("yearday", self.yearday),
        ]
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date, after Howard Hinnant.
///
/// The shift to a March-based year is what makes it branchless: February, the
/// only month whose length varies, lands at the *end* of the year, so the leap
/// day never sits in the middle of the run-length table `(153 * mp + 2) / 5`.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // year of era, [0, 399]
    let mp = (m + 9) % 12; // March = 0 … February = 11
    let doy = (153 * mp + 2) / 5 + d - 1; // day of that shifted year
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // day of era
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`]: `(year, month, day)` from a day count.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Break a Unix timestamp into UTC calendar fields.
///
/// Euclidean division rather than truncating division, so a timestamp before
/// 1970 lands on the right day instead of one day late.
pub fn parts(ts: i64) -> TimeParts {
    let days = ts.div_euclid(DAY);
    let secs = ts.rem_euclid(DAY);
    let (year, month, day) = civil_from_days(days);
    TimeParts {
        year,
        month,
        day,
        hour: secs / 3_600,
        minute: (secs / 60) % 60,
        second: secs % 60,
        // 1970-01-01 was a Thursday, so day 0 is weekday 4 counting from Sunday.
        weekday: (days + 4).rem_euclid(7),
        yearday: days - days_from_civil(year, 1, 1) + 1,
    }
}

/// A Unix timestamp from UTC calendar fields — the inverse of [`parts`].
///
/// Out-of-range fields carry rather than fail, which is what makes this useful
/// for arithmetic: month 13 is next January, day 0 is the last day of the
/// previous month, and hour 25 is tomorrow at 01:00. Saturating arithmetic
/// keeps an absurd year from panicking; no reachable date comes near it.
pub fn stamp(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> i64 {
    // Only the month needs normalizing by hand. It indexes a table, so it has to
    // be in [1, 12] before the lookup; every other field is summed, and a sum
    // carries on its own.
    let m0 = mo - 1;
    let year = y.saturating_add(m0.div_euclid(12));
    let month = m0.rem_euclid(12) + 1;
    days_from_civil(year, month, d)
        .saturating_mul(DAY)
        .saturating_add(h.saturating_mul(3_600))
        .saturating_add(mi.saturating_mul(60))
        .saturating_add(s)
}

/// A Unix timestamp as an ISO 8601 UTC string, e.g. `2026-08-16T14:03:22Z`.
///
/// Fixed width and sortable as text, which is the point of choosing it over
/// [`local`]'s human format.
pub fn utc(ts: i64) -> String {
    let p = parts(ts);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        p.year, p.month, p.day, p.hour, p.minute, p.second
    )
}

/// `time.now()` — whole seconds since the Unix epoch.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_time_now() -> i64 {
    now()
}

/// `time.now_ms()` — milliseconds since the Unix epoch.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_time_now_ms() -> i64 {
    now_ms()
}

/// `time.sleep(secs)` — block for `secs` seconds (non-positive → no-op).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_time_sleep(secs: f64) {
    sleep(secs);
}

/// `time.local(tz)` — formatted local time (TAINTED); empty string on a `date`
/// spawn failure (the AOT has no raise path here).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_time_local(tz: *const c_char) -> *mut c_char {
    let s = local(unsafe { cstr::borrow(tz) }).unwrap_or_default();
    cstr::emit(s.as_bytes(), TAINTED)
}

/// `time.monotonic()` — seconds from this process's fixed origin.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_time_monotonic() -> f64 {
    monotonic()
}

/// `time.utc(ts)` — ISO 8601 UTC.
///
/// TRUSTED, unlike [`jrt_time_local`]: this is computed from an integer in
/// process, where `local` is the output of a subprocess.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_time_utc(ts: i64) -> *mut c_char {
    cstr::emit(utc(ts).as_bytes(), TRUSTED)
}

/// `time.stamp(y, mo, d, h, mi, s)` — UTC fields to a Unix timestamp.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_time_stamp(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> i64 {
    stamp(y, mo, d, h, mi, s)
}

/// `time.parts(ts)` — an already-tagged dict word of UTC calendar fields.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_time_parts(ts: i64) -> W {
    let mut d = DictObj::<W>::new();
    for (k, v) in parts(ts).fields() {
        d.insert(k, JadeValue::from_int(v).bits() as i64);
    }
    JadeValue::from_ptr(crate::gc::leak_obj(d) as *const core::ffi::c_void as *const ()).bits()
        as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every fixed date here was checked against `date -u` before being written
    /// down, so this pins the algorithm to the system calendar rather than to
    /// itself.
    #[test]
    fn known_timestamps_convert() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(1_786_889_002), "2026-08-16T14:03:22Z");
        assert_eq!(utc(1_000_000_000), "2001-09-09T01:46:40Z");
    }

    #[test]
    fn parts_matches_the_string() {
        let p = parts(1_786_889_002);
        assert_eq!((p.year, p.month, p.day), (2026, 8, 16));
        assert_eq!((p.hour, p.minute, p.second), (14, 3, 22));
        assert_eq!(p.weekday, 0, "2026-08-16 is a Sunday");
        assert_eq!(p.yearday, 228);
    }

    #[test]
    fn epoch_weekday_is_thursday() {
        assert_eq!(parts(0).weekday, 4);
        assert_eq!(parts(0).yearday, 1);
    }

    /// A negative timestamp has to floor rather than truncate, or it lands a day
    /// late with a negative time of day.
    #[test]
    fn before_the_epoch_floors() {
        assert_eq!(utc(-1), "1969-12-31T23:59:59Z");
        assert_eq!(utc(-DAY), "1969-12-31T00:00:00Z");
        let p = parts(-1);
        assert_eq!((p.hour, p.minute, p.second), (23, 59, 59));
        assert_eq!(p.weekday, 3, "1969-12-31 is a Wednesday");
    }

    #[test]
    fn leap_years_follow_the_gregorian_rule() {
        assert_eq!(utc(stamp(2024, 2, 29, 0, 0, 0)), "2024-02-29T00:00:00Z");
        // Divisible by 100 but not 400 — not a leap year, so the 29th rolls over.
        assert_eq!(utc(stamp(2100, 2, 29, 0, 0, 0)), "2100-03-01T00:00:00Z");
        // Divisible by 400 — a leap year after all.
        assert_eq!(utc(stamp(2000, 2, 29, 0, 0, 0)), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn out_of_range_fields_carry() {
        assert_eq!(utc(stamp(2026, 13, 1, 0, 0, 0)), "2027-01-01T00:00:00Z");
        assert_eq!(utc(stamp(2026, 0, 1, 0, 0, 0)), "2025-12-01T00:00:00Z");
        assert_eq!(utc(stamp(2026, 3, 0, 0, 0, 0)), "2026-02-28T00:00:00Z");
        assert_eq!(utc(stamp(2026, 8, 16, 25, 0, 0)), "2026-08-17T01:00:00Z");
        assert_eq!(utc(stamp(2026, 8, 16, 0, 0, -1)), "2026-08-15T23:59:59Z");
    }

    /// `stamp` and `parts` are inverses, checked across four centuries and both
    /// sides of the epoch rather than on one lucky date.
    #[test]
    fn stamp_and_parts_round_trip() {
        let mut ts = -60_000_000_000; // ~1067 CE
        while ts < 60_000_000_000 {
            let p = parts(ts);
            assert_eq!(
                stamp(p.year, p.month, p.day, p.hour, p.minute, p.second),
                ts,
                "round trip failed at {}",
                ts
            );
            ts += 987_654_321;
        }
    }

    /// Consecutive days must advance the weekday by exactly one, with no gap at
    /// a month or year boundary.
    #[test]
    fn weekday_advances_one_day_at_a_time() {
        let start = stamp(2023, 11, 20, 0, 0, 0);
        for i in 0..800 {
            let expected = (parts(start).weekday + i).rem_euclid(7);
            assert_eq!(parts(start + i * DAY).weekday, expected, "day {}", i);
        }
    }

    /// The monotonic clock is what a duration is measured with, so the one thing
    /// it must never do is go backwards.
    #[test]
    fn monotonic_never_decreases() {
        let a = monotonic();
        sleep(0.001);
        let b = monotonic();
        assert!(b >= a, "monotonic went backwards: {} then {}", a, b);
        assert!(b - a < 60.0, "implausible gap: {}", b - a);
    }
}
