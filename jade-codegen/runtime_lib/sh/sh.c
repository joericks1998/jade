/* sh/ — std::sh runtime module.
 *
 * exec/run/output. exec and run are code-execution sinks and refuse tainted
 * input; all captured output is TAINTED. Uses only libc + the public jade ABI.
 * The VM's sh_pkg.rs is the source of truth. */

#include "runtime.h"
#include "sh/sh.h"

#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <unistd.h>
#include <errno.h>
#include <poll.h>
#include <sys/wait.h>

char* jrt_sh_exec(const char* cmd) {
    jrt_refuse_if_tainted(cmd, "sh.exec(cmd)");
    if (!cmd) return jrt_str_dup("", JRT_TAINTED);
    FILE* f = popen(cmd, "r");
    if (!f) return jrt_str_dup("", JRT_TAINTED);
    size_t cap = 1024, len = 0;
    char* buf = malloc(cap);
    if (!buf) { pclose(f); return jrt_str_dup("", JRT_TAINTED); }
    size_t nr;
    while ((nr = fread(buf + len, 1, cap - len - 1, f)) > 0) {
        len += nr;
        if (len + 1 >= cap) { cap *= 2; char* nb = realloc(buf, cap); if (!nb) break; buf = nb; }
    }
    pclose(f);
    while (len > 0 && (buf[len-1] == '\n' || buf[len-1] == '\r')) len--;
    char* tagged = jrt_str_new(len, JRT_TAINTED);
    if (len > 0) memcpy(tagged, buf, len);
    free(buf);
    return tagged;
}

/* sh.run(cmd) — run via `sh -c`, discard output, return the exit code (or -1 if
 * the process was killed by a signal or could not be spawned). Mirrors the VM's
 * sh.run (status.code().unwrap_or(-1)). */
int64_t jrt_sh_run(const char* cmd) {
    jrt_refuse_if_tainted(cmd, "sh.run(cmd)");
    if (!cmd) return -1;
    int rc = system(cmd);
    if (rc == -1) return -1;
    return WIFEXITED(rc) ? (int64_t)WEXITSTATUS(rc) : -1;
}
