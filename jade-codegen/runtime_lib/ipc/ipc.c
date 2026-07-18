/* ipc.c — single owner of the jade-tree socket fd.
 *
 * The connection is opened lazily on first request via pthread_once.
 * All I/O is serialized through g_mtx. Any transport failure exits the
 * program with a stderr message — no silent reconnection, no partial
 * responses. The persistent fd avoids the connect/handshake cost of
 * the previous "open a new socket per ?p" design (typically ~3 syscalls
 * per pipe stage in a chain like `data |> f |> g |> h`). */

#include "ipc.h"

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#define JRT_RESP_INIT_CAP 4096
/* Ceilings on what the (untrusted) inference daemon may send for one request,
 * so a malicious/buggy daemon can't OOM or hang the process by streaming
 * forever. */
#define JRT_RESP_MAX_BYTES ((size_t)64 << 20)  /* 64 MiB of accumulated tokens */
#define JRT_MAX_FRAMES     ((uint64_t)4000000) /* frame-count ceiling per request */

static int             g_fd        = -1;
static pthread_mutex_t g_mtx       = PTHREAD_MUTEX_INITIALIZER;
static pthread_once_t  g_init_once = PTHREAD_ONCE_INIT;

static void fatal(const char* msg) {
    fprintf(stderr, "jade: %s\n", msg);
    exit(1);
}

static void fatal_errno(const char* msg) {
    fprintf(stderr, "jade: %s: %s\n", msg, strerror(errno));
    exit(1);
}

static int sock_path(char* buf, size_t bufsz) {
    const char* home = getenv("HOME");
    if (!home || !*home) home = "/root";
    int n = snprintf(buf, bufsz, "%s/.jade/llm.sock", home);
    return (n > 0 && (size_t)n < bufsz) ? 0 : -1;
}

static int write_all(int fd, const void* buf, size_t n) {
    const char* p = (const char*)buf;
    while (n > 0) {
        ssize_t w = write(fd, p, n);
        if (w < 0) { if (errno == EINTR) continue; return -1; }
        p += (size_t)w; n -= (size_t)w;
    }
    return 0;
}

static int read_exact(int fd, void* buf, size_t n) {
    char* p = (char*)buf;
    while (n > 0) {
        ssize_t r = read(fd, p, n);
        if (r < 0) { if (errno == EINTR) continue; return -1; }
        if (r == 0) return -1;
        p += (size_t)r; n -= (size_t)r;
    }
    return 0;
}

static void init_connection(void) {
    char path[256];
    if (sock_path(path, sizeof(path)) < 0) {
        fatal("HOME path too long to derive jade socket location");
    }
    /* sockaddr_un.sun_path is typically 108 bytes — reject early if our
     * resolved path won't fit, rather than silently truncating. */
    size_t path_len = strlen(path);
    struct sockaddr_un probe;
    if (path_len >= sizeof(probe.sun_path)) {
        fatal("HOME-derived jade socket path exceeds sockaddr_un.sun_path");
    }

    /* Three retries with 100ms backoff to tolerate a brief daemon-startup window. */
    int fd = -1;
    for (int retry = 0; retry < 3; retry++) {
        struct sockaddr_un addr;
        memset(&addr, 0, sizeof(addr));
        addr.sun_family = AF_UNIX;
        memcpy(addr.sun_path, path, path_len + 1);
        int s = socket(AF_UNIX, SOCK_STREAM, 0);
        if (s < 0) fatal_errno("socket()");
        if (connect(s, (struct sockaddr*)&addr, sizeof(addr)) == 0) {
            fd = s;
            break;
        }
        close(s);
        if (retry < 2) usleep(100000);
    }
    if (fd < 0) {
        fprintf(stderr,
            "jade: cannot connect to inference daemon at %s — is jade-tree running?\n",
            path);
        exit(1);
    }
    g_fd = fd;
    atexit(jrt_ipc_shutdown);
}

void jrt_ipc_shutdown(void) {
    pthread_mutex_lock(&g_mtx);
    if (g_fd >= 0) {
        close(g_fd);
        g_fd = -1;
    }
    pthread_mutex_unlock(&g_mtx);
}

/* json_mode selects which frame body accumulates into the response buffer:
 *   0 → TOKEN (0x01) frames accumulate and drive on_token; JSON (0x05) ignored.
 *   1 → JSON  (0x05) frames accumulate; TOKEN frames ignored (no on_token).
 * This mirrors jadelang's jade_os.rs, where token-streaming ops drain 0x05 and
 * the health op accumulates only 0x05. */
