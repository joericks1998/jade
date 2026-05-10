/* jade_rt_jadeos.c — Jade OS kernel-backed async runtime.
 *
 * Selected by `make JADEOS=1` when cross-compiling for the Jade OS target.
 * Requires Jade OS kernel headers: <jade/task.h>, <jade/alloc.h>
 *
 * Jade OS exposes native task primitives that map 1-to-1 onto jade_rt's API,
 * making this implementation a thin shim over the kernel syscall layer.
 */

#if !defined(__JADE_OS__)
#  error "jade_rt_jadeos.c must only be compiled for the Jade OS target (pass -D__JADE_OS__)."
#endif

#include "jade_rt.h"
#include <jade/alloc.h>
#include <jade/task.h>

/* JadeOS userspace runs against musl libc — full C standard library is
 * available for dict runtime, exception handling, and /dev/jade I/O. */
#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>
#include <setjmp.h>
#include <stdio.h>
#include <ctype.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>

/* Fatal abort for Jade OS: kills the current task with exit code -1.
 * Used when a runtime invariant is violated (OOM, failed task spawn).
 * __builtin_unreachable() suppresses false "function may not return" warnings. */
#define JADE_ABORT() do { jade_task_exit(-1); __builtin_unreachable(); } while (0)

struct jade_future {
    jade_task_id_t task_id;
    jade_value_t   result;
};

typedef struct {
    jade_task_fn        fn;
    jade_value_t*       args;
    int                 n_args;
    struct jade_future* future;
} TaskCtx;

static void task_entry(void* vp) {
    TaskCtx* tc = (TaskCtx*)vp;
    jade_value_t result = tc->fn(tc->args, tc->n_args);
    jade_free(tc->args);
    struct jade_future* fut = tc->future;
    jade_free(tc);          /* matched allocation: jade_alloc(sizeof(TaskCtx)) */
    fut->result = result;
    /* Full memory barrier: ensure the result store is visible to jade_task_wait
     * callers before the task's completion is signalled by jade_task_exit. */
    __sync_synchronize();
    jade_task_exit(0);
}

jade_future_t jade_spawn(jade_task_fn fn, jade_value_t* args, int n_args) {
    struct jade_future* fut = jade_alloc(sizeof(struct jade_future));
    if (!fut) JADE_ABORT();

    jade_value_t* args_copy = NULL;
    if (n_args > 0) {
        /* Guard against overflow on 32-bit targets where size_t is 32 bits. */
        if ((size_t)n_args > SIZE_MAX / sizeof(jade_value_t)) JADE_ABORT();
        args_copy = jade_alloc(sizeof(jade_value_t) * (size_t)n_args);
        if (!args_copy) { jade_free(fut); JADE_ABORT(); }
        jade_memcpy(args_copy, args, sizeof(jade_value_t) * (size_t)n_args);
    }

    TaskCtx* tc = jade_alloc(sizeof(TaskCtx));
    if (!tc) { jade_free(args_copy); jade_free(fut); JADE_ABORT(); }
    tc->fn     = fn;
    tc->args   = args_copy;
    tc->n_args = n_args;
    tc->future = fut;

    jade_task_id_t tid = jade_task_spawn(task_entry, tc, JADE_TASK_NORMAL);
    if (tid == JADE_INVALID_TASK) {
        jade_free(tc);
        jade_free(args_copy);
        jade_free(fut);
        JADE_ABORT();
    }
    fut->task_id = tid;
    return fut;
}

jade_value_t jade_await(jade_future_t future) {
    struct jade_future* fut = future;
    jade_task_wait(fut->task_id);
    return fut->result;
}

void jade_join(jade_future_t* futures, int n, jade_value_t* results_out) {
    for (int i = 0; i < n; i++) {
        results_out[i] = jade_await(futures[i]);
    }
}

void jade_future_free(jade_future_t future) {
    jade_free(future);
}

/* ── Dict ─────────────────────────────────────────────────────────────── */

#define JADE_DICT_INIT_CAP 8

