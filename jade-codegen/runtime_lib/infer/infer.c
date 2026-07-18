/* infer.c — inference request construction and prompt deref.
 *
 * Owns the structured JSON request builder and all jrt_prompt_* entry points.
 * Dispatches through ipc for transport — never touches a socket
 * directly. Target-independent: same file is linked into both pthread and
 * jadeos runtime variants. */

#include "runtime.h"
#include "infer.h"
#include "ipc.h"

#include <ctype.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define JADE_INFER_MAX_TOKENS 1024

/* ── Growable byte buffer for JSON serialization ──────────────────────── */

typedef struct {
    char*  data;
    size_t len;
    size_t cap;
} infer_buf_t;

static void buf_init(infer_buf_t* b) {
    b->cap  = 256;
    b->len  = 0;
    b->data = malloc(b->cap);
    if (!b->data) { fprintf(stderr, "jade: oom (infer_buf)\n"); exit(1); }
}

static void buf_reserve(infer_buf_t* b, size_t extra) {
    if (b->len + extra + 1 <= b->cap) return;
    while (b->len + extra + 1 > b->cap) {
        b->cap *= 2;
    }
    char* nd = realloc(b->data, b->cap);
    if (!nd) { fprintf(stderr, "jade: oom (infer_buf grow)\n"); exit(1); }
    b->data = nd;
}

static void buf_putc(infer_buf_t* b, char c) {
    buf_reserve(b, 1);
    b->data[b->len++] = c;
}

static void buf_puts(infer_buf_t* b, const char* s) {
    size_t n = strlen(s);
    buf_reserve(b, n);
    memcpy(b->data + b->len, s, n);
    b->len += n;
}

static void buf_putd(infer_buf_t* b, int n) {
    char tmp[32];
    int w = snprintf(tmp, sizeof(tmp), "%d", n);
    if (w > 0) { buf_reserve(b, (size_t)w); memcpy(b->data + b->len, tmp, (size_t)w); b->len += (size_t)w; }
}

/* Emit a JSON string literal with proper escaping. NULL → null literal. */
static void buf_put_json_str(infer_buf_t* b, const char* s) {
    if (!s) { buf_puts(b, "null"); return; }
    buf_putc(b, '"');
    for (const unsigned char* p = (const unsigned char*)s; *p; p++) {
        switch (*p) {
        case '"':  buf_putc(b, '\\'); buf_putc(b, '"'); break;
        case '\\': buf_putc(b, '\\'); buf_putc(b, '\\'); break;
        case '\n': buf_putc(b, '\\'); buf_putc(b, 'n'); break;
        case '\r': buf_putc(b, '\\'); buf_putc(b, 'r'); break;
        case '\t': buf_putc(b, '\\'); buf_putc(b, 't'); break;
        default:
            if (*p < 0x20) {
                char esc[8];
                int w = snprintf(esc, sizeof(esc), "\\u%04x", (unsigned)*p);
                if (w > 0) { buf_reserve(b, (size_t)w); memcpy(b->data + b->len, esc, (size_t)w); b->len += (size_t)w; }
            } else {
                buf_putc(b, (char)*p);
            }
        }
    }
    buf_putc(b, '"');
}

/* ── Structured request builder ───────────────────────────────────────── */

typedef struct {
    const char* prompt;          /* required */
    const char* model;           /* required (may be empty string) */
    const char* grammar;         /* nullable */
    const char* anchor;          /* nullable */
    const char* stop_anchor;     /* nullable */
    int         max_tokens;      /* required */
    int         count_only;      /* boolean */
    int         stats_only;      /* boolean (total_tokens stats request) */
    int         keep_anchors;    /* boolean — make tool-span boundaries in-band */
    uint8_t     trust;           /* JRT_TRUSTED or JRT_TAINTED */
} infer_req_t;

/* Sticky session control, set from Jade via `llm.keep_anchors(b)`. Mirrors the
 * VM's `VmState.keep_anchors`: every prompt request reads this flag. Single-
 * threaded-set / many-read; a plain int is fine (writes are word-atomic on the
 * platforms we target, and Jade sets it from program code, not spawned tasks). */
