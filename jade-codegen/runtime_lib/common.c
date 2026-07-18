/* Platform-agnostic core of the Jade runtime. Shared verbatim by the host
 * (posix.c) and Jade OS (jadeos.c) backends, which supply only the concurrency
 * layer (jade_spawn/await/join) and the process-exit primitive jade_rt_exit. */
#include "runtime.h"

#include <assert.h>
#include <ctype.h>
#include <math.h>
#include <setjmp.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Fatal error: print `msg`, then terminate via the platform exit primitive
 * (jade_rt_exit is supplied by posix.c / jadeos.c — exit vs jade_task_exit). */
void jade_rt_fatal(const char* msg) {
    fprintf(stderr, "%s\n", msg);
    jade_rt_exit(1);
}

/* ── Tagged string allocator ─────────────────────────────────────────────
 * jrt_str_new / jrt_trust_of / jrt_str_dup / jrt_str_free / jrt_str_concat all
 * moved to the shared Rust runtime crate (jade-runtime, src/string.rs). The
 * 8-byte-header layout (trust at data[-1], NUL at data[len], free(data-8)) is
 * byte-identical, allocated through the same system malloc/free so strings are
 * interchangeable across the C runtime, Rust, and codegen's literal globals.
 * Declarations remain in runtime.h; the ~130 call sites across the runtime
 * resolve against the Rust symbols at link time. With concat now Rust-native,
 * `jade_runtime` no longer calls back into the C runtime — the archive
 * dependency is one-directional. */

/* ── Boxed float ──────────────────────────────────────────────────────────
 * jrt_box_float / jrt_unbox_float moved to the shared Rust runtime crate
 * (jadelang/jade-runtime, src/float.rs), which the AOT link pulls in as a
 * staticlib. The declarations remain in runtime.h; the calls throughout this
 * file resolve against the Rust symbols. Floats are heap-boxed there exactly as
 * before (system malloc of an 8-byte f64, tagged JRT_TAG_FLOAT). */

/* Await N futures into results_out. Platform-agnostic (only calls jade_await,
 * which each backend supplies), so it lives here rather than being duplicated
 * in posix.c / jadeos.c. */
void jade_join(jade_future_t* futures, int n, jade_value_t* results_out) {
    for (int i = 0; i < n; i++) {
        results_out[i] = jade_await(futures[i]);
    }
}

/* Length-bounded string equality/ordering moved to the shared Rust runtime
 * (jade-runtime, src/strval.rs) along with the dynamic eq/cmp that used them.
 * The 1 MiB scan cap and the "no NUL within cap → not a well-formed string"
 * rule (needed because JRT_TAG_PTR is shared by strings and dicts/arrays) are
 * preserved there. */

/* jrt_eq_any moved to the shared Rust runtime crate (jade-runtime, src/ops.rs
 * eq / src/strval.rs). It never raises, so Rust exports it directly (ffi.rs);
 * the declaration remains in runtime.h. */

int jrt_snprintf_any(char* buf, size_t cap, int64_t val) {
    /* Format a type-erased value by its runtime tag (matches the VM's
     * value_to_display for scalars/strings). A non-string heap object reaching
     * here is an Unknown-typed dict/array/struct printed without static type
     * info — render a safe placeholder rather than dereferencing it as text. */
    jade_value_t v = (jade_value_t)val;
    if (jrt_is_int(v))   return snprintf(buf, cap, "%lld", (long long)jrt_unbox_int(v));
    if (jrt_is_str(v))   return snprintf(buf, cap, "%s", (const char*)jrt_unbox_ptr(v));
    if (jrt_is_float(v)) return jrt_snprintf_float(buf, cap, jrt_unbox_float(v));
    if (jrt_is_bool(v))  return snprintf(buf, cap, "%s", jrt_unbox_bool(v) ? "true" : "false");
    if (jrt_is_nil(v))   return snprintf(buf, cap, "nil");
    return snprintf(buf, cap, "<object>");  /* non-string heap pointer */
}

void jrt_print_any(int64_t val, const char* suffix) {
    /* Print a type-erased value to stdout, then `suffix`. Used by print() for
     * statically-Unknown args. Strings are written directly (unbounded) — unlike
     * routing through jrt_snprintf_any + a fixed scratch buffer, which truncates
     * a long Unknown string (e.g. llm.tool_grammar()). Scalars/objects are short,
     * so they format into a small buffer via the shared jrt_snprintf_any. */
    jade_value_t v = (jade_value_t)val;
    if (jrt_is_str(v)) {
        fputs((const char*)jrt_unbox_ptr(v), stdout);
    } else if (jrt_is_ptr(v)) {
        /* A kind-tagged collection — render `[…]`/`{…}` (recursive, unbounded). */
        char* r = jrt_render_any(val);
        fputs(r, stdout);
        free(r);
    } else {
        char buf[64];
        jrt_snprintf_any(buf, sizeof buf, val);
        fputs(buf, stdout);
    }
    if (suffix) fputs(suffix, stdout);
}

/* ── Dynamic conversion builtins int()/float()/bool() (Chunk backend) ──────
 * Mirror the VM's `coerce_builtin` (vm.rs) EXACTLY on the tagged runtime kind.
 * int()/float() raise a catchable error on a non-numeric string or a
 * non-convertible kind (longjmp stays in C); bool() never raises. */
#include <errno.h>
#include <strings.h>            /* strcasecmp */
static void throw_msg(const char* m);   /* defined below; used by int()/float() */

/* Trim ASCII whitespace in place by returning adjusted [start,end). Caller owns
 * the buffer; we only compute bounds. */
