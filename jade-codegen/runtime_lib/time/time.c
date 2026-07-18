/* time/ — std::time runtime module.
 *
 * now/now_ms/sleep plus a timezone-aware local(). Uses only libc and the public
 * jade ABI (runtime.h); same file links into both runtime backends. The VM
 * (jadelang/src/compiler/builtins/time_pkg.rs) is the behavioural source of truth. */

#include "runtime.h"
#include "time/time.h"

#include <stdlib.h>
#include <string.h>
#include <time.h>

/* time.now() — whole seconds since the Unix epoch. Mirrors the VM. */
int64_t jrt_time_now(void) { return (int64_t)time(NULL); }

/* time.now_ms() — milliseconds since the Unix epoch. Mirrors the VM. */
int64_t jrt_time_now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return (int64_t)ts.tv_sec * 1000 + (int64_t)ts.tv_nsec / 1000000;
}

/* time.sleep(secs) — block for `secs` seconds (fractional supported, matching
 * the VM's Duration::from_secs_f64). Non-positive durations are a no-op. */
void jrt_time_sleep(double secs) {
    if (secs <= 0.0) return;
    struct timespec ts;
    ts.tv_sec  = (time_t)secs;
    ts.tv_nsec = (long)((secs - (double)ts.tv_sec) * 1e9);
    nanosleep(&ts, NULL);
}

/* setenv("TZ", ...)/tzset() mutates a process-wide global; serialize so
 * concurrent jrt_time_local calls (or other libc time consumers) don't
 * race on the saved/restored value. A portable atomic spinlock keeps the
 * shared core free of any pthread dependency (the backends own threading). */
static volatile int tz_lock = 0;

char* jrt_time_local(const char* tz) {
    while (__sync_lock_test_and_set(&tz_lock, 1)) { /* spin */ }

    char* prev_tz = NULL;
    const char* old = getenv("TZ");
    if (old) prev_tz = strdup(old);

    if (tz && *tz) {
        setenv("TZ", tz, 1);
    } else {
        unsetenv("TZ");
    }
    tzset();

    time_t now = time(NULL);
    struct tm tm_buf;
    localtime_r(&now, &tm_buf);
    char buf[128];
    size_t n = strftime(buf, sizeof(buf), "%a %b %e %H:%M:%S %Z %Y", &tm_buf);

    if (prev_tz) {
        setenv("TZ", prev_tz, 1);
        free(prev_tz);
    } else {
        unsetenv("TZ");
    }
    tzset();

    __sync_lock_release(&tz_lock);

    char* out = jrt_str_new(n, JRT_TAINTED);
    if (n > 0) memcpy(out, buf, n);
    return out;
}