static int g_keep_anchors = 0;

void jrt_llm_keep_anchors(int on) { g_keep_anchors = on ? 1 : 0; }

static void build_request(const infer_req_t* req, infer_buf_t* out) {
    buf_init(out);
    buf_putc(out, '{');

    buf_puts(out, "\"prompt\":");
    buf_put_json_str(out, req->prompt ? req->prompt : "");

    buf_puts(out, ",\"model\":");
    buf_put_json_str(out, req->model ? req->model : "");

    buf_puts(out, ",\"max_tokens\":");
    buf_putd(out, req->max_tokens);

    if (req->grammar) {
        buf_puts(out, ",\"grammar\":");
        buf_put_json_str(out, req->grammar);
    }
    if (req->anchor) {
        buf_puts(out, ",\"anchor\":");
        buf_put_json_str(out, req->anchor);
    }
    if (req->stop_anchor) {
        buf_puts(out, ",\"stop_anchor\":");
        buf_put_json_str(out, req->stop_anchor);
    }
    if (req->count_only) {
        buf_puts(out, ",\"count_only\":true");
    }
    if (req->stats_only) {
        buf_puts(out, ",\"stats_only\":true");
    }
    /* keep_anchors + trust are always emitted, matching jadelang's
     * encode_request (jade_os.rs) — the daemon reads them with serde defaults,
     * but being explicit keeps the wire unambiguous. */
    buf_puts(out, ",\"keep_anchors\":");
    buf_puts(out, req->keep_anchors ? "true" : "false");
    buf_puts(out, ",\"trust\":");
    buf_putd(out, (int)req->trust);

    buf_putc(out, '}');
}

/* ── Public prompt functions ──────────────────────────────────────────── */

char* jrt_prompt(const char* prompt, const char* model) {
    infer_req_t req = {
        .prompt = prompt,
        .model = model ? model : "",
        .max_tokens = JADE_INFER_MAX_TOKENS,
        .keep_anchors = g_keep_anchors,
        .trust = jrt_trust_of(prompt),
    };
    infer_buf_t json;
    build_request(&req, &json);

    char* resp = NULL;
    size_t resp_len = 0;
    jrt_ipc_request(json.data, json.len, &resp, &resp_len, NULL);
    free(json.data);
    if (!resp) return NULL;
    /* jaded is a TRUSTED transformer: it propagates the prompt's trust to its
     * output rather than minting taint. A trusted prompt yields a trusted
     * response (may flow to sinks); a tainted prompt (e.g. built from fetched
     * data) yields a tainted response (refused at sinks). The AOT runtime only
     * ever talks to the local daemon, so this propagation is the whole rule. */
    char* tagged = jrt_str_new(resp_len, req.trust);
    if (resp_len > 0) memcpy(tagged, resp, resp_len);
    free(resp);
    return tagged;
}

char* jrt_prompt_grammar(const char* prompt, const char* model, const char* grammar) {
    infer_req_t req = {
        .prompt = prompt,
        .model = model ? model : "",
        .grammar = grammar,
        .max_tokens = JADE_INFER_MAX_TOKENS,
        .keep_anchors = g_keep_anchors,
        .trust = jrt_trust_of(prompt),
    };
    infer_buf_t json;
    build_request(&req, &json);

    char* resp = NULL;
    size_t resp_len = 0;
    jrt_ipc_request(json.data, json.len, &resp, &resp_len, NULL);
    free(json.data);
    if (!resp) return NULL;
    /* jaded is a TRUSTED transformer: it propagates the prompt's trust to its
     * output rather than minting taint. A trusted prompt yields a trusted
     * response (may flow to sinks); a tainted prompt (e.g. built from fetched
     * data) yields a tainted response (refused at sinks). The AOT runtime only
     * ever talks to the local daemon, so this propagation is the whole rule. */
    char* tagged = jrt_str_new(resp_len, req.trust);
    if (resp_len > 0) memcpy(tagged, resp, resp_len);
    free(resp);
    return tagged;
}

