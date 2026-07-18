/* random/ — std::random runtime module.
 *
 * A self-contained xoshiro256** generator (splitmix64-seeded), serialized with a
 * portable atomic spinlock so concurrent (async) callers don't corrupt state.
 * Uses only libc + the public jade ABI. The VM's random_pkg.rs is the source of
 * truth for the API and invariants; the exact value sequence differs (the VM
 * uses rand::StdRng), an intentional divergence like time.*. */

#include "runtime.h"
#include "random/random.h"

#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <time.h>

static uint64_t       rng_state[4];
static int            rng_seeded = 0;
static volatile int   rng_lock   = 0;

static uint64_t splitmix64(uint64_t* x) {
    uint64_t z = (*x += 0x9E3779B97F4A7C15ULL);
    z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ULL;
    z = (z ^ (z >> 27)) * 0x94D049BB133111EBULL;
    return z ^ (z >> 31);
}

static void rng_reseed(uint64_t seed) {
    uint64_t s = seed;
    for (int i = 0; i < 4; i++) rng_state[i] = splitmix64(&s);
    /* Guard against the all-zero state (degenerate for xoshiro). */
    if (!(rng_state[0] | rng_state[1] | rng_state[2] | rng_state[3]))
        rng_state[0] = 0x9E3779B97F4A7C15ULL;
    rng_seeded = 1;
}

static inline uint64_t rotl(uint64_t x, int k) { return (x << k) | (x >> (64 - k)); }

/* One 64-bit draw (xoshiro256**). Caller must hold rng_lock. */
static uint64_t rng_next_locked(void) {
    if (!rng_seeded)
        rng_reseed((uint64_t)time(NULL) ^ ((uint64_t)getpid() << 16));
    uint64_t* s = rng_state;
    uint64_t result = rotl(s[1] * 5, 7) * 9;
    uint64_t t = s[1] << 17;
    s[2] ^= s[0]; s[3] ^= s[1]; s[1] ^= s[2]; s[0] ^= s[3];
    s[2] ^= t;    s[3] = rotl(s[3], 45);
    return result;
}

/* Uniform value in [0, bound) with rejection sampling (no modulo bias).
 * bound == 0 means "no bound" → a raw 64-bit draw. Holds rng_lock internally. */
static uint64_t rng_bounded(uint64_t bound) {
    while (__sync_lock_test_and_set(&rng_lock, 1)) { /* spin */ }
    uint64_t r;
    if (bound == 0) {
        r = rng_next_locked();
    } else {
        uint64_t limit = UINT64_MAX - (UINT64_MAX % bound);
        do { r = rng_next_locked(); } while (r >= limit);
        r %= bound;
    }
    __sync_lock_release(&rng_lock);
    return r;
}

int64_t jrt_random_int(int64_t lo, int64_t hi) {
    if (lo > hi) {
        char m[96];
        snprintf(m, sizeof m, "random.int: min (%lld) > max (%lld)",
                 (long long)lo, (long long)hi);
        jade_exc_throw_typed(jrt_box_str(jrt_str_dup(m, JRT_TRUSTED)), NULL);
        return 0; /* unreachable */
    }
    uint64_t range = (uint64_t)hi - (uint64_t)lo + 1ULL; /* 0 ⇒ full 64-bit span */
    return lo + (int64_t)rng_bounded(range);
}

double jrt_random_float(void) {
    uint64_t r = rng_bounded(0) >> 11;            /* 53 random bits */
    return (double)r / (double)(1ULL << 53);      /* [0.0, 1.0) */
}

void jrt_random_seed(int64_t n) {
    while (__sync_lock_test_and_set(&rng_lock, 1)) { /* spin */ }
    rng_reseed((uint64_t)n);
    __sync_lock_release(&rng_lock);
}
