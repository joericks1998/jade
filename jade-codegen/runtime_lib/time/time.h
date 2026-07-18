/* time/time.h — std::time runtime module ABI. */
#ifndef JADE_RT_TIME_H
#define JADE_RT_TIME_H

#include <stdint.h>

/* time.now() — whole seconds since the Unix epoch. */
int64_t jrt_time_now(void);
/* time.now_ms() — milliseconds since the Unix epoch. */
int64_t jrt_time_now_ms(void);
/* time.sleep(secs) — block for `secs` seconds (fractional ok; <=0 is a no-op). */
void    jrt_time_sleep(double secs);
/* time.local(tz) — formatted local time in an IANA timezone (NULL/"" = system).
 * Heap-allocated TAINTED string. */
char*   jrt_time_local(const char* tz);

#endif /* JADE_RT_TIME_H */