char* jrt_prompt_grammar_ex(const char* prompt, const char* model,
                            const char* pattern,
                            const char* anchor_or_null,
                            const char* stop_or_null) {
    infer_req_t req = {
        .prompt = prompt,
        .model = model ? model : "",
        .grammar = pattern,
        .anchor = anchor_or_null,
        .stop_anchor = stop_or_null,
        .max_tokens = JADE_INFER_MAX_TOKENS,
        .keep_anchors = g_keep_anchors,
        .trust = jrt_trust_of(prompt),
    };
    infer_buf_t json;
    build_request(&req, &json);

    char* resp = NULL;
    size_t resp_len = 0;
    jrt_ipc_request(json.data, json.len, &resp, &resp_len, NULL);
    free(json.data);
    if (!resp) return NULL;
    /* jaded is a TRUSTED transformer: it propagates the prompt's trust to its
     * output rather than minting taint. A trusted prompt yields a trusted
     * response (may flow to sinks); a tainted prompt (e.g. built from fetched
     * data) yields a tainted response (refused at sinks). The AOT runtime only
     * ever talks to the local daemon, so this propagation is the whole rule. */
    char* tagged = jrt_str_new(resp_len, req.trust);
    if (resp_len > 0) memcpy(tagged, resp, resp_len);
    free(resp);
    return tagged;
}

/* ── jrt_prompt_typed: retry until response parses as the requested type ── */

/* Returns buf with surrounding whitespace stripped, or NULL if the trimmed
 * text doesn't fit in `bufsz`. Truncating-then-parsing would let an over-long
 * tainted response parse "valid" and get upgraded TAINTED→TRUSTED, so an
 * oversized response is rejected outright (this is a security gate). */
static const char* infer_trim(const char* s, char* buf, size_t bufsz) {
    while (*s && isspace((unsigned char)*s)) s++;
    size_t n = strlen(s);
    while (n > 0 && isspace((unsigned char)s[n-1])) n--;
    if (n >= bufsz) return NULL;
    memcpy(buf, s, n); buf[n] = '\0';
    return buf;
}

static int infer_valid_type(const char* resp, const char* type_name, char* tbuf, size_t tbsz) {
    if (!infer_trim(resp, tbuf, tbsz)) return 0;
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
    return 1;
}

char* jrt_prompt_typed(const char* prompt, const char* model,
                       const char* type_name, int max_retries) {
    /* `str` keeps the model's taint; numeric/bool types validate out
     * shell-injection vectors so the result is structurally TRUSTED. */
    uint8_t result_trust = (strcmp(type_name, "str") == 0) ? JRT_TAINTED : JRT_TRUSTED;

    char* resp = jrt_prompt(prompt, model);
    if (!resp) return NULL;

    char tbuf[512];
    if (infer_valid_type(resp, type_name, tbuf, sizeof(tbuf))) {
        char* out = jrt_str_dup(tbuf, result_trust);
        jrt_str_free(resp);
        return out;
    }
    jrt_str_free(resp);

    /* Build the correction prompt as a tagged TRUSTED string — it's a
     * runtime-controlled construct, not user input. */
    size_t cpcap = strlen(type_name) * 2 + 128;
    for (int attempt = 0; attempt < max_retries; attempt++) {
        char* correction = jrt_str_new(cpcap, JRT_TRUSTED);
        int n = snprintf(correction, cpcap,
            "Reply with only a valid %s value, nothing else. Previous response was not valid.",
            type_name);
        if (n < 0) { jrt_str_free(correction); return NULL; }

        char* retry_resp = jrt_prompt(correction, model);
        jrt_str_free(correction);
        if (!retry_resp) return NULL;

        if (infer_valid_type(retry_resp, type_name, tbuf, sizeof(tbuf))) {
            char* out = jrt_str_dup(tbuf, result_trust);
            jrt_str_free(retry_resp);
            return out;
        }
        jrt_str_free(retry_resp);
    }
    return NULL;
}

/* ── Streaming prompt deref with prefix-aware mute ────────────────────── */

typedef struct {
    char* rbuf;
    size_t rlen;
    size_t rcap;
    char* pbuf;
    size_t plen;
    size_t pcap;
    int muted;
    const char* anchor;
    const char* stop;
} stream_state_t;

