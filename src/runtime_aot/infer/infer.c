/* infer.c — prompt deref for compiled binaries.
 *
 * Owns every jrt_prompt_* entry point. Each one drives the installed provider
 * package in process, through the native-package machinery (jrt_native_*), so
 * this file has no transport of its own: no socket, no JSON request builder, no
 * framing. Those existed to reach the inference daemon and went with it.
 * Target-independent: the same file is linked into every platform backend
 * variant. */

#include "runtime.h"
#include "infer.h"

#include <ctype.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* ── The request a prompt deref makes ─────────────────────────────────── */

typedef struct {
    const char* prompt;          /* required */
    const char* model;           /* required (may be empty string) */
    const char* grammar;         /* nullable */
    const char* anchor;          /* nullable */
    const char* stop_anchor;     /* nullable */
    uint8_t     trust;           /* JRT_TRUSTED or JRT_TAINTED */
} infer_req_t;

/* ── Provider-package drive ────────────────────────────────────────────────
 *
 * `?p` drives the provider package installed in the active slot, in process. A
 * provider is a compiled Jade `--lib` (dovata's anthropic/openai) exposing
 * `infer(request) -> [Frame]` and, usually, `configure(opts)` — the same
 * contract the VM drives. We load it through the native-package machinery
 * (jrt_native_*), configure it with the stored credential, call
 * `infer({prompt})`, and fold the returned frame dicts
 * ({"type":"Token"|"Done"|"Error",…}) into the response text. A bad shape or an
 * `Error` frame raises a catchable Jade error — like the VM, so a compiled `?p`
 * inside a `try` can catch it.
 *
 * This used to be one of two paths, chosen per request against the daemon on
 * `$HOME/.jade/llm.sock`. It is the only one now. */

static void* g_provider = NULL; /* cached handle for the active provider package */

/* Raise a catchable Jade error (TRUSTED message). Does not return. The message is
 * copied, so a pointer into a soon-freed value is safe to pass. */
static void provider_raise(const char* msg) {
    jade_exc_throw_typed(jrt_box_str(jrt_str_dup(msg, JRT_TRUSTED)), NULL);
}

/* Load the active provider package (once) and configure it with the stored
 * credential. jrt_native_load raises on failure. */
static void* provider_handle(void) {
    if (g_provider) return g_provider;
    char* path = jrt_provider_active_lib_path();
    if (!path) provider_raise("no inference backend available — run `jade register` to choose a provider and set your API key");
    void* h = jrt_native_load(path); /* raises on dlopen / missing jade_pkg_init */
    free(path);

    char* cfg = jrt_provider_active_config();
    if (cfg && cfg[0] && jrt_native_has(h, "configure")) {
        char* s = jrt_str_dup(cfg, JRT_TRUSTED); /* jrt_json_parse_chunk wants a tagged string */
        jade_value_t config = jrt_json_parse_chunk(s);
        jrt_str_free(s);
        jrt_native_call(h, "configure", &config, 1);
    }
    free(cfg);
    g_provider = h;
    return h;
}

/* Build the `infer` request dict from the prompt request. */
static jade_value_t provider_request(const infer_req_t* req) {
    void* d = jrt_kdict_new();
    jrt_kdict_set(d, (int64_t)jrt_box_str(jrt_str_dup("prompt", JRT_TRUSTED)),
                     (int64_t)jrt_box_str(jrt_str_dup(req->prompt ? req->prompt : "", req->trust)));
    if (req->model && req->model[0])
        jrt_kdict_set(d, (int64_t)jrt_box_str(jrt_str_dup("model", JRT_TRUSTED)),
                         (int64_t)jrt_box_str(jrt_str_dup(req->model, JRT_TRUSTED)));
    /* grammar + its anchors are one feature: the anchors bound the span the
     * grammar constrains, so sending the pattern alone would silently drop half
     * of an explicit Grammar.new(pattern, anchor, stop). Same keys as the VM's
     * request_value, so a package sees one request shape from both engines. */
    if (req->grammar)
        jrt_kdict_set(d, (int64_t)jrt_box_str(jrt_str_dup("grammar", JRT_TRUSTED)),
                         (int64_t)jrt_box_str(jrt_str_dup(req->grammar, JRT_TRUSTED)));
    if (req->anchor)
        jrt_kdict_set(d, (int64_t)jrt_box_str(jrt_str_dup("anchor", JRT_TRUSTED)),
                         (int64_t)jrt_box_str(jrt_str_dup(req->anchor, JRT_TRUSTED)));
    if (req->stop_anchor)
        jrt_kdict_set(d, (int64_t)jrt_box_str(jrt_str_dup("stop_anchor", JRT_TRUSTED)),
                         (int64_t)jrt_box_str(jrt_str_dup(req->stop_anchor, JRT_TRUSTED)));
    return jrt_box_ptr(d);
}