typedef struct { char* key; int64_t val; } JadeDictSlot;
typedef struct { JadeDictSlot* slots; int64_t cap; int64_t count; } JadeDict;

static uint64_t dict_hash(const char* key) {
    uint64_t h = 14695981039346656037ULL;
    for (const unsigned char* p = (const unsigned char*)key; *p; p++) {
        h ^= (uint64_t)*p; h *= 1099511628211ULL;
    }
    return h;
}

static int dict_grow(JadeDict* d) {
    int64_t new_cap = d->cap * 2;
    JadeDictSlot* ns = calloc((size_t)new_cap, sizeof(JadeDictSlot));
    if (!ns) return 0;
    for (int64_t i = 0; i < d->cap; i++) {
        if (!d->slots[i].key) continue;
        uint64_t idx = dict_hash(d->slots[i].key) % (uint64_t)new_cap;
        while (ns[idx].key) idx = (idx + 1) % (uint64_t)new_cap;
        ns[idx] = d->slots[i];
    }
    free(d->slots); d->slots = ns; d->cap = new_cap;
    return 1;
}

void* jade_dict_create(void) {
    JadeDict* d = malloc(sizeof(JadeDict));
    if (!d) JADE_ABORT();
    d->slots = calloc(JADE_DICT_INIT_CAP, sizeof(JadeDictSlot));
    if (!d->slots) JADE_ABORT();
    d->cap = JADE_DICT_INIT_CAP; d->count = 0;
    return d;
}

void jade_dict_set(void* dict, const char* key, int64_t val) {
    JadeDict* d = (JadeDict*)dict;
    if (d->count * 4 >= d->cap * 3) { if (!dict_grow(d)) JADE_ABORT(); }
    uint64_t idx = dict_hash(key) % (uint64_t)d->cap;
    while (d->slots[idx].key && strcmp(d->slots[idx].key, key) != 0)
        idx = (idx + 1) % (uint64_t)d->cap;
    if (!d->slots[idx].key) {
        d->slots[idx].key = strdup(key); if (!d->slots[idx].key) JADE_ABORT();
        d->count++;
    }
    d->slots[idx].val = val;
}

int64_t jade_dict_get(void* dict, const char* key) {
    JadeDict* d = (JadeDict*)dict;
    uint64_t idx = dict_hash(key) % (uint64_t)d->cap;
    for (int64_t i = 0; i < d->cap; i++) {
        if (!d->slots[idx].key) return 0;
        if (strcmp(d->slots[idx].key, key) == 0) return d->slots[idx].val;
        idx = (idx + 1) % (uint64_t)d->cap;
    }
    return 0;
}

int64_t jade_dict_len(void* dict) { return ((JadeDict*)dict)->count; }

void jade_dict_free(void* dict) {
    JadeDict* d = (JadeDict*)dict;
    for (int64_t i = 0; i < d->cap; i++) if (d->slots[i].key) free(d->slots[i].key);
    free(d->slots); free(d);
}

/* ── Exceptions ───────────────────────────────────────────────────────── */
/* JadeOS: single-threaded process model — global exception stack is safe. */

#define JADE_EXC_MAX_DEPTH 64

static void*   exc_stack[JADE_EXC_MAX_DEPTH];
static int64_t exc_thrown_value;
static int     exc_depth = 0;

void jade_exc_push_frame(void* jmpbuf) {
    if (exc_depth >= JADE_EXC_MAX_DEPTH) {
        fprintf(stderr, "jade: exception stack overflow\n"); JADE_ABORT();
    }
    exc_stack[exc_depth++] = jmpbuf;
}

void jade_exc_pop(void) { if (exc_depth > 0) exc_depth--; }

void jade_exc_throw(int64_t value) {
    if (exc_depth == 0) {
        fprintf(stderr, "jade: unhandled exception\n"); jade_task_exit(1);
    }
    exc_thrown_value = value;
    void* buf = exc_stack[--exc_depth];
    longjmp(*(jmp_buf*)buf, 1);
}

int64_t jade_exc_value(void) { return exc_thrown_value; }