static intptr_t mute_find(const char* buf, size_t len, const char* needle) {
    if (!needle || !*needle) return -1;
    size_t nlen = strlen(needle);
    if (nlen > len) return -1;
    for (size_t i = 0; i + nlen <= len; i++) {
        if (memcmp(buf + i, needle, nlen) == 0) return (intptr_t)i;
    }
    return -1;
}

static int mute_is_prefix(const char* buf, size_t len, const char* needle) {
    if (!needle || !*needle || len == 0) return 0;
    size_t nlen = strlen(needle);
    if (len > nlen) return 0;
    return memcmp(needle, buf, len) == 0;
}

static void stream_on_token(const char* bytes, size_t len, void* user) {
    stream_state_t* st = (stream_state_t*)user;

    /* Mirror everything into the full-response accumulator. */
    if (st->rlen + len + 1 > st->rcap) {
        while (st->rlen + len + 1 > st->rcap) st->rcap *= 2;
        char* nb = realloc(st->rbuf, st->rcap);
        if (!nb) { fprintf(stderr, "jade: oom (stream rbuf)\n"); exit(1); }
        st->rbuf = nb;
    }
    memcpy(st->rbuf + st->rlen, bytes, len);
    st->rlen += len;

    if (st->muted && !st->stop) return;
    if (!st->muted && !st->anchor) {
        fwrite(bytes, 1, len, stdout);
        fflush(stdout);
        return;
    }

    /* Append to pending buffer for prefix-aware scanning. */
    if (st->plen + len + 1 > st->pcap) {
        while (st->plen + len + 1 > st->pcap) st->pcap *= 2;
        char* nb = realloc(st->pbuf, st->pcap);
        if (!nb) { fprintf(stderr, "jade: oom (stream pbuf)\n"); exit(1); }
        st->pbuf = nb;
    }
    memcpy(st->pbuf + st->plen, bytes, len);
    st->plen += len;

    for (;;) {
        if (st->plen == 0) break;
        if (st->muted) {
            intptr_t hit = mute_find(st->pbuf, st->plen, st->stop);
            if (hit >= 0) {
                size_t slen = strlen(st->stop);
                size_t after = st->plen - (hit + slen);
                if (after > 0) memmove(st->pbuf, st->pbuf + hit + slen, after);
                st->plen = after;
                st->muted = 0;
                continue;
            }
            if (mute_is_prefix(st->pbuf, st->plen, st->stop)) break;
            memmove(st->pbuf, st->pbuf + 1, st->plen - 1);
            st->plen--;
        } else {
            intptr_t hit = mute_find(st->pbuf, st->plen, st->anchor);
            if (hit >= 0) {
                if (hit > 0) {
                    fwrite(st->pbuf, 1, hit, stdout);
                    fflush(stdout);
                }
                size_t alen = strlen(st->anchor);
                size_t after = st->plen - (hit + alen);
                if (after > 0) memmove(st->pbuf, st->pbuf + hit + alen, after);
                st->plen = after;
                st->muted = 1;
                continue;
            }
            if (mute_is_prefix(st->pbuf, st->plen, st->anchor)) break;
            fwrite(st->pbuf, 1, 1, stdout);
            fflush(stdout);
            memmove(st->pbuf, st->pbuf + 1, st->plen - 1);
            st->plen--;
        }
    }
}