/* Drive the active provider for one request; return the accumulated response text
 * (malloc'd, *out_len bytes), or raise on an `Error` frame. */
static char* provider_infer_text(const infer_req_t* req, size_t* out_len) {
    void* h = provider_handle();
    jade_value_t request = provider_request(req);
    jade_value_t frames = jrt_native_call(h, "infer", &request, 1);

    if (!jrt_is_ptr(frames) || jrt_kind_of(jrt_unbox_ptr(frames)) != JK_ARRAY)
        provider_raise("provider `infer` did not return a frame array");
    void* arr = jrt_unbox_ptr(frames);
    int64_t n = jrt_coll_array_len(arr);

    size_t cap = 256, len = 0;
    char* text = (char*)malloc(cap);
    if (!text) jade_rt_fatal("jade: out of memory");
    text[0] = 0;

    for (int64_t i = 0; i < n; i++) {
        jade_value_t fr = (jade_value_t)jrt_coll_array_get(arr, i);
        if (!jrt_is_ptr(fr) || jrt_kind_of(jrt_unbox_ptr(fr)) != JK_DICT) continue;
        void* fd = jrt_unbox_ptr(fr);

        int64_t tyw = (int64_t)JRT_NIL;
        if (!jrt_coll_dict_get(fd, "type", &tyw) || !jrt_is_str((jade_value_t)tyw)) continue;
        const char* ty = (const char*)jrt_unbox_ptr((jade_value_t)tyw);

        if (strcmp(ty, "Token") == 0) {
            int64_t txw = (int64_t)JRT_NIL;
            if (jrt_coll_dict_get(fd, "text", &txw) && jrt_is_str((jade_value_t)txw)) {
                const char* t = (const char*)jrt_unbox_ptr((jade_value_t)txw);
                size_t tl = strlen(t);
                if (len + tl + 1 > cap) {
                    while (len + tl + 1 > cap) cap *= 2;
                    char* nt = (char*)realloc(text, cap);
                    if (!nt) { free(text); jade_rt_fatal("jade: out of memory"); }
                    text = nt;
                }
                memcpy(text + len, t, tl);
                len += tl;
                text[len] = 0;
            }
        } else if (strcmp(ty, "Error") == 0) {
            int64_t mw = (int64_t)JRT_NIL;
            const char* m = (jrt_coll_dict_get(fd, "message", &mw) && jrt_is_str((jade_value_t)mw))
                          ? (const char*)jrt_unbox_ptr((jade_value_t)mw) : "provider error";
            free(text);
            provider_raise(m); /* does not return */
        }
        /* Done / Json carry no plain text — ignore. */
    }

    *out_len = len;
    return text;
}

/* provider_infer_text, tagged with the prompt's trust (a provider is a trusted
 * transformer: a trusted prompt yields a trusted response, a tainted one a
 * tainted response — same rule as the daemon path). */
static char* provider_tagged(const infer_req_t* req) {
    size_t len = 0;
    char* text = provider_infer_text(req, &len);
    char* tagged = jrt_str_new(len, req->trust);
    if (len > 0) memcpy(tagged, text, len);
    free(text);
    return tagged;
}

/* ── Public prompt functions ──────────────────────────────────────────── */

/* Each of these built a JSON body and handed it to the daemon when no provider
 * was installed. That branch is gone, so they are now the same call with a
 * different request shape.
 *
 * The response carries the *prompt's* trust rather than minting taint: a
 * provider is a trusted transformer, so a trusted prompt yields a trusted
 * response (may flow to sinks) and a tainted one (say, built from fetched data)
 * yields a tainted response (refused at sinks). */

char* jrt_prompt(const char* prompt, const char* model) {
    infer_req_t req = {
        .prompt = prompt,
        .model = model ? model : "",
        .trust = jrt_trust_of(prompt),
    };
    return provider_tagged(&req);
}

char* jrt_prompt_grammar(const char* prompt, const char* model, const char* grammar) {
    infer_req_t req = {
        .prompt = prompt,
        .model = model ? model : "",
        .grammar = grammar,
        .trust = jrt_trust_of(prompt),
    };
    return provider_tagged(&req);
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
        .trust = jrt_trust_of(prompt),
    };
    return provider_tagged(&req);
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
                       const char* type_name) {
    /* `str` keeps the model's taint; numeric/bool types validate out
     * shell-injection vectors so the result is structurally TRUSTED. */
    uint8_t result_trust = (strcmp(type_name, "str") == 0) ? JRT_TAINTED : JRT_TRUSTED;

    /* Single-shot: grammar-constrained sampling already forces the reply into a
     * shape the target type accepts, so a validation failure here is a genuine
     * mismatch to surface, not something to re-ask for. NULL on failure. */
    char* resp = jrt_prompt(prompt, model);
    if (!resp) return NULL;

    char tbuf[512];
    if (infer_valid_type(resp, type_name, tbuf, sizeof(tbuf))) {
        char* out = jrt_str_dup(tbuf, result_trust);
        jrt_str_free(resp);
        return out;
    }
    jrt_str_free(resp);
    return NULL;
}