/* ── LLM Inference ────────────────────────────────────────────────────── */
/* JadeOS: jade-tree listens on a Unix domain socket; no kernel module needed. */

#include <sys/socket.h>
#include <sys/un.h>

#define JADE_SOCK_PATH        "/run/jade/llm.sock"
#define JADE_INFER_MAX_TOKENS 1024
#define JADE_RESP_INIT_CAP    4096

static char* infer_json_escape(const char* s) {
    size_t n = strlen(s);
    char* out = malloc(n * 6 + 3); if (!out) return NULL;
    char* p = out; *p++ = '"';
    for (const unsigned char* c = (const unsigned char*)s; *c; c++) {
        if (*c == '"')  { *p++ = '\\'; *p++ = '"'; }
        else if (*c == '\\') { *p++ = '\\'; *p++ = '\\'; }
        else if (*c == '\n') { *p++ = '\\'; *p++ = 'n'; }
        else if (*c == '\r') { *p++ = '\\'; *p++ = 'r'; }
        else if (*c == '\t') { *p++ = '\\'; *p++ = 't'; }
        else if (*c < 0x20)  { p += sprintf(p, "\\u%04x", (unsigned)*c); }
        else { *p++ = (char)*c; }
    }
    *p++ = '"'; *p = '\0'; return out;
}

static int infer_write_all(int fd, const void* buf, size_t n) {
    const char* p = (const char*)buf;
    while (n > 0) {
        ssize_t w = write(fd, p, n);
        if (w < 0) { if (errno == EINTR) continue; return -1; }
        p += (size_t)w; n -= (size_t)w;
    }
    return 0;
}

static int infer_read_exact(int fd, void* buf, size_t n) {
    char* p = (char*)buf;
    while (n > 0) {
        ssize_t r = read(fd, p, n);
        if (r < 0) { if (errno == EINTR) continue; return -1; }
        if (r == 0) return -1;
        p += (size_t)r; n -= (size_t)r;
    }
    return 0;
}

char* jade_infer(const char* prompt, const char* model) {
    char* ep = infer_json_escape(prompt);
    char* em = infer_json_escape(model);
    if (!ep || !em) { free(ep); free(em); return NULL; }

    size_t jcap = strlen(ep) + strlen(em) + 80;
    char* json = malloc(jcap);
    if (!json) { free(ep); free(em); return NULL; }
    int jlen = snprintf(json, jcap,
        "{\"prompt\":%s,\"model\":%s,\"history\":[],\"max_tokens\":%d}",
        ep, em, JADE_INFER_MAX_TOKENS);
    free(ep); free(em);
    if (jlen < 0 || (size_t)jlen >= jcap) { free(json); return NULL; }

    /* Connect to jade-tree's Unix socket; retry up to 3 times (100 ms apart). */
    int fd = -1;
    for (int retry = 0; retry < 3; retry++) {
        struct sockaddr_un addr;
        memset(&addr, 0, sizeof(addr));
        addr.sun_family = AF_UNIX;
        strncpy(addr.sun_path, JADE_SOCK_PATH, sizeof(addr.sun_path) - 1);
        int s = socket(AF_UNIX, SOCK_STREAM, 0);
        if (s < 0) break;
        if (connect(s, (struct sockaddr*)&addr, sizeof(addr)) == 0) { fd = s; break; }
        close(s);
        if (retry < 2) usleep(100000);
    }
    if (fd < 0) { free(json); return NULL; }

    uint32_t blen = (uint32_t)jlen;
    uint8_t hdr[4] = { blen & 0xFF, (blen>>8)&0xFF, (blen>>16)&0xFF, (blen>>24)&0xFF };
    if (infer_write_all(fd, hdr, 4) < 0 || infer_write_all(fd, json, (size_t)jlen) < 0) {
        free(json); close(fd); return NULL;
    }
    free(json);

    size_t rcap = JADE_RESP_INIT_CAP, rlen = 0;
    char* rbuf = malloc(rcap); if (!rbuf) { close(fd); return NULL; }

    for (;;) {
        uint8_t ftype;
        if (infer_read_exact(fd, &ftype, 1) < 0) { free(rbuf); close(fd); return NULL; }
        uint8_t lb[2];
        if (infer_read_exact(fd, lb, 2) < 0) { free(rbuf); close(fd); return NULL; }
        uint16_t plen = (uint16_t)(lb[0] | (lb[1] << 8));

        if (ftype == 0x01) {
            while (rlen + plen + 1 > rcap) {
                rcap *= 2;
                char* nb = realloc(rbuf, rcap);
                if (!nb) { free(rbuf); close(fd); return NULL; }
                rbuf = nb;
            }
            if (infer_read_exact(fd, rbuf + rlen, plen) < 0) { free(rbuf); close(fd); return NULL; }
            rlen += plen;
        } else if (ftype == 0x02) {
            /* DONE: consume payload (token count) and stop. */
            if (plen > 0) { char tmp[8]; infer_read_exact(fd, tmp, plen < 8 ? plen : 8); }
            break;
        } else {
            /* ERROR or unknown: consume payload and fail. */
            if (plen > 0) { char tmp[256]; infer_read_exact(fd, tmp, plen < 256 ? plen : 256); }
            free(rbuf); close(fd); return NULL;
        }
    }
    close(fd);
    rbuf[rlen] = '\0';
    return rbuf;
}