char* jrt_prompt_stream_ex(const char* prompt, const char* model,
                           const char* pattern_or_null,
                           const char* anchor_or_null,
                           const char* stop_or_null,
                           int start_muted) {
    infer_req_t req = {
        .prompt = prompt,
        .model = model ? model : "",
        .grammar = pattern_or_null,
        .anchor = anchor_or_null,
        .stop_anchor = stop_or_null,
        .max_tokens = JADE_INFER_MAX_TOKENS,
        .keep_anchors = g_keep_anchors,
        /* Tell the daemon the prompt's actual taint (a tainted prompt must not
         * be reported as trusted), matching the non-streaming jrt_prompt* paths. */
        .trust = jrt_trust_of(prompt),
    };
    infer_buf_t json;
    build_request(&req, &json);

    stream_state_t st = {0};
    st.rcap = 4096;
    st.rbuf = malloc(st.rcap);
    st.pcap = 256;
    st.pbuf = malloc(st.pcap);
    if (!st.rbuf || !st.pbuf) {
        free(json.data); free(st.rbuf); free(st.pbuf);
        fprintf(stderr, "jade: oom (stream init)\n");
        exit(1);
    }
    st.muted = start_muted ? 1 : 0;
    st.anchor = anchor_or_null;
    st.stop = stop_or_null;

    char* daemon_resp = NULL;
    jrt_ipc_request_streaming(json.data, json.len, stream_on_token, &st,
                              &daemon_resp, NULL, NULL);
    free(json.data);
    free(daemon_resp);

    if (!st.muted && st.plen > 0 && !mute_is_prefix(st.pbuf, st.plen, st.anchor)) {
        fwrite(st.pbuf, 1, st.plen, stdout);
        fflush(stdout);
    }
    free(st.pbuf);

    /* Tag the accumulated text with the prompt's trust (jaded propagates, it
     * doesn't mint taint — see jrt_prompt). */
    char* tagged = jrt_str_new(st.rlen, req.trust);
    if (st.rlen > 0) memcpy(tagged, st.rbuf, st.rlen);
    free(st.rbuf);
    return tagged;
}

/* ── Token count (count_only request) ─────────────────────────────────── */

int64_t jrt_count_tokens(const char* str) {
    if (!str) return 0;
    infer_req_t req = {
        .prompt = str,
        .model = "",
        .max_tokens = 0,
        .count_only = 1,
        .trust = jrt_trust_of(str),  /* report the input's real taint */
    };
    infer_buf_t json;
    build_request(&req, &json);

    char* resp = NULL;
    uint64_t tokens_used = 0;
    jrt_ipc_request(json.data, json.len, &resp, NULL, &tokens_used);
    free(json.data);
    free(resp);
    return (int64_t)tokens_used;
}

/* ── Cumulative token count (stats_only request) ──────────────────────────
 * Mirrors the VM's llm.total_tokens (total_tokens_blocking in jade_os.rs): a
 * `stats_only` request whose DONE frame carries the session's running total. */
int64_t jrt_total_tokens(void) {
    infer_req_t req = {
        .prompt = "",
        .model = "",
        .max_tokens = 0,
        .stats_only = 1,
        .trust = JRT_TRUSTED,
    };
    infer_buf_t json;
    build_request(&req, &json);

    char* resp = NULL;
    uint64_t tokens_used = 0;
    jrt_ipc_request(json.data, json.len, &resp, NULL, &tokens_used);
    free(json.data);
    free(resp);
    return (int64_t)tokens_used;
}

/* ── Daemon health snapshot (health_only request) ─────────────────────────
 * Mirrors the VM's llm.health (health_blocking in jade_os.rs): a `health_only`
 * request whose 0x05 JSON frames carry the snapshot object, parsed into a dict.
 * Returns a tagged dict value, or JRT_NIL if the daemon sent no parseable JSON. */
jade_value_t jrt_llm_health(void) {
    /* Minimal request; the daemon ignores prompt/model for health_only. The
     * shared build_request adds keep_anchors/trust which the daemon defaults. */
    infer_buf_t json;
    buf_init(&json);
    buf_puts(&json, "{\"prompt\":\"\",\"model\":\"\",\"max_tokens\":0,\"health_only\":true}");

    char* resp = NULL;
    jrt_ipc_request_json(json.data, json.len, &resp, NULL);
    free(json.data);
    if (!resp) return JRT_NIL;

    /* resp is the accumulated JSON text (0x05 frames). Parse into a dict. */
    void* dict = jrt_json_parse(resp);
    free(resp);
    if (!dict) return JRT_NIL;
    return jrt_box_ptr(dict);
}

/* ── Model name resolution (env-derived, TRUSTED) ─────────────────────── */

char* jrt_get_model(void) {
    const char* m = getenv("JADE_MODEL");
    if (!m) m = "";
    return jrt_str_dup(m, JRT_TRUSTED);
}