static void do_request(const void* req_json, size_t req_len,
                       jrt_token_cb on_token, void* user,
                       char** resp_out, size_t* resp_len_out,
                       uint64_t* tokens_used_out, int json_mode) {
    pthread_once(&g_init_once, init_connection);

    pthread_mutex_lock(&g_mtx);

    uint8_t hdr[4] = {
        (uint8_t)(req_len & 0xFF),
        (uint8_t)((req_len >> 8) & 0xFF),
        (uint8_t)((req_len >> 16) & 0xFF),
        (uint8_t)((req_len >> 24) & 0xFF),
    };
    if (write_all(g_fd, hdr, 4) < 0 || write_all(g_fd, req_json, req_len) < 0) {
        pthread_mutex_unlock(&g_mtx);
        fatal_errno("write to jade-tree failed (daemon may have exited)");
    }

    size_t rcap = JRT_RESP_INIT_CAP, rlen = 0;
    char*  rbuf = malloc(rcap);
    if (!rbuf) { pthread_mutex_unlock(&g_mtx); fatal("out of memory allocating response buffer"); }

    uint64_t tokens_used = 0;
    int saw_done = 0;
    uint64_t frames = 0;

    while (!saw_done) {
        /* Frame-count ceiling: bound a daemon that streams frames forever. */
        if (++frames > JRT_MAX_FRAMES) {
            free(rbuf);
            pthread_mutex_unlock(&g_mtx);
            fatal("jade-tree response exceeded frame limit");
        }
        uint8_t ftype;
        if (read_exact(g_fd, &ftype, 1) < 0) {
            free(rbuf);
            pthread_mutex_unlock(&g_mtx);
            fatal_errno("read from jade-tree failed (connection lost)");
        }
        uint8_t lb[2];
        if (read_exact(g_fd, lb, 2) < 0) {
            free(rbuf);
            pthread_mutex_unlock(&g_mtx);
            fatal_errno("read from jade-tree failed (truncated frame header)");
        }
        uint16_t plen = (uint16_t)(lb[0] | (lb[1] << 8));

        /* TOKEN (token streams) and JSON (structured ops) share the same
         * accumulation path; json_mode selects which one fills rbuf. The other
         * is drained and discarded so a stray frame never desyncs the stream. */
        int accumulate = (ftype == JRT_FRAME_TOKEN && !json_mode)
                      || (ftype == JRT_FRAME_JSON  &&  json_mode);
        int discard    = (ftype == JRT_FRAME_TOKEN &&  json_mode)
                      || (ftype == JRT_FRAME_JSON  && !json_mode);

        switch (ftype) {
        case JRT_FRAME_TOKEN:
        case JRT_FRAME_JSON: {
            if (discard) {
                /* Drain the payload of the frame type we don't consume. */
                size_t remain = plen;
                while (remain > 0) {
                    char drop[256];
                    size_t d = remain < sizeof(drop) ? remain : sizeof(drop);
                    if (read_exact(g_fd, drop, d) < 0) {
                        free(rbuf);
                        pthread_mutex_unlock(&g_mtx);
                        fatal_errno("read from jade-tree failed (truncated frame)");
                    }
                    remain -= d;
                }
                break;
            }
            (void)accumulate;
            /* Byte ceiling: bound total accumulated bytes. */
            if (rlen + (size_t)plen + 1 > JRT_RESP_MAX_BYTES) {
                free(rbuf);
                pthread_mutex_unlock(&g_mtx);
                fatal("jade-tree response exceeded size limit");
            }
            while (rlen + plen + 1 > rcap) {
                rcap *= 2;
                char* nb = realloc(rbuf, rcap);
                if (!nb) { free(rbuf); pthread_mutex_unlock(&g_mtx); fatal("out of memory"); }
                rbuf = nb;
            }
            if (read_exact(g_fd, rbuf + rlen, plen) < 0) {
                free(rbuf);
                pthread_mutex_unlock(&g_mtx);
                fatal_errno("read from jade-tree failed (truncated token frame)");
            }
            /* on_token only fires for TOKEN frames in token mode. */
            if (ftype == JRT_FRAME_TOKEN && on_token && plen > 0) on_token(rbuf + rlen, plen, user);
            rlen += plen;
            break;
        }
        case JRT_FRAME_DONE: {
            /* DONE payload: optional 8-byte LE token count. */
            uint8_t tb[8];
            size_t to_read = plen < 8 ? plen : 8;
            if (to_read > 0) {
                if (read_exact(g_fd, tb, to_read) < 0) {
                    free(rbuf);
                    pthread_mutex_unlock(&g_mtx);
                    fatal_errno("read from jade-tree failed (truncated DONE payload)");
                }
                if (to_read == 8) {
                    tokens_used = (uint64_t)tb[0]
                        | ((uint64_t)tb[1] << 8)
                        | ((uint64_t)tb[2] << 16)
                        | ((uint64_t)tb[3] << 24)
                        | ((uint64_t)tb[4] << 32)
                        | ((uint64_t)tb[5] << 40)
                        | ((uint64_t)tb[6] << 48)
                        | ((uint64_t)tb[7] << 56);
                }
                /* Discard any extra bytes beyond 8. */
                size_t remain = plen - to_read;
                while (remain > 0) {
                    char drop[64];
                    size_t take = remain < sizeof(drop) ? remain : sizeof(drop);
                    if (read_exact(g_fd, drop, take) < 0) {
                        free(rbuf);
                        pthread_mutex_unlock(&g_mtx);
                        fatal_errno("read from jade-tree failed (discarding DONE tail)");
                    }
                    remain -= take;
                }
            }
            saw_done = 1;
            break;
        }
        case JRT_FRAME_ERROR: {
            /* Read the error message and exit. */
            char errbuf[513];
            size_t take = plen < sizeof(errbuf) - 1 ? plen : sizeof(errbuf) - 1;
            if (take > 0 && read_exact(g_fd, errbuf, take) < 0) {
                free(rbuf);
                pthread_mutex_unlock(&g_mtx);
                fatal_errno("read from jade-tree failed (truncated ERROR frame)");
            }
            errbuf[take] = '\0';
            /* Drain any remaining payload bytes. */
            size_t remain = plen - take;
            while (remain > 0) {
                char drop[64];
                size_t d = remain < sizeof(drop) ? remain : sizeof(drop);
                if (read_exact(g_fd, drop, d) < 0) break;
                remain -= d;
            }
            free(rbuf);
            pthread_mutex_unlock(&g_mtx);
            fprintf(stderr, "jade: inference error from jade-tree: %s\n", errbuf);
            exit(1);
        }
        case JRT_FRAME_META: {
            /* Informational; discard payload. */
            size_t remain = plen;
            while (remain > 0) {
                char drop[256];
                size_t d = remain < sizeof(drop) ? remain : sizeof(drop);
                if (read_exact(g_fd, drop, d) < 0) {
                    free(rbuf);
                    pthread_mutex_unlock(&g_mtx);
                    fatal_errno("read from jade-tree failed (truncated META frame)");
                }
                remain -= d;
            }
            break;
        }
        default: {
            /* Unknown frame type — drain and exit. */
            size_t remain = plen;
            while (remain > 0) {
                char drop[256];
                size_t d = remain < sizeof(drop) ? remain : sizeof(drop);
                if (read_exact(g_fd, drop, d) < 0) break;
                remain -= d;
            }
            free(rbuf);
            pthread_mutex_unlock(&g_mtx);
            fprintf(stderr, "jade: unknown frame type 0x%02x from jade-tree\n", ftype);
            exit(1);
        }
        }
    }

    pthread_mutex_unlock(&g_mtx);

    rbuf[rlen] = '\0';
    *resp_out = rbuf;
    if (resp_len_out) *resp_len_out = rlen;
    if (tokens_used_out) *tokens_used_out = tokens_used;
}

void jrt_ipc_request(const void* req_json, size_t req_len,
                     char** resp_out, size_t* resp_len_out,
                     uint64_t* tokens_used_out) {
    do_request(req_json, req_len, NULL, NULL, resp_out, resp_len_out, tokens_used_out, 0);
}

void jrt_ipc_request_streaming(const void* req_json, size_t req_len,
                               jrt_token_cb on_token, void* user,
                               char** resp_out, size_t* resp_len_out,
                               uint64_t* tokens_used_out) {
    do_request(req_json, req_len, on_token, user, resp_out, resp_len_out, tokens_used_out, 0);
}

void jrt_ipc_request_json(const void* req_json, size_t req_len,
                          char** resp_out, size_t* resp_len_out) {
    do_request(req_json, req_len, NULL, NULL, resp_out, resp_len_out, NULL, 1);
}