static void trim_ascii(const char* s, const char** out_start, const char** out_end) {
    const char* a = s;
    while (*a == ' ' || *a == '\t' || *a == '\n' || *a == '\r' || *a == '\f' || *a == '\v') a++;
    const char* b = a + strlen(a);
    while (b > a) {
        char c = b[-1];
        if (c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f' || c == '\v') b--;
        else break;
    }
    *out_start = a;
    *out_end = b;
}

int64_t jrt_int_any(int64_t val) {
    jade_value_t v = (jade_value_t)val;
    if (jrt_is_int(v))   return val;
    if (jrt_is_float(v)) return jrt_box_int((int64_t)jrt_unbox_float(v));
    if (jrt_is_bool(v))  return jrt_box_int(jrt_unbox_bool(v) ? 1 : 0);
    if (jrt_is_str(v)) {
        const char* s = (const char*)jrt_unbox_ptr(v);
        const char *a, *b;
        trim_ascii(s, &a, &b);
        if (b > a) {
            /* strtoll on a NUL-terminated copy of the trimmed span; require it
             * to consume the whole span (Rust's str::parse is whole-string). */
            char tmp[64];
            size_t n = (size_t)(b - a);
            if (n < sizeof tmp) {
                memcpy(tmp, a, n); tmp[n] = '\0';
                char* end = NULL;
                errno = 0;
                long long r = strtoll(tmp, &end, 10);
                if (errno == 0 && end == tmp + n) return jrt_box_int((int64_t)r);
            }
        }
        char msg[96];
        snprintf(msg, sizeof msg, "int(): cannot convert \"%s\" to int", s);
        throw_msg(msg);
    }
    throw_msg("int(): cannot convert value to int");
    return JRT_NIL; /* unreachable (throw_msg longjmps) */
}

int64_t jrt_float_any(int64_t val) {
    jade_value_t v = (jade_value_t)val;
    if (jrt_is_float(v)) return val;
    if (jrt_is_int(v))   return jrt_box_float((double)jrt_unbox_int(v));
    if (jrt_is_bool(v))  return jrt_box_float(jrt_unbox_bool(v) ? 1.0 : 0.0);
    if (jrt_is_str(v)) {
        const char* s = (const char*)jrt_unbox_ptr(v);
        const char *a, *b;
        trim_ascii(s, &a, &b);
        if (b > a) {
            char tmp[64];
            size_t n = (size_t)(b - a);
            if (n < sizeof tmp) {
                memcpy(tmp, a, n); tmp[n] = '\0';
                char* end = NULL;
                errno = 0;
                double r = strtod(tmp, &end);
                if (end == tmp + n) return jrt_box_float(r);
            }
        }
        char msg[96];
        snprintf(msg, sizeof msg, "float(): cannot convert \"%s\" to float", s);
        throw_msg(msg);
    }
    throw_msg("float(): cannot convert value to float");
    return JRT_NIL; /* unreachable */
}

int64_t jrt_bool_any(int64_t val) {
    jade_value_t v = (jade_value_t)val;
    if (jrt_is_bool(v))  return val;
    if (jrt_is_int(v))   return jrt_box_bool(jrt_unbox_int(v) != 0);
    if (jrt_is_float(v)) return jrt_box_bool(jrt_unbox_float(v) != 0.0);
    if (jrt_is_nil(v))   return JRT_FALSE;
    if (jrt_is_str(v)) {
        const char* s = (const char*)jrt_unbox_ptr(v);
        /* case-insensitive "true"/"false"; ""→false; any other non-empty→true. */
        if (s[0] == '\0') return JRT_FALSE;
        if (strcasecmp(s, "true") == 0)  return JRT_TRUE;
        if (strcasecmp(s, "false") == 0) return JRT_FALSE;
        return JRT_TRUE;
    }
    /* any other heap object → true (only nil is false). */
    return JRT_TRUE;
}

char* jrt_str_of_any(int64_t val) {
    /* Render a type-erased value to a tagged string for f-string interpolation.
     * A real string is returned as-is (its trust byte at data[-1] is preserved);
     * a scalar formats via the shared jrt_snprintf_any as a TRUSTED string.
     * (A non-string heap object renders as jrt_snprintf_any's "<object>"
     * placeholder — the same ObjKind gap the Chunk backend documents.) */
    jade_value_t v = (jade_value_t)val;
    if (jrt_is_str(v)) return (char*)jrt_unbox_ptr(v);
    if (jrt_is_ptr(v)) {
        /* Render a kind-tagged collection into a fresh TRUSTED tagged string. */
        char* r = jrt_render_any(val);
        char* out = jrt_str_dup(r, JRT_TRUSTED);
        free(r);
        return out;
    }
    char buf[64];
    jrt_snprintf_any(buf, sizeof buf, val);
    return jrt_str_dup(buf, JRT_TRUSTED);
}

/* ── Kind-tagged heap objects (Chunk backend collections) ──────────────────
 * kind at offset 0, len at offset 8 (matches JrtArrayHdr so jrt_len_* work). */
typedef struct {
    int64_t  kind;   /* JK_ARRAY */
    int64_t  len;
    int64_t  cap;
    int64_t* data;   /* tagged element words */
} JKArray;

typedef struct { char* key; int64_t val; } JKDSlot;
typedef struct {
    int64_t   kind;   /* JK_DICT */
    int64_t   len;    /* entry count (offset 8) */
    int64_t   cap;
    JKDSlot*  slots;
} JKDict;

int64_t jrt_kind_of(void* p) { return ((JKArray*)p)->kind; }

void* jrt_karr_new(void) {
    JKArray* a = malloc(sizeof(JKArray));
    a->kind = JK_ARRAY;
    a->len = 0;
    a->cap = 0;
    a->data = NULL;
    return a;
}

void jrt_karr_push(void* ap, int64_t val) {
    JKArray* a = (JKArray*)ap;
    if (a->len >= a->cap) {
        int64_t nc = a->cap ? a->cap * 2 : 4;
        a->data = realloc(a->data, (size_t)nc * sizeof(int64_t));
        a->cap = nc;
    }
    a->data[a->len++] = val;
}

void* jrt_kdict_new(void) {
    JKDict* d = malloc(sizeof(JKDict));
    d->kind = JK_DICT;
    d->len = 0;
    d->cap = 0;
    d->slots = NULL;
    return d;
}

/* Set key→val, updating in place if present else appending (linear probe —
 * dicts are small). `key_word` is a tagged string; its bytes are copied. */
void jrt_kdict_set(void* dp, int64_t key_word, int64_t val) {
    JKDict* d = (JKDict*)dp;
    const char* k = (const char*)jrt_unbox_ptr((jade_value_t)key_word);
    for (int64_t i = 0; i < d->len; i++) {
        if (strcmp(d->slots[i].key, k) == 0) { d->slots[i].val = val; return; }
    }
    if (d->len >= d->cap) {
        int64_t nc = d->cap ? d->cap * 2 : 4;
        d->slots = realloc(d->slots, (size_t)nc * sizeof(JKDSlot));
        d->cap = nc;
    }
    size_t kl = strlen(k);
    char* kc = malloc(kl + 1);
    memcpy(kc, k, kl + 1);
    d->slots[d->len].key = kc;
    d->slots[d->len].val = val;
    d->len++;
}

/* Shallow copy (VM dict value semantics: assignment/mutation clones the map;
 * values are shared words). Keys are re-duplicated so frees stay independent. */
static JKDict* jk_dict_copy(JKDict* s) {
    JKDict* d = malloc(sizeof(JKDict));
    d->kind = JK_DICT;
    d->len = s->len;
    d->cap = s->len ? s->len : 0;
    d->slots = s->len ? malloc((size_t)s->len * sizeof(JKDSlot)) : NULL;
    for (int64_t i = 0; i < s->len; i++) {
        size_t kl = strlen(s->slots[i].key);
        char* kc = malloc(kl + 1);
        memcpy(kc, s->slots[i].key, kl + 1);
        d->slots[i].key = kc;
        d->slots[i].val = s->slots[i].val;
    }
    return d;
}

typedef struct {
    int64_t  kind;        /* JK_STRUCT */
    int64_t  len;         /* field count (offset 8) */
    int64_t  cap;
    char*    type_name;
    JKDSlot* fields;
} JKStruct;

void* jrt_kstruct_new(const char* type_name) {
    JKStruct* s = malloc(sizeof(JKStruct));
    s->kind = JK_STRUCT;
    s->len = 0;
    s->cap = 0;
    s->fields = NULL;
    size_t n = strlen(type_name);
    s->type_name = malloc(n + 1);
    memcpy(s->type_name, type_name, n + 1);
    return s;
}

void jrt_kstruct_set(void* sp, const char* field, int64_t val) {
    JKStruct* s = (JKStruct*)sp;
    for (int64_t i = 0; i < s->len; i++) {
        if (strcmp(s->fields[i].key, field) == 0) { s->fields[i].val = val; return; }
    }
    if (s->len >= s->cap) {
        int64_t nc = s->cap ? s->cap * 2 : 4;
        s->fields = realloc(s->fields, (size_t)nc * sizeof(JKDSlot));
        s->cap = nc;
    }
    size_t n = strlen(field);
    char* fc = malloc(n + 1);
    memcpy(fc, field, n + 1);
    s->fields[s->len].key = fc;
    s->fields[s->len].val = val;
    s->len++;
}

int64_t jrt_get_field(int64_t obj, const char* field) {
    jade_value_t v = (jade_value_t)obj;
    if (jrt_is_ptr(v)) {
        void* p = jrt_unbox_ptr(v);
        if (((JKArray*)p)->kind == JK_STRUCT) {
            JKStruct* s = (JKStruct*)p;
            for (int64_t i = 0; i < s->len; i++) {
                if (strcmp(s->fields[i].key, field) == 0) return s->fields[i].val;
            }
            throw_msg("undefined field");
        }
    }
    throw_msg("value has no fields");
    return JRT_NIL;
}

void jrt_set_field(int64_t obj, const char* field, int64_t val) {
    jade_value_t v = (jade_value_t)obj;
    if (jrt_is_ptr(v)) {
        void* p = jrt_unbox_ptr(v);
        if (((JKArray*)p)->kind == JK_STRUCT) { jrt_kstruct_set(p, field, val); return; }
    }
    throw_msg("value does not support field assignment");
}

char* jrt_get_type_name(int64_t obj) {
    jade_value_t v = (jade_value_t)obj;
    if (jrt_is_ptr(v)) {
        void* p = jrt_unbox_ptr(v);
        if (((JKArray*)p)->kind == JK_STRUCT) {
            return jrt_str_dup(((JKStruct*)p)->type_name, JRT_TRUSTED);
        }
    }
    return jrt_str_dup("", JRT_TRUSTED);
}

/* i-th UTF-8 codepoint of a tagged string as a fresh tagged string (VM char
 * indexing), preserving the source's trust byte; raises on out-of-range. */
static int64_t jk_str_index(int64_t obj, int64_t idx) {
    if (!jrt_is_int((jade_value_t)idx)) throw_msg("string index must be int");
    int64_t i = jrt_unbox_int((jade_value_t)idx);
    const char* s = (const char*)jrt_unbox_ptr((jade_value_t)obj);
    if (i < 0) throw_msg("string index out of bounds");
    const char* p = s;
    int64_t n = 0;
    while (*p) {
        unsigned char c = (unsigned char)*p;
        int step = (c >= 0xF0) ? 4 : (c >= 0xE0) ? 3 : (c >= 0xC0) ? 2 : 1;
        if (n == i) {
            uint8_t trust = jrt_trust_of(s);
            char* out = jrt_str_new(step, trust);
            memcpy(out, p, (size_t)step);
            out[step] = '\0';
            return (int64_t)jrt_box_str(out);
        }
        p += step;
        n++;
    }
    throw_msg("string index out of bounds");
    return JRT_NIL;
}

int64_t jrt_val_index(int64_t obj, int64_t idx) {
    jade_value_t v = (jade_value_t)obj;
    if (jrt_is_str(v)) return jk_str_index(obj, idx);
    if (jrt_is_ptr(v)) {
        void* p = jrt_unbox_ptr(v);
        int64_t kind = ((JKArray*)p)->kind;
        if (kind == JK_ARRAY) {
            if (!jrt_is_int((jade_value_t)idx)) throw_msg("array index must be int");
            int64_t i = jrt_unbox_int((jade_value_t)idx);
            JKArray* a = (JKArray*)p;
            if (i < 0 || i >= a->len) throw_msg("index out of bounds");
            return a->data[i];
        }
        if (kind == JK_DICT) {
            if (!jrt_is_str((jade_value_t)idx)) throw_msg("dict index must be str");
            const char* k = (const char*)jrt_unbox_ptr((jade_value_t)idx);
            JKDict* d = (JKDict*)p;
            for (int64_t i = 0; i < d->len; i++) {
                if (strcmp(d->slots[i].key, k) == 0) return d->slots[i].val;
            }
            throw_msg("key not found");
        }
    }
    throw_msg("value is not indexable");
    return JRT_NIL;
}

int64_t jrt_val_set_index(int64_t obj, int64_t idx, int64_t val) {
    jade_value_t v = (jade_value_t)obj;
    if (jrt_is_ptr(v)) {
        void* p = jrt_unbox_ptr(v);
        int64_t kind = ((JKArray*)p)->kind;
        if (kind == JK_ARRAY) {
            /* Arrays are reference-semantic (VM Arc): mutate in place. */
            if (!jrt_is_int((jade_value_t)idx)) throw_msg("array index must be int");
            int64_t i = jrt_unbox_int((jade_value_t)idx);
            JKArray* a = (JKArray*)p;
            if (i < 0 || i >= a->len) throw_msg("index out of bounds");
            a->data[i] = val;
            return obj;
        }
        if (kind == JK_DICT) {
            /* Dicts are value-semantic (VM clones on mutation): copy then set,
             * and return the new container so the caller rebinds the variable. */
            if (!jrt_is_str((jade_value_t)idx)) throw_msg("dict index must be str");
            JKDict* d = jk_dict_copy((JKDict*)p);
            jrt_kdict_set(d, idx, val);
            return (int64_t)jrt_box_ptr(d);
        }
    }
    throw_msg("value does not support index assignment");
    return obj;
}

/* Growable byte buffer for the recursive renderer. */
typedef struct { char* buf; size_t len; size_t cap; } JKSB;
static void jksb_puts(JKSB* s, const char* str) {
    size_t n = strlen(str);
    if (s->len + n + 1 > s->cap) {
        while (s->len + n + 1 > s->cap) s->cap *= 2;
        s->buf = realloc(s->buf, s->cap);
    }
    memcpy(s->buf + s->len, str, n);
    s->len += n;
}
/* Append a string as a Rust-`{:?}`-style quoted literal (used for dict keys). */
static void jksb_quoted(JKSB* s, const char* k) {
    jksb_puts(s, "\"");
    char esc[2] = {0, 0};
    for (const char* p = k; *p; p++) {
        switch (*p) {
            case '"':  jksb_puts(s, "\\\""); break;
            case '\\': jksb_puts(s, "\\\\"); break;
            case '\n': jksb_puts(s, "\\n"); break;
            case '\t': jksb_puts(s, "\\t"); break;
            case '\r': jksb_puts(s, "\\r"); break;
            default:   esc[0] = *p; jksb_puts(s, esc); break;
        }
    }
    jksb_puts(s, "\"");
}

static void jk_render(JKSB* s, int64_t val) {
    jade_value_t v = (jade_value_t)val;
    if (jrt_is_str(v)) { jksb_puts(s, (const char*)jrt_unbox_ptr(v)); return; }
    if (jrt_is_ptr(v)) {
        void* p = jrt_unbox_ptr(v);
        int64_t kind = ((JKArray*)p)->kind;
        if (kind == JK_ARRAY) {
            JKArray* a = (JKArray*)p;
            jksb_puts(s, "[");
            for (int64_t i = 0; i < a->len; i++) {
                if (i) jksb_puts(s, ", ");
                jk_render(s, a->data[i]);
            }
            jksb_puts(s, "]");
            return;
        }
        if (kind == JK_DICT) {
            /* `{"k": v, …}` with keys sorted ascending (VM value_to_display). */
            JKDict* d = (JKDict*)p;
            int64_t n = d->len;
            int64_t* order = malloc((size_t)(n > 0 ? n : 1) * sizeof(int64_t));
            for (int64_t i = 0; i < n; i++) order[i] = i;
            for (int64_t i = 1; i < n; i++) {   /* insertion sort by key */
                int64_t j = i, cur = order[i];
                while (j > 0 && strcmp(d->slots[order[j - 1]].key, d->slots[cur].key) > 0) {
                    order[j] = order[j - 1];
                    j--;
                }
                order[j] = cur;
            }
            jksb_puts(s, "{");
            for (int64_t i = 0; i < n; i++) {
                if (i) jksb_puts(s, ", ");
                jksb_quoted(s, d->slots[order[i]].key);
                jksb_puts(s, ": ");
                jk_render(s, d->slots[order[i]].val);
            }
            jksb_puts(s, "}");
            free(order);
            return;
        }
        if (kind == JK_STRUCT) { jksb_puts(s, "<struct>"); return; }
        jksb_puts(s, "<object>");
        return;
    }
    char tmp[64];
    jrt_snprintf_any(tmp, sizeof tmp, val);
    jksb_puts(s, tmp);
}
char* jrt_render_any(int64_t val) {
    JKSB s;
    s.cap = 64;
    s.len = 0;
    s.buf = malloc(s.cap);
    jk_render(&s, val);
    s.buf[s.len] = '\0';
    return s.buf;
}

int32_t jrt_in_any(int64_t needle, int64_t haystack) {
    jade_value_t h = (jade_value_t)haystack;
    if (jrt_is_str(h)) {
        if (!jrt_is_str((jade_value_t)needle)) throw_msg("'in' substring must be str");
        const char* s = (const char*)jrt_unbox_ptr(h);
        const char* sub = (const char*)jrt_unbox_ptr((jade_value_t)needle);
        return strstr(s, sub) != NULL ? 1 : 0;
    }
    if (jrt_is_ptr(h)) {
        void* p = jrt_unbox_ptr(h);
        int64_t kind = ((JKArray*)p)->kind;
        if (kind == JK_ARRAY) {
            JKArray* a = (JKArray*)p;
            for (int64_t i = 0; i < a->len; i++) {
                if (jrt_eq_any((uint64_t)a->data[i], (uint64_t)needle)) return 1;
            }
            return 0;
        }
        if (kind == JK_DICT) {
            if (!jrt_is_str((jade_value_t)needle)) throw_msg("'in' dict key must be str");
            const char* k = (const char*)jrt_unbox_ptr((jade_value_t)needle);
            JKDict* d = (JKDict*)p;
            for (int64_t i = 0; i < d->len; i++) {
                if (strcmp(d->slots[i].key, k) == 0) return 1;
            }
            return 0;
        }
    }
    throw_msg("'in' requires array, dict, or str");
    return 0;
}

int jrt_snprintf_float(char* buf, size_t cap, double val) {
    char tmp[64];
    if (isnan(val)) {
        snprintf(tmp, sizeof tmp, "NaN");
    } else if (isinf(val)) {
        snprintf(tmp, sizeof tmp, val < 0 ? "-inf" : "inf");
    } else {
        /* Match the VM's display: the shortest decimal that round-trips to the
         * same double (mirrors Rust's `{}` for f64). Grow precision until the
         * formatted text re-parses exactly. */
        int prec = 1;
        for (; prec < 17; prec++) {
            snprintf(tmp, sizeof tmp, "%.*g", prec, val);
            if (strtod(tmp, NULL) == val) break;
        }
        snprintf(tmp, sizeof tmp, "%.*g", prec, val);
        /* Integer-valued floats print with a trailing ".0", like the VM.
         * (%g uses exponent form for very large/small magnitudes — those keep
         * the 'e' and are left as-is.) */
        if (!strpbrk(tmp, ".eEnN")) {
            size_t l = strlen(tmp);
            if (l + 2 < sizeof tmp) {
                tmp[l] = '.'; tmp[l + 1] = '0'; tmp[l + 2] = '\0';
            }
        }
    }
    return snprintf(buf, cap, "%s", tmp);
}

/* jrt_ipow moved to the shared Rust runtime crate (jade-runtime, src/num.rs);
 * declaration remains in runtime.h and calls resolve against the staticlib. */

/* ── Tag-erased arithmetic / comparison (statically-Unknown operands) ──────
 * The dispatch logic (int op int stays int unless it must widen, any float
 * promotes, `+` concatenates strings, div/mod-by-zero and non-numeric operands
 * error) now lives ONCE in the shared Rust runtime crate (jade-runtime,
 * src/ops.rs). The `jrt_core_*` ops return an error CODE via an out-param
 * instead of raising — a Rust frame cannot be crossed by longjmp — so the
 * functions below are thin C forwarders that translate a code into a catchable
 * exception here, keeping the longjmp entirely on the C side. jrt_eq_any and
 * jrt_to_bool never raise, so Rust exports them directly (see ffi.rs). */

/* Raise "<op> requires numeric operands" (matches the VM's TypeError text). */
static void throw_num_type(const char* op) {
    char msg[64];
    snprintf(msg, sizeof msg, "%s requires numeric operands", op);
    jade_exc_throw_typed(jrt_box_str(jrt_str_dup(msg, JRT_TRUSTED)), NULL);
}

static void throw_msg(const char* m) {
    jade_exc_throw_typed(jrt_box_str(jrt_str_dup(m, JRT_TRUSTED)), NULL);
}

/* Translate a jrt_core_* error code into the matching raise (no-op on OK). */
static void throw_op_err(uint32_t err, const char* op) {
    if (err == JRT_OP_DIVZERO)       throw_msg("division by zero");
    else if (err == JRT_OP_REMZERO)  throw_msg("modulo by zero");
    else if (err == JRT_OP_OVERFLOW) throw_msg("integer overflow");
    else if (err == JRT_OP_TYPE)     throw_num_type(op);
}

double jrt_any_to_double(jade_value_t v) {
    uint32_t err = JRT_OP_OK;
    double d = jrt_core_to_double(v, &err);
    if (err) throw_op_err(err, "math");
    return d;
}

jade_value_t jrt_add_any(jade_value_t a, jade_value_t b) {
    uint32_t err = JRT_OP_OK;
    jade_value_t r = jrt_core_add(a, b, &err);
    if (err) throw_op_err(err, "'+'");
    return r;
}

jade_value_t jrt_sub_any(jade_value_t a, jade_value_t b) {
    uint32_t err = JRT_OP_OK;
    jade_value_t r = jrt_core_sub(a, b, &err);
    if (err) throw_op_err(err, "'-'");
    return r;
}

jade_value_t jrt_mul_any(jade_value_t a, jade_value_t b) {
    uint32_t err = JRT_OP_OK;
    jade_value_t r = jrt_core_mul(a, b, &err);
    if (err) throw_op_err(err, "'*'");
    return r;
}

jade_value_t jrt_div_any(jade_value_t a, jade_value_t b) {
    uint32_t err = JRT_OP_OK;
    jade_value_t r = jrt_core_div(a, b, &err);
    if (err) throw_op_err(err, "'/'");
    return r;
}

jade_value_t jrt_mod_any(jade_value_t a, jade_value_t b) {
    uint32_t err = JRT_OP_OK;
    jade_value_t r = jrt_core_mod(a, b, &err);
    if (err) throw_op_err(err, "'%'");
    return r;
}

jade_value_t jrt_pow_any(jade_value_t a, jade_value_t b) {
    uint32_t err = JRT_OP_OK;
    jade_value_t r = jrt_core_pow(a, b, &err);
    if (err) throw_op_err(err, "'**'");
    return r;
}

jade_value_t jrt_neg_any(jade_value_t a) {
    uint32_t err = JRT_OP_OK;
    jade_value_t r = jrt_core_neg(a, &err);
    if (err) throw_op_err(err, "'-'");
    return r;
}

int jrt_cmp_any(jade_value_t a, jade_value_t b) {
    uint32_t err = JRT_OP_OK;
    int c = jrt_core_cmp(a, b, &err);
    if (err) throw_op_err(err, "comparison");
    return c;
}

/* Dynamic ==/!=. Now VM-strict: cross-kind operands (e.g. int vs float) raise a
 * catchable TypeError instead of silently comparing. codegen calls this for
 * both == and != (negating the result), so both raise on a kind mismatch. */
int jrt_eq_any(uint64_t a, uint64_t b) {
    uint32_t err = JRT_OP_OK;
    int r = jrt_core_eq((jade_value_t)a, (jade_value_t)b, &err);
    if (err) throw_op_err(err, "'=='");
    return r;
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
    if (!d) jade_rt_fatal("jade: out of memory");
    d->slots = calloc(JADE_DICT_INIT_CAP, sizeof(JadeDictSlot));
    if (!d->slots) jade_rt_fatal("jade: out of memory");
    d->cap = JADE_DICT_INIT_CAP; d->count = 0;
    return d;
}

void jade_dict_set(void* dict, const char* key, int64_t val) {
    JadeDict* d = (JadeDict*)dict;
    if (d->count * 4 >= d->cap * 3) { if (!dict_grow(d)) jade_rt_fatal("jade: out of memory"); }
    uint64_t idx = dict_hash(key) % (uint64_t)d->cap;
    while (d->slots[idx].key && strcmp(d->slots[idx].key, key) != 0)
        idx = (idx + 1) % (uint64_t)d->cap;
    if (!d->slots[idx].key) {
        d->slots[idx].key = strdup(key); if (!d->slots[idx].key) jade_rt_fatal("jade: out of memory");
        d->count++;
    }
    d->slots[idx].val = val;
}

/* Look up `key`; returns its (tagged) value, or JRT_NIL if absent. (Used by the
 * Unknown-field fallback path, which must not raise.) */
int64_t jade_dict_get(void* dict, const char* key) {
    JadeDict* d = (JadeDict*)dict;
    uint64_t idx = dict_hash(key) % (uint64_t)d->cap;
    for (int64_t i = 0; i < d->cap; i++) {
        if (!d->slots[idx].key) return JRT_NIL;
        if (strcmp(d->slots[idx].key, key) == 0) return d->slots[idx].val;
        idx = (idx + 1) % (uint64_t)d->cap;
    }
    return JRT_NIL;
}

/* Like jade_dict_get, but RAISES a catchable runtime error on a missing key —
 * matching the VM's `d[k]` semantics ("key 'X' not found in dict"). The error
 * value is a TRUSTED string so `catch e { print(e) }` shows the message. */
int64_t jade_dict_get_checked(void* dict, const char* key) {
    JadeDict* d = (JadeDict*)dict;
    uint64_t idx = dict_hash(key) % (uint64_t)d->cap;
    for (int64_t i = 0; i < d->cap; i++) {
        if (!d->slots[idx].key) break;
        if (strcmp(d->slots[idx].key, key) == 0) return d->slots[idx].val;
        idx = (idx + 1) % (uint64_t)d->cap;
    }
    char msg[256];
    snprintf(msg, sizeof msg, "key '%s' not found in dict", key ? key : "");
    jade_exc_throw_typed(jrt_box_str(jrt_str_dup(msg, JRT_TRUSTED)), NULL);
    return JRT_NIL;  /* unreachable */
}

int64_t jade_dict_len(void* dict) { return ((JadeDict*)dict)->count; }

/* Shallow-copy a dict into a fresh one (keys are re-strdup'd by jade_dict_set;
 * values — including string/dict pointers — are shared). Implements the VM's
 * dict value semantics on assignment: dicts are owned HashMaps, so binding one
 * to a new name copies it (mutating one must not affect the other). Arrays are
 * Arc-shared in the VM and are intentionally NOT copied. (A nested dict value
 * is shared, not deep-copied — the untyped uniform value model can't tell a
 * dict pointer from a string pointer; flat dicts, the eval cases, are exact.) */
void* jade_dict_copy(void* src) {
    JadeDict* s = (JadeDict*)src;
    void* dst = jade_dict_create();
    if (!s) return dst;
    for (int64_t i = 0; i < s->cap; i++) {
        if (s->slots[i].key) jade_dict_set(dst, s->slots[i].key, s->slots[i].val);
    }
    return dst;
}

int32_t jade_dict_has(void* dict, const char* key) {
    JadeDict* d = (JadeDict*)dict;
    uint64_t idx = dict_hash(key) % (uint64_t)d->cap;
    for (int64_t i = 0; i < d->cap; i++) {
        if (!d->slots[idx].key) return 0;
        if (strcmp(d->slots[idx].key, key) == 0) return 1;
        idx = (idx + 1) % (uint64_t)d->cap;
    }
    return 0;
}

void jade_dict_free(void* dict) {
    JadeDict* d = (JadeDict*)dict;
    for (int64_t i = 0; i < d->cap; i++) if (d->slots[i].key) free(d->slots[i].key);
    free(d->slots); free(d);
}

/* ── Dict primitive methods (d.keys() / d.values() / d.merge()) ──────────
 * keys() and values() return arrays sorted by key, matching the VM
 * (dict_pkg.rs sorts keys / sorts pairs by key) — so iteration order is
 * deterministic and difftest-stable despite the hash table's internal order. */

static int dict_key_strcmp(const void* a, const void* b) {
    return strcmp(*(const char* const*)a, *(const char* const*)b);
}

void* jrt_dict_keys(void* dict) {
    void* arr = jrt_array_new();
    JadeDict* d = (JadeDict*)dict;
    if (!d || d->count == 0) return arr;
    const char** ks = malloc((size_t)d->count * sizeof(char*));
    if (!ks) jade_rt_fatal("jade: out of memory");
    int64_t n = 0;
    for (int64_t i = 0; i < d->cap; i++) if (d->slots[i].key) ks[n++] = d->slots[i].key;
    qsort(ks, (size_t)n, sizeof(char*), dict_key_strcmp);
    for (int64_t i = 0; i < n; i++)
        jrt_array_push(arr, jrt_box_str(jrt_str_dup(ks[i], JRT_TRUSTED)));
    free(ks);
    return arr;
}

typedef struct { const char* k; int64_t v; } JadeKV;
static int dict_kv_keycmp(const void* a, const void* b) {
    return strcmp(((const JadeKV*)a)->k, ((const JadeKV*)b)->k);
}

void* jrt_dict_values(void* dict) {
    void* arr = jrt_array_new();
    JadeDict* d = (JadeDict*)dict;
    if (!d || d->count == 0) return arr;
    JadeKV* kv = malloc((size_t)d->count * sizeof(JadeKV));
    if (!kv) jade_rt_fatal("jade: out of memory");
    int64_t n = 0;
    for (int64_t i = 0; i < d->cap; i++)
        if (d->slots[i].key) { kv[n].k = d->slots[i].key; kv[n].v = d->slots[i].val; n++; }
    qsort(kv, (size_t)n, sizeof(JadeKV), dict_kv_keycmp);
    for (int64_t i = 0; i < n; i++) jrt_array_push(arr, kv[i].v);  /* values stay tagged */
    free(kv);
    return arr;
}

/* d1.merge(d2) — new dict with d1's entries then d2's (d2 wins on conflict).
 * Mirrors the VM's dict.merge (returns a fresh Dict; inputs unchanged). */
void* jrt_dict_merge(void* d1, void* d2) {
    void* out = jade_dict_copy(d1);
    JadeDict* s = (JadeDict*)d2;
    if (s) for (int64_t i = 0; i < s->cap; i++)
        if (s->slots[i].key) jade_dict_set(out, s->slots[i].key, s->slots[i].val);
    return out;
}

/* ── Exceptions ───────────────────────────────────────────────────────── */

#define JADE_EXC_MAX_DEPTH 64

static _Thread_local void*       exc_stack[JADE_EXC_MAX_DEPTH];
static _Thread_local int64_t     exc_thrown_value;
static _Thread_local const char* exc_thrown_type;   /* struct type name, or NULL */
static _Thread_local int         exc_depth = 0;

void jade_exc_push_frame(void* jmpbuf) {
    if (exc_depth >= JADE_EXC_MAX_DEPTH) {
        jade_rt_fatal("jade: exception stack overflow");
    }
    exc_stack[exc_depth++] = jmpbuf;
}

void jade_exc_pop(void) { if (exc_depth > 0) exc_depth--; }

/* Throw with an explicit static type name. `type` is the struct type name for
 * `raise SomeStruct {...}`, or NULL for primitives/strings. The catch site
 * compares this stored name against `catch <Type>` arms instead of blindly
 * dereferencing the thrown value as a struct (which segfaults for non-structs). */
void jade_exc_throw_typed(int64_t value, const char* type) {
    if (exc_depth == 0) {
        /* No active handler: surface the thrown value so an uncaught error is
         * EXPLICIT (like the VM), not a generic "unhandled exception". A string
         * value is the error message; otherwise name the thrown type if known. */
        if (jrt_is_str((jade_value_t)value)) {
            char buf[1056];
            snprintf(buf, sizeof buf, "jade: %s",
                     (const char*)jrt_unbox_ptr((jade_value_t)value));
            jade_rt_fatal(buf);
        }
        if (type) {
            char buf[256];
            snprintf(buf, sizeof buf, "jade: uncaught %s", type);
            jade_rt_fatal(buf);
        }
        jade_rt_fatal("jade: unhandled exception");
    }
    exc_thrown_value = value;
    exc_thrown_type  = type;
    void* buf = exc_stack[--exc_depth];
    longjmp(*(jmp_buf*)buf, 1);
}

void jade_exc_throw(int64_t value) { jade_exc_throw_typed(value, NULL); }

int64_t     jade_exc_value(void) { return exc_thrown_value; }
const char* jade_exc_type(void)  { return exc_thrown_type;  }

#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
#include <time.h>
#include <sys/wait.h>
#include <sys/stat.h>
#include <dirent.h>
#include <poll.h>


/* ── jade_array header (matches LLVM %jade.array = {ptr,i64,i64}) ── */
typedef struct { void* data; int64_t len; int64_t cap; } JrtArrayHdr;

/* len() on a value whose static type is Unknown: dispatch on the runtime tag.
 * A string → byte length; any other heap pointer → array .len field. (Dicts
 * and arrays share the non-string heap tag, so len() on an Unknown-typed dict
 * isn't distinguishable here — a known limitation; use a typed receiver.) */
int64_t jrt_len_any(jade_value_t v) {
    if (jrt_is_str(v)) return (int64_t)strlen((const char*)jrt_unbox_ptr(v));
    if (jrt_is_ptr(v)) return ((JrtArrayHdr*)jrt_unbox_ptr(v))->len;
    return 0;
}

/* len() of a statically-Unknown value (codegen emit_len). Like jrt_len_any for
 * strings (strlen), but EVERY non-string — array/dict-tagged OR a raw untagged
 * heap pointer (a stdlib Ret::Ptr result, e.g. fs.list_dir / env.cwd, stored in
 * an Unknown slot as 8-aligned bits that read as the INT tag) — falls through to
 * the array-header .len at offset 8. This preserves the long-standing offset-8
 * behavior for non-strings while giving a correct length for tagged strings such
 * as llm.tool_grammar(). NULL → 0. */
int64_t jrt_len_unknown(int64_t val) {
    jade_value_t v = (jade_value_t)val;
    if (jrt_is_str(v)) return (int64_t)strlen((const char*)jrt_unbox_ptr(v));
    void* p = jrt_unbox_ptr(v);
    if (!p) return 0;
    return ((JrtArrayHdr*)p)->len;
}

/* ── String methods ─────────────────────────────────────────────── */

int32_t jrt_str_contains(const char* haystack, const char* needle) {
    if (!haystack || !needle) return 0;
    return strstr(haystack, needle) != NULL ? 1 : 0;
}

void* jrt_str_split(const char* str, const char* delim) {
    uint8_t t = jrt_trust_of(str);
    if (!str || !delim || *delim == '\0') {
        JrtArrayHdr* h = malloc(sizeof(JrtArrayHdr));
        if (!h) return NULL;
        int64_t* data = malloc(sizeof(int64_t));
        if (!data) { free(h); return NULL; }
        data[0] = jrt_box_str(jrt_str_dup(str ? str : "", t));
        h->data = data; h->len = 1; h->cap = 1;
        return h;
    }
    size_t dlen = strlen(delim);
    int count = 1;
    const char* p = str;
    while ((p = strstr(p, delim)) != NULL) { count++; p += dlen; }

    JrtArrayHdr* h = malloc(sizeof(JrtArrayHdr));
    if (!h) return NULL;
    int64_t* data = malloc((size_t)count * sizeof(int64_t));
    if (!data) { free(h); return NULL; }

    const char* start = str; int idx = 0;
    while ((p = strstr(start, delim)) != NULL) {
        size_t seg = (size_t)(p - start);
        char* part = jrt_str_new(seg, t);
        if (seg > 0) memcpy(part, start, seg);
        data[idx++] = jrt_box_str(part);
        start = p + dlen;
    }
    data[idx++] = jrt_box_str(jrt_str_dup(start, t));
    h->data = data; h->len = count; h->cap = count;
    return h;
}

char* jrt_str_trim(const char* str) {
    uint8_t t = jrt_trust_of(str);
    if (!str) return jrt_str_dup("", t);
    while (*str && isspace((unsigned char)*str)) str++;
    size_t n = strlen(str);
    while (n > 0 && isspace((unsigned char)str[n-1])) n--;
    char* out = jrt_str_new(n, t);
    if (n > 0) memcpy(out, str, n);
    return out;
}

/* str.upper / str.lower — ASCII case conversion, preserving the source trust.
 * (The VM uses Unicode to_uppercase/to_lowercase; for ASCII — the case the eval
 * suite and programs cover — these match. Non-ASCII bytes are passed through.) */
char* jrt_str_upper(const char* str) {
    uint8_t t = jrt_trust_of(str);
    size_t n = str ? strlen(str) : 0;
    char* out = jrt_str_new(n, t);
    for (size_t i = 0; i < n; i++) out[i] = (char)toupper((unsigned char)str[i]);
    return out;
}
char* jrt_str_lower(const char* str) {
    uint8_t t = jrt_trust_of(str);
    size_t n = str ? strlen(str) : 0;
    char* out = jrt_str_new(n, t);
    for (size_t i = 0; i < n; i++) out[i] = (char)tolower((unsigned char)str[i]);
    return out;
}

/* str.starts_with / str.ends_with — return 1/0. */
int32_t jrt_str_starts_with(const char* str, const char* prefix) {
    if (!str || !prefix) return 0;
    size_t lp = strlen(prefix);
    return strncmp(str, prefix, lp) == 0 ? 1 : 0;
}
int32_t jrt_str_ends_with(const char* str, const char* suffix) {
    if (!str || !suffix) return 0;
    size_t ls = strlen(str), lf = strlen(suffix);
    if (lf > ls) return 0;
    return memcmp(str + ls - lf, suffix, lf) == 0 ? 1 : 0;
}

char* jrt_str_replace(const char* str, const char* from, const char* to) {
    uint8_t t = jrt_trust_of(str) | jrt_trust_of(to);
    if (!str || !from || !to || *from == '\0') return jrt_str_dup(str ? str : "", t);
    size_t flen = strlen(from), tlen = strlen(to);
    /* Two-pass: count occurrences, compute exact size, allocate once. */
    size_t occ = 0;
    {
        const char* p = str;
        const char* f;
        while ((f = strstr(p, from)) != NULL) { occ++; p = f + flen; }
    }
    size_t base = strlen(str);
    size_t total = base + occ * tlen - occ * flen;
    char* out = jrt_str_new(total, t);
    size_t pos = 0;
    const char* p = str;
    const char* f;
    while ((f = strstr(p, from)) != NULL) {
        size_t pre = (size_t)(f - p);
        if (pre > 0) memcpy(out + pos, p, pre);
        pos += pre;
        if (tlen > 0) memcpy(out + pos, to, tlen);
        pos += tlen;
        p = f + flen;
    }
    size_t rest = strlen(p);
    if (rest > 0) memcpy(out + pos, p, rest);
    return out;
}

/* ── Array methods ──────────────────────────────────────────────── */

/* Create an empty jade_array header. jrt_array_push grows it from cap 0.
 * Returns NULL on OOM. Box with jrt_box_ptr to hand it back as a value. */
void* jrt_array_new(void) {
    JrtArrayHdr* h = malloc(sizeof(JrtArrayHdr));
    if (!h) return NULL;
    h->data = NULL; h->len = 0; h->cap = 0;
    return h;
}

void jrt_array_push(void* arr, int64_t val) {
    if (!arr) return;
    JrtArrayHdr* h = (JrtArrayHdr*)arr;
    if (h->len >= h->cap) {
        int64_t nc = h->cap == 0 ? 8 : h->cap * 2;
        void* nd = realloc(h->data, (size_t)nc * sizeof(int64_t));
        if (!nd) return;
        h->data = nd; h->cap = nc;
    }
    ((int64_t*)h->data)[h->len++] = val;
}

int64_t jrt_array_pop(void* arr) {
    if (!arr) return JRT_NIL;
    JrtArrayHdr* h = (JrtArrayHdr*)arr;
    if (h->len == 0) return JRT_NIL;  /* VM yields nil on empty pop */
    return ((int64_t*)h->data)[--h->len];
}

/* Element count of a jade_array (NULL → 0). */
int64_t jrt_array_len(void* arr) {
    return arr ? ((JrtArrayHdr*)arr)->len : 0;
}

/* Element `i` as a tagged value; JRT_NIL if out of range. */
int64_t jrt_array_get(void* arr, int64_t i) {
    if (!arr) return JRT_NIL;
    JrtArrayHdr* h = (JrtArrayHdr*)arr;
    if (i < 0 || i >= h->len) return JRT_NIL;
    return ((int64_t*)h->data)[i];
}

/* Store tagged `val` at element `i` (no-op if out of range). */
void jrt_array_set(void* arr, int64_t i, int64_t val) {
    if (!arr) return;
    JrtArrayHdr* h = (JrtArrayHdr*)arr;
    if (i >= 0 && i < h->len) ((int64_t*)h->data)[i] = val;
}

/* arr.reverse() — reverse in place (the VM's primitive method returns Nil). */
void jrt_array_reverse(void* arr) {
    if (!arr) return;
    JrtArrayHdr* h = (JrtArrayHdr*)arr;
    int64_t* d = (int64_t*)h->data;
    for (int64_t i = 0, j = h->len - 1; i < j; i++, j--) {
        int64_t t = d[i]; d[i] = d[j]; d[j] = t;
    }
}

/* arr.sort() — sort in place by the VM's value ordering (jrt_cmp_any: numeric
 * by value with int/float coercion, strings by bytes). The VM's primitive sort
 * mutates in place and returns Nil. qsort isn't stable, but equal primitives are
 * indistinguishable so observable output matches sort_by. */
static int array_sort_cmp(const void* a, const void* b) {
    return jrt_cmp_any((jade_value_t)*(const int64_t*)a, (jade_value_t)*(const int64_t*)b);
}
void jrt_array_sort(void* arr) {
    if (!arr) return;
    JrtArrayHdr* h = (JrtArrayHdr*)arr;
    if (h->len > 1) qsort(h->data, (size_t)h->len, sizeof(int64_t), array_sort_cmp);
}

/* Call a Jade function VALUE with one tagged argument and return its tagged
 * result. `fnval` is a jade_fn_t fat pointer { fn_ptr, env_ptr, name } (the
 * codegen layout). The uniform indirect ABI is `i64 (i64 arg, void* env)`; a
 * native-FFI value is flagged by fn_ptr == &jrt_native_call (env = {handle,
 * name}). This mirrors emit_indirect_call so closures, named fns, and native
 * packages all work as map/filter callbacks. A raise inside the callback
 * longjmps straight to the nearest Jade catch, unwinding through here. */
static int64_t jade_fn_call1(void* fnval, int64_t arg) {
    void** slots   = (void**)fnval;
    void*  fn_ptr  = slots[0];
    void*  env_ptr = slots[1];
    if (fn_ptr == (void*)jrt_native_call) {
        void*       handle = ((void**)env_ptr)[0];
        const char* name   = (const char*)((void**)env_ptr)[1];
        jade_value_t a = (jade_value_t)arg;
        return (int64_t)jrt_native_call(handle, name, &a, 1);
    }
    typedef int64_t (*JadeFn1)(int64_t, void*);
    return ((JadeFn1)fn_ptr)(arg, env_ptr);
}

/* array.map(arr, fn) — new array of fn(elem) for each element. Mirrors the VM's
 * NativeFnId::ArrayMap. */
void* jrt_array_map(void* arr, void* fnval) {
    void* out = jrt_array_new();
    int64_t n = jrt_array_len(arr);
    for (int64_t i = 0; i < n; i++) {
        jrt_array_push(out, jade_fn_call1(fnval, jrt_array_get(arr, i)));
    }
    return out;
}

/* array.filter(arr, fn) — elements for which fn(elem) is true. The predicate
 * must return a bool (matching the VM, which raises otherwise). */
void* jrt_array_filter(void* arr, void* fnval) {
    void* out = jrt_array_new();
    int64_t n = jrt_array_len(arr);
    for (int64_t i = 0; i < n; i++) {
        int64_t e = jrt_array_get(arr, i);
        int64_t r = jade_fn_call1(fnval, e);
        if (jrt_is_bool((jade_value_t)r)) {
            if (jrt_unbox_bool((jade_value_t)r)) jrt_array_push(out, e);
        } else {
            jade_exc_throw_typed(
                jrt_box_str(jrt_str_dup("array.filter: predicate must return a bool", JRT_TRUSTED)),
                NULL);
        }
    }
    return out;
}

/* ── JSON ───────────────────────────────────────────────────────── */

/* JSON-escape `s` into a heap-allocated `"quoted"` string. Caller frees. */
static char* json_escape(const char* s) {
    size_t n = strlen(s);
    char* out = malloc(n * 6 + 3); if (!out) return NULL;
    char* p = out; *p++ = '"';
    for (const unsigned char* c = (const unsigned char*)s; *c; c++) {
        if (*c == '"')       { *p++ = '\\'; *p++ = '"'; }
        else if (*c == '\\') { *p++ = '\\'; *p++ = '\\'; }
        else if (*c == '\n') { *p++ = '\\'; *p++ = 'n'; }
        else if (*c == '\r') { *p++ = '\\'; *p++ = 'r'; }
        else if (*c == '\t') { *p++ = '\\'; *p++ = 't'; }
        else if (*c < 0x20)  { p += sprintf(p, "\\u%04x", (unsigned)*c); }
        else                 { *p++ = (char)*c; }
    }
    *p++ = '"'; *p = '\0'; return out;
}

char* jrt_json_esc_str(const char* s) {
    uint8_t t = jrt_trust_of(s);
    if (!s) return jrt_str_dup("null", t);
    char* tmp = json_escape(s);
    if (!tmp) return jrt_str_dup("", t);
    char* out = jrt_str_dup(tmp, t);
    free(tmp);
    return out;
}

/* Build JSON for a single dict; returns plain malloc'd buffer + length and
 * accumulates trust of every string-typed value. Internal helper. */
static char* json_dict_to_str_plain(void* dict_ptr, size_t* out_len, uint8_t* trust_acc);

char* jrt_json_arr_dicts(int64_t arr_i64) {
    jade_value_t v = (jade_value_t)arr_i64;
    if (arr_i64 == 0 || jrt_is_nil(v)) return jrt_str_dup("null", JRT_TRUSTED);
    /* The value arrives tagged. A boxed float / bool / int renders as a JSON
     * scalar; otherwise it is a heap pointer to an array or a string. */
    if (jrt_is_float(v)) { char nb[64]; jrt_snprintf_float(nb, sizeof nb, jrt_unbox_float(v)); return jrt_str_dup(nb, JRT_TRUSTED); }
    if (jrt_is_bool(v))  return jrt_str_dup(jrt_unbox_bool(v) ? "true" : "false", JRT_TRUSTED);
    if (jrt_is_int(v))   { char nb[64]; snprintf(nb, sizeof nb, "%lld", (long long)jrt_unbox_int(v)); return jrt_str_dup(nb, JRT_TRUSTED); }
    /* A string value is JSON-escaped (the distinct string tag means we no longer
     * have to guess by reading a would-be array header — which was an OOB read
     * for short strings). */
    if (jrt_is_str(v)) return jrt_json_esc_str((const char*)jrt_unbox_ptr(v));
    /* Otherwise it is a heap array (or dict). */
    JrtArrayHdr* arr = (JrtArrayHdr*)jrt_unbox_ptr(v);
    int64_t n = arr->len;
    if (n < 0 || n > (1 << 24)) {
        return jrt_str_dup("null", JRT_TRUSTED);  /* implausible shape */
    }
    int64_t* elems = (int64_t*)arr->data;

    size_t cap = 256 + (size_t)(n > 0 ? n : 1) * 256;
    char* out = malloc(cap); if (!out) return NULL;
    size_t pos = 0;
    uint8_t trust = JRT_TRUSTED;
    out[pos++] = '[';

    for (int64_t i = 0; i < n; i++) {
        if (i > 0) {
            if (pos + 2 >= cap) { cap *= 2; char* nb = realloc(out, cap); if (!nb) { free(out); return NULL; } out = nb; }
            out[pos++] = ',';
        }
        size_t dl = 0;
        char* ds = json_dict_to_str_plain(jrt_unbox_ptr((jade_value_t)elems[i]), &dl, &trust);
        if (!ds) { ds = strdup("{}"); dl = ds ? strlen(ds) : 0; }
        while (pos + dl + 4 >= cap) { cap *= 2; char* nb = realloc(out, cap); if (!nb) { free(out); free(ds); return NULL; } out = nb; }
        if (dl > 0) memcpy(out + pos, ds, dl);
        pos += dl;
        free(ds);
    }
    if (pos + 4 >= cap) { cap += 8; char* nb = realloc(out, cap); if (!nb) { free(out); return NULL; } out = nb; }
    out[pos++] = ']';
    char* tagged = jrt_str_new(pos, trust);
    memcpy(tagged, out, pos);
    free(out);
    return tagged;
}

static char* json_dict_to_str_plain(void* dict_ptr, size_t* out_len, uint8_t* trust_acc) {
    if (!dict_ptr) {
        char* s = strdup("{}");
        if (out_len) *out_len = s ? 2 : 0;
        return s;
    }
    JadeDict* d = (JadeDict*)dict_ptr;
    size_t cap = 256; char* out = malloc(cap); if (!out) return NULL;
    size_t pos = 0;
    out[pos++] = '{';
    int first = 1;
    for (int64_t j = 0; j < d->cap; j++) {
        if (!d->slots[j].key) continue;
        if (!first) {
            if (pos + 2 >= cap) { cap *= 2; char* nb = realloc(out, cap); if (!nb) { free(out); return NULL; } out = nb; }
            out[pos++] = ',';
        }
        first = 0;
        char* ek = json_escape(d->slots[j].key);
        if (!ek) { free(out); return NULL; }
        size_t kl = strlen(ek);
        while (pos + kl + 4 >= cap) { cap *= 2; char* nb = realloc(out, cap); if (!nb) { free(out); free(ek); return NULL; } out = nb; }
        memcpy(out + pos, ek, kl); pos += kl; free(ek);
        out[pos++] = ':';
        /* Values are tagged: a string is escaped as JSON text; scalars render
         * as JSON literals by kind (matching what a value's runtime type is). */
        jade_value_t vv = d->slots[j].val;
        char numbuf[64];
        char* ev;
        if (jrt_is_str(vv)) {
            const char* vstr = (const char*)jrt_unbox_ptr(vv);
            if (trust_acc && vstr) *trust_acc |= jrt_trust_of(vstr);
            ev = json_escape(vstr ? vstr : "");
        } else if (jrt_is_ptr(vv)) {
            /* Non-string heap value — a nested object; recurse. (Nested arrays
             * share the heap-ptr tag and would be rendered as objects; that is
             * an existing limitation of JSON-encoding deeply-nested values.) */
            size_t dl = 0;
            ev = json_dict_to_str_plain(jrt_unbox_ptr(vv), &dl, trust_acc);
            if (!ev) ev = strdup("{}");
        } else if (jrt_is_float(vv)) {
            jrt_snprintf_float(numbuf, sizeof numbuf, jrt_unbox_float(vv));
            ev = strdup(numbuf);
        } else if (jrt_is_bool(vv)) {
            ev = strdup(jrt_unbox_bool(vv) ? "true" : "false");
        } else if (jrt_is_nil(vv)) {
            ev = strdup("null");
        } else { /* int */
            snprintf(numbuf, sizeof numbuf, "%lld", (long long)jrt_unbox_int(vv));
            ev = strdup(numbuf);
        }
        if (!ev) { free(out); return NULL; }
        size_t vl = strlen(ev);
        while (pos + vl + 4 >= cap) { cap *= 2; char* nb = realloc(out, cap); if (!nb) { free(out); free(ev); return NULL; } out = nb; }
        memcpy(out + pos, ev, vl); pos += vl; free(ev);
    }
    if (pos + 4 >= cap) { cap += 8; char* nb = realloc(out, cap); if (!nb) { free(out); return NULL; } out = nb; }
    out[pos++] = '}';
    out[pos] = '\0';
    if (out_len) *out_len = pos;
    return out;
}

/* Minimal flat JSON object parser. Every parsed value inherits the trust
 * level of the input — bytes that came from an LLM stay tainted after
 * structural extraction. */
void* jrt_json_parse(const char* s) {
    if (!s) return jade_dict_create();
    uint8_t t = jrt_trust_of(s);
    void* dict = jade_dict_create();
    while (*s && isspace((unsigned char)*s)) s++;
    if (*s != '{') return dict;
    s++;
    while (*s) {
        while (*s && isspace((unsigned char)*s)) s++;
        if (*s == '}' || *s == '\0') break;
        if (*s != '"') break;
        s++;
        const char* ks = s;
        while (*s && *s != '"') { if (*s == '\\') s++; if (*s) s++; }
        size_t klen = (size_t)(s - ks);
        char* key = malloc(klen + 1); if (!key) break;
        memcpy(key, ks, klen); key[klen] = '\0';
        if (*s == '"') s++;
        while (*s && isspace((unsigned char)*s)) s++;
        if (*s != ':') { free(key); break; }
        s++;
        while (*s && isspace((unsigned char)*s)) s++;
        char* val = NULL;
        int val_is_subdict = 0;  /* nested {…} → a dict pointer, not a string */
        if (*s == '"') {
            s++;
            /* Buffer the unescaped value bytes in a plain heap buffer first,
             * then copy into a tagged string at the end. */
            size_t vcap = 256; char* tmp = malloc(vcap); size_t vp = 0;
            if (tmp) {
                while (*s && *s != '"') {
                    if (*s == '\\') { s++; if (!*s) break; }
                    if (vp + 2 >= vcap) { vcap *= 2; char* nb = realloc(tmp, vcap); if (!nb) { free(tmp); tmp = NULL; break; } tmp = nb; }
                    if (tmp) tmp[vp++] = *s;
                    s++;
                }
            }
            if (tmp) {
                val = jrt_str_new(vp, t);
                if (vp > 0) memcpy(val, tmp, vp);
                free(tmp);
            }
            if (*s == '"') s++;
        } else if (strncmp(s, "true", 4) == 0) {
            val = jrt_str_dup("true", JRT_TRUSTED); s += 4;
        } else if (strncmp(s, "false", 5) == 0) {
            val = jrt_str_dup("false", JRT_TRUSTED); s += 5;
        } else if (strncmp(s, "null", 4) == 0) {
            val = jrt_str_dup("", JRT_TRUSTED); s += 4;
        } else if (*s == '{') {
            /* Nested object: scan to the matching '}' (respecting string
             * literals), then recursively parse it into a sub-dict whose
             * pointer becomes this key's value. The substring is copied into a
             * tagged buffer carrying the parent's trust so the recursive call
             * tags its leaves consistently. */
            const char* obj_start = s;
            int depth = 0, in_str = 0;
            while (*s) {
                char c = *s;
                if (in_str) {
                    if (c == '\\') { s++; if (*s) s++; continue; }
                    if (c == '"') in_str = 0;
                    s++;
                    continue;
                }
                if (c == '"') { in_str = 1; s++; continue; }
                if (c == '{') depth++;
                else if (c == '}') { depth--; s++; if (depth == 0) break; else continue; }
                s++;
            }
            size_t olen = (size_t)(s - obj_start);
            char* sub = jrt_str_new(olen, t);
            if (sub) {
                memcpy(sub, obj_start, olen);
                void* subdict = jrt_json_parse(sub);
                jrt_str_free(sub);
                val = (char*)subdict;
                val_is_subdict = 1;
            }
        } else {
            const char* ns = s;
            while (*s && (isdigit((unsigned char)*s) || *s=='-' || *s=='.' || *s=='e' || *s=='E' || *s=='+')) s++;
            size_t nl = (size_t)(s - ns);
            /* Numbers parsed out of a tainted input are themselves safe (no
             * shell metachars survive), but conservatively propagate the
             * source's trust — a malformed "number" containing `;` would
             * otherwise leak through. */
            val = jrt_str_new(nl, t);
            if (nl > 0) memcpy(val, ns, nl);
        }
        /* Store tagged by kind so a read-back through an Unknown channel renders
         * correctly: a nested object is a dict pointer, everything else (string,
         * number-as-text, true/false/null) is a string. */
        if (val) jade_dict_set(dict, key,
                               val_is_subdict ? jrt_box_ptr(val) : jrt_box_str(val));
        free(key);
        while (*s && isspace((unsigned char)*s)) s++;
        if (*s == ',') s++;
    }
    return dict;
}

/* ── Conversions ────────────────────────────────────────────────── */

/* Parse a string to a bool, matching the VM's `bool()` (vm.rs vm_type_call):
 * the lowercased string "false" or the empty string → false; every other
 * string (including "true") → true. Case-insensitive via tolower to mirror
 * the VM's `s.to_lowercase()`, without depending on <strings.h>. */
int32_t jrt_bool_of_str(const char* s) {
    if (!s || s[0] == '\0') return 0;
    const char* f = "false";
    size_t i = 0;
    for (; f[i] && s[i]; i++) {
        if ((char)tolower((unsigned char)s[i]) != f[i]) return 1;
    }
    return (f[i] == '\0' && s[i] == '\0') ? 0 : 1;
}

/* Refuse tainted strings at code-execution / IO sinks. Shared by the fs/ and
 * sh/ runtime modules (declared in runtime.h); lives in the always-linked core. */
void jrt_refuse_if_tainted(const char* arg, const char* sink_name) {
    if (jrt_trust_of(arg) != JRT_TRUSTED) {
        fprintf(stderr,
            "jade: refused tainted string in %s — value derived from an "
            "untrusted source (LLM, network, file, stdin) and cannot flow "
            "to a code-execution sink\n",
            sink_name);
        jade_rt_exit(1);
    }
}

/* ── Stdlib leaf modules ──────────────────────────────────────────────
 * fs/ sh/ env/ path/ http/ time/ random/ each live in their own folder
 * (built by build.rs). They use only the public ABI declared in runtime.h —
 * jrt_str_*, jade_dict_*, jrt_array_*, jrt_box_*, jrt_refuse_if_tainted, the
 * jade_rt_* backend hooks. The data-structure operations (dict/array/json) that
 * need the internal heap structs above stay here in common.c. */

/* ── Input ──────────────────────────────────────────────────────────── */

char* jrt_readline(const char* prompt) {
    if (prompt && *prompt) { printf("%s", prompt); fflush(stdout); }
    /* An interactive terminal is the local operator, whose intent we trust
     * (TRUSTED — it can seed a trusted prompt). A piped/redirected stdin is an
     * external channel (a file, the network, another process) and stays
     * TAINTED — the boundary the threat model is about. */
    uint8_t trust = isatty(STDIN_FILENO) ? JRT_TRUSTED : JRT_TAINTED;
    char buf[4096];
    if (!fgets(buf, sizeof(buf), stdin)) return jrt_str_dup("", trust);
    size_t n = strlen(buf);
    while (n > 0 && (buf[n-1] == '\n' || buf[n-1] == '\r')) n--;
    char* out = jrt_str_new(n, trust);
    if (n > 0) memcpy(out, buf, n);
    return out;
}

