/* random/random.h — std::random runtime module ABI.
 *
 * The generated sequence does NOT match the VM's StdRng (different algorithm) —
 * an intentional divergence, like time.*: values differ but distributions and
 * invariants (range, length, membership) hold. seed() makes a run reproducible
 * within a single execution path. */
#ifndef JADE_RT_RANDOM_H
#define JADE_RT_RANDOM_H

#include <stdint.h>

/* random.int(lo, hi) — uniform integer in [lo, hi] inclusive. Raises if lo>hi. */
int64_t jrt_random_int(int64_t lo, int64_t hi);
/* random.float() — uniform double in [0.0, 1.0). */
double  jrt_random_float(void);
/* random.choice(arr) — a random element (tagged). Raises on an empty array. */
int64_t jrt_random_choice(void* arr);
/* random.shuffle(arr) — Fisher-Yates in place. Returns Nil. */
void    jrt_random_shuffle(void* arr);
/* random.seed(n) — reseed the generator. Returns Nil. */
void    jrt_random_seed(int64_t n);

#endif /* JADE_RT_RANDOM_H */