/* Trim leading/trailing whitespace in-place; returns pointer to first non-space. */
static const char* infer_trim(const char* s, char* buf, size_t bufsz) {
    while (*s && isspace((unsigned char)*s)) s++;
    size_t n = strlen(s);
    while (n > 0 && isspace((unsigned char)s[n-1])) n--;
    if (n >= bufsz) n = bufsz - 1;
    memcpy(buf, s, n); buf[n] = '\0';
    return buf;
}

static int infer_valid_type(const char* resp, const char* type_name, char* tbuf, size_t tbsz) {
    infer_trim(resp, tbuf, tbsz);
    if (strcmp(type_name, "str") == 0) return 1;
    if (strcmp(type_name, "int") == 0) {
        if (*tbuf == '\0') return 0;
        char* end; strtoll(tbuf, &end, 10);
        return *end == '\0';
    }
    if (strcmp(type_name, "float") == 0) {
        if (*tbuf == '\0') return 0;
        char* end; strtod(tbuf, &end);
        return *end == '\0';
    }
    if (strcmp(type_name, "bool") == 0)
        return strcmp(tbuf, "true") == 0 || strcmp(tbuf, "false") == 0;
    return 1; /* unknown type: pass through */
}

char* jade_infer_typed(const char* prompt, const char* model,
                       const char* type_name, int max_retries) {
    char* resp = jade_infer(prompt, model);
    if (!resp) return NULL;

    char tbuf[512];
    if (infer_valid_type(resp, type_name, tbuf, sizeof(tbuf))) {
        /* Return trimmed copy for consistent parsing by the codegen. */
        char* out = strdup(tbuf); free(resp); return out;
    }
    free(resp);

    /* Retry loop: fold prior exchange into a self-contained correction prompt. */
    char* current_prompt = strdup(prompt);
    if (!current_prompt) return NULL;

    for (int attempt = 0; attempt < max_retries; attempt++) {
        size_t cpcap = strlen(current_prompt) + strlen(type_name) * 2 + 128;
        char* correction = malloc(cpcap);
        if (!correction) { free(current_prompt); return NULL; }
        snprintf(correction, cpcap,
            "Reply with only a valid %s value, nothing else. Previous response was not valid.",
            type_name);

        char* retry_resp = jade_infer(correction, model);
        free(correction);
        free(current_prompt);
        current_prompt = NULL;

        if (!retry_resp) return NULL;

        if (infer_valid_type(retry_resp, type_name, tbuf, sizeof(tbuf))) {
            char* out = strdup(tbuf); free(retry_resp); return out;
        }
        current_prompt = retry_resp; /* use as context for next attempt */
    }

    free(current_prompt);
    return NULL;
}