/* Struct-typed prompt deref: `?p |> City`.
 *
 * Mirrors jrt_prompt_typed's shape — ask, validate, re-ask with a correction —
 * but validation means "did it coerce into a City", which lives in the shared
 * runtime (jade-runtime, src/coercef.rs) beside the field table so the rule is
 * written once for both engines.
 *
 * Previously this path did not exist: infer_valid_type returns "valid" for any
 * type name it does not recognise, so a struct-typed deref accepted the raw
 * reply and produced a *string*. Field access on it then failed with "value has
 * no fields" while the VM had built a real struct.
 *
 * Single-shot (grammar-constrained sampling already shapes the reply): returns a
 * tagged struct word, or raises when the reply doesn't coerce. */
int64_t jrt_prompt_struct(const char* prompt, const char* model,
                          const char* type_name) {
    char* resp = jrt_prompt(prompt, model);
    if (resp) {
        int64_t out = jrt_coerce_struct(resp, type_name);
        jrt_str_free(resp);
        if (out != JRT_NIL) return out;
    }

    char msg[160];
    snprintf(msg, sizeof msg,
             "prompt '<prompt>' failed to produce a valid typed value after 1 attempt(s)");
    jade_exc_throw_typed(jrt_box_str(jrt_str_dup(msg, JRT_TRUSTED)), NULL);
    return JRT_NIL; /* unreachable (the throw longjmps) */
}

/* Raising wrapper around jrt_prompt_typed.
 *
 * jrt_prompt_typed returns NULL when the reply doesn't coerce. Codegen used to
 * tag that NULL as a string and carry on, so a prompt that never produced a
 * coercible value segfaulted the compiled program while the VM reported a
 * clean error. The message matches JadeError::PromptOverflow, minus the source
 * span the AOT does not carry at runtime. A typed deref is single-shot, so the
 * count is always 1 — matching the VM. */
char* jrt_prompt_typed_checked(const char* prompt, const char* model,
                               const char* type_name) {
    char* out = jrt_prompt_typed(prompt, model, type_name);
    if (out) return out;
    char msg[160];
    snprintf(msg, sizeof msg,
             "prompt '<prompt>' failed to produce a valid typed value after 1 attempt(s)");
    jade_exc_throw_typed(jrt_box_str(jrt_str_dup(msg, JRT_TRUSTED)), NULL);
    return NULL; /* unreachable (the throw longjmps) */
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
        .trust = jrt_trust_of(prompt),
    };

    /* Ask first, then set up the stream state. A provider raises on an `Error`
     * frame, and that throw longjmps past every free() below — so nothing is
     * allocated until the call that can throw has returned. */
    size_t plen = 0;
    char* ptext = provider_infer_text(&req, &plen);

    stream_state_t st = {0};
    st.rcap = 4096;
    st.rbuf = malloc(st.rcap);
    st.pcap = 256;
    st.pbuf = malloc(st.pcap);
    if (!st.rbuf || !st.pbuf) {
        free(ptext); free(st.rbuf); free(st.pbuf);
        fprintf(stderr, "jade: oom (stream init)\n");
        exit(1);
    }
    st.muted = start_muted ? 1 : 0;
    st.anchor = anchor_or_null;
    st.stop = stop_or_null;

    /* A provider returns the whole reply at once rather than a token stream, so
     * this is one call where the daemon made many. It still goes through the
     * same muting scanner: the VM runs its provider replies through its own
     * scanner too, and skipping it here would print an anchored region the VM
     * suppresses — a parity gap this path used to have, back when it wrote the
     * reply straight to stdout. */
    if (plen > 0) stream_on_token(ptext, plen, &st);
    free(ptext);

    if (!st.muted && st.plen > 0 && !mute_is_prefix(st.pbuf, st.plen, st.anchor)) {
        fwrite(st.pbuf, 1, st.plen, stdout);
        fflush(stdout);
    }
    free(st.pbuf);

    char* tagged = jrt_str_new(st.rlen, req.trust);
    if (st.rlen > 0) memcpy(tagged, st.rbuf, st.rlen);
    free(st.rbuf);
    return tagged;
}

/* Terminate a stream()'s live output. Lives here so it shares the same stdout
 * buffering as stream_on_token above; the streaming entry point in the Rust
 * runtime calls it once the response is complete. */
void jrt_stream_newline(void) {
    fputc('\n', stdout);
    fflush(stdout);
}
