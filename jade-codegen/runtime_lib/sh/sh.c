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

/* sh.output(cmd) — run via `sh -c`, capture stdout and stderr SEPARATELY,
 * return a dict {stdout, stderr, code}. Mirrors the VM's Command::output():
 * never errors on non-zero exit, no newline stripping. Both captured streams are
 * TAINTED. We fork/exec instead of popen so the two streams stay distinct, and
 * poll() both pipes to avoid pipe-buffer deadlock. */
void* jrt_sh_output(const char* cmd) {
    void* dict = jade_dict_create();
    if (!cmd) {
        jade_dict_set(dict, "stdout", jrt_box_str(jrt_str_dup("", JRT_TAINTED)));
        jade_dict_set(dict, "stderr", jrt_box_str(jrt_str_dup("", JRT_TAINTED)));
        jade_dict_set(dict, "code",   jrt_box_int(-1));
        return dict;
    }

    int op[2], ep[2];
    if (pipe(op) != 0) { op[0] = op[1] = -1; }
    if (pipe(ep) != 0) { ep[0] = ep[1] = -1; }

    pid_t pid = fork();
    if (pid == 0) {
        /* child */
        if (op[1] >= 0) { dup2(op[1], STDOUT_FILENO); }
        if (ep[1] >= 0) { dup2(ep[1], STDERR_FILENO); }
        if (op[0] >= 0) close(op[0]);
        if (op[1] >= 0) close(op[1]);
        if (ep[0] >= 0) close(ep[0]);
        if (ep[1] >= 0) close(ep[1]);
        execl("/bin/sh", "sh", "-c", cmd, (char*)NULL);
        _exit(127);
    }

    /* parent */
    if (op[1] >= 0) close(op[1]);
    if (ep[1] >= 0) close(ep[1]);

    size_t ocap = 1024, olen = 0, eclen = 0, ecap = 1024;
    char* obuf = malloc(ocap);
    char* ebuf = malloc(ecap);
    if (!obuf || !ebuf) jade_rt_fatal("jade: out of memory");
    int ofd = op[0], efd = ep[0];

    while (ofd >= 0 || efd >= 0) {
        struct pollfd pfds[2];
        int n = 0;
        int oidx = -1, eidx = -1;
        if (ofd >= 0) { pfds[n].fd = ofd; pfds[n].events = POLLIN; oidx = n; n++; }
        if (efd >= 0) { pfds[n].fd = efd; pfds[n].events = POLLIN; eidx = n; n++; }
        if (n == 0) break;
        if (poll(pfds, n, -1) < 0) { if (errno == EINTR) continue; break; }

        if (oidx >= 0 && (pfds[oidx].revents & (POLLIN | POLLHUP))) {
            if (olen + 1 >= ocap) {
                size_t ncap = ocap * 2; char* nb = realloc(obuf, ncap);
                if (!nb) jade_rt_fatal("jade: out of memory");
                obuf = nb; ocap = ncap;
            }
            ssize_t r = read(ofd, obuf + olen, ocap - olen - 1);
            if (r > 0) olen += (size_t)r;
            else { close(ofd); ofd = -1; }
        }
        if (eidx >= 0 && (pfds[eidx].revents & (POLLIN | POLLHUP))) {
            if (eclen + 1 >= ecap) {
                size_t ncap = ecap * 2; char* nb = realloc(ebuf, ncap);
                if (!nb) jade_rt_fatal("jade: out of memory");
                ebuf = nb; ecap = ncap;
            }
            ssize_t r = read(efd, ebuf + eclen, ecap - eclen - 1);
            if (r > 0) eclen += (size_t)r;
            else { close(efd); efd = -1; }
        }
    }

    int status = 0;
    waitpid(pid, &status, 0);
    int64_t code = WIFEXITED(status) ? (int64_t)WEXITSTATUS(status)
                 : (WIFSIGNALED(status) ? (int64_t)(128 + WTERMSIG(status)) : (int64_t)-1);

    char* ostr = jrt_str_new(olen, JRT_TAINTED);
    if (olen > 0 && obuf) memcpy(ostr, obuf, olen);
    char* estr = jrt_str_new(eclen, JRT_TAINTED);
    if (eclen > 0 && ebuf) memcpy(estr, ebuf, eclen);
    free(obuf); free(ebuf);

    jade_dict_set(dict, "stdout", jrt_box_str(ostr));
    jade_dict_set(dict, "stderr", jrt_box_str(estr));
    jade_dict_set(dict, "code",   jrt_box_int(code));
    return dict;
}
