/* sh/sh.h — std::sh runtime module ABI. exec/run are code-execution sinks and
 * refuse tainted input; all captured output is TAINTED. */
#ifndef JADE_RT_SH_H
#define JADE_RT_SH_H

#include <stdint.h>

/* sh.exec(cmd) — run via `sh -c`, return trimmed stdout as a TAINTED string. */
char*   jrt_sh_exec(const char* cmd);
/* sh.run(cmd) — run via `sh -c`, discard output, return the exit code (-1 on
 * signal/spawn failure). */
int64_t jrt_sh_run(const char* cmd);

#endif /* JADE_RT_SH_H */
