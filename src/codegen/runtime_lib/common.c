/* Platform-agnostic core of the Jade runtime. Shared verbatim by the host
 * backend (posix.c) and any alternate platform backend, which supply only the
 * concurrency layer (jade_spawn/await/join) and the process-exit primitive
 * jade_rt_exit. */
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
 * (jade_rt_exit is supplied by the platform backend). */
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

/* jade_join moved to the platform backend alongside jade_spawn/jade_await: the
 * scheduler now lives in the Rust runtime (jade-runtime, src/task.rs) and joins
 * there in one call, so there is no longer a platform-agnostic loop to share. */

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
 * The object storage — arrays, dicts, structs — now lives ONCE in the shared
 * Rust runtime crate (jade-runtime, src/coll.rs) behind a common ObjHeader, and
 * the builders / accessors / kind byte / type-name / dict value-copy / recursive
 * renderer are exported from there (src/ffi_coll.rs) under their historical jrt_*
 * names. Their former C definitions (the JKArray/JKDict/JKStruct structs,
 * jrt_kind_of, jrt_karr_*, jrt_kdict_*, jrt_kstruct_*, jk_dict_copy,
 * jrt_get_type_name, jk_render/jrt_render_any) are deleted; the declarations
 * remain in runtime.h and calls resolve against the staticlib.
 *
 * What stays here are thin forwarders for the operations that can RAISE a
 * catchable Jade error: an exception is a longjmp, which must not cross a Rust
 * frame, so the bounds/type checks and throw_msg stay on the C side. They call
 * the jrt_coll_* storage helpers (in Rust) for the raw reads/writes and
 * jrt_kind_of for the runtime kind (JK_ARRAY/JK_DICT/JK_STRUCT now equal the
 * ObjKind discriminants — see runtime.h). */

int64_t jrt_get_field(int64_t obj, const char* field) {
    jade_value_t v = (jade_value_t)obj;
    /* A caught runtime error is a bare (message) string in the AOT, whereas the
     * VM models it as a RuntimeError struct with a `.message` field. So
     * `error.message` on a string yields the string itself — matching the VM. */
    if (jrt_is_str(v) && strcmp(field, "message") == 0) return obj;
    if (jrt_is_ptr(v)) {
        void* p = jrt_unbox_ptr(v);
        if (jrt_kind_of(p) == JK_STRUCT) {
            int64_t out;
            if (jrt_coll_struct_get(p, field, &out)) return out;
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
        if (jrt_kind_of(p) == JK_STRUCT) { jrt_kstruct_set(p, field, val); return; }
    }
    throw_msg("value does not support field assignment");
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
        int64_t kind = jrt_kind_of(p);
        if (kind == JK_ARRAY) {
            if (!jrt_is_int((jade_value_t)idx)) throw_msg("array index must be int");
            int64_t i = jrt_unbox_int((jade_value_t)idx);
            if (i < 0 || i >= jrt_coll_array_len(p)) throw_msg("index out of bounds");
            return jrt_coll_array_get(p, i);
        }
        if (kind == JK_DICT) {
            if (!jrt_is_str((jade_value_t)idx)) throw_msg("dict index must be str");
            const char* k = (const char*)jrt_unbox_ptr((jade_value_t)idx);
            int64_t out;
            if (jrt_coll_dict_get(p, k, &out)) return out;
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
        int64_t kind = jrt_kind_of(p);
        if (kind == JK_ARRAY) {
            /* Arrays are reference-semantic (VM Arc): mutate in place. */
            if (!jrt_is_int((jade_value_t)idx)) throw_msg("array index must be int");
            int64_t i = jrt_unbox_int((jade_value_t)idx);
            if (i < 0 || i >= jrt_coll_array_len(p)) throw_msg("index out of bounds");
            jrt_coll_array_set(p, i, val);
            return obj;
        }
        if (kind == JK_DICT) {
            /* Dicts are value-semantic (VM clones on mutation): copy then set,
             * and return the new container so the caller rebinds the variable. */
            if (!jrt_is_str((jade_value_t)idx)) throw_msg("dict index must be str");
            void* d = jrt_coll_dict_copy(p);
            jrt_kdict_set(d, idx, val);
            return (int64_t)jrt_box_ptr(d);
        }
    }
    throw_msg("value does not support index assignment");
    return obj;
}

/* jk_render / jrt_render_any moved to the shared Rust runtime crate
 * (jade-runtime, src/render.rs + src/ffi_coll.rs jrt_render_any): a recursive,
 * kind-aware renderer that formats scalars/strings with the SAME primitives the
 * VM's value_to_display uses, so AOT and VM output are byte-identical. It returns
 * a plain system-malloc'd buffer this file's callers still free() as before. */

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
        int64_t kind = jrt_kind_of(p);
        if (kind == JK_ARRAY) {
            int64_t n = jrt_coll_array_len(p);
            for (int64_t i = 0; i < n; i++) {
                if (jrt_eq_any((uint64_t)jrt_coll_array_get(p, i), (uint64_t)needle)) return 1;
            }
            return 0;
        }
        if (kind == JK_DICT) {
            if (!jrt_is_str((jade_value_t)needle)) throw_msg("'in' dict key must be str");
            const char* k = (const char*)jrt_unbox_ptr((jade_value_t)needle);
            int64_t out;
            return jrt_coll_dict_get(p, k, &out) ? 1 : 0;
        }
    }
    throw_msg("'in' requires array, dict, or str");
    return 0;
}

/* ── Chunk-backend collection-producing stdlib forwarders ──────────────────
 * These bridge the C RNG / the raising boundary to the Rust ObjHeader helpers
 * (a Jade exception is a longjmp and must not cross a Rust frame). */

/* fs.list_dir: the Rust helper builds the array or flags an I/O error; raise it
 * here as a catchable exception (message is generic — difftest only checks that
 * it is caught). Returns the array as a tagged pointer word. */
int64_t jrt_fs_list_dir_chunk(const char* path) {
    int32_t err = 0;
    void* arr = jrt_coll_fs_list_dir(path, &err);
    if (err) throw_msg("list_dir: could not read directory");
    return (int64_t)jrt_box_ptr(arr);
}

/* random.choice(arr) -> a random element (already a tagged word); nil on empty.
 * Uses the C RNG (jrt_random_int) so it shares the VM-seeded sequence, and the
 * Rust ObjHeader accessors for element access. */
int64_t jrt_random_choice_chunk(int64_t arr_word) {
    void* arr = jrt_unbox_ptr((jade_value_t)arr_word);
    int64_t n = jrt_coll_array_len(arr);
    if (n == 0) return JRT_NIL;
    int64_t idx = jrt_random_int(0, n - 1);
    return jrt_coll_array_get(arr, idx);
}

/* http.* forwarders: the Rust impls (jade-runtime src/httpf.rs) record a pending
 * error on transport failure (curl exit != 0) instead of throwing; throw it here.
 * A 4xx/5xx is a normal status, not an error. */
static void http_throw_pending(void) {
    char* e = jrt_http_take_error();
    if (e) jade_exc_throw_typed(jrt_box_str(e), NULL);
}
jade_value_t jrt_http_get(const char* url, void* headers) {
    jade_value_t r = (jade_value_t)jrt_http_get_impl(url, headers);
    http_throw_pending();
    return r;
}
jade_value_t jrt_http_post(const char* url, const char* body, void* headers) {
    jade_value_t r = (jade_value_t)jrt_http_post_impl(url, body, headers);
    http_throw_pending();
    return r;
}
jade_value_t jrt_http_put(const char* url, const char* body, void* headers) {
    jade_value_t r = (jade_value_t)jrt_http_put_impl(url, body, headers);
    http_throw_pending();
    return r;
}
jade_value_t jrt_http_delete(const char* url, void* headers) {
    jade_value_t r = (jade_value_t)jrt_http_delete_impl(url, headers);
    http_throw_pending();
    return r;
}
jade_value_t jrt_http_head(const char* url, void* headers) {
    jade_value_t r = (jade_value_t)jrt_http_head_impl(url, headers);
    http_throw_pending();
    return r;
}

/* sh.exec/run forwarders: refuse a tainted command (a code-execution sink), call
 * the Rust impl (jade-runtime src/shf.rs), then throw any pending error it
 * recorded — sh.exec raises on a non-zero exit, run/exec on a spawn failure. */
char* jrt_sh_exec(const char* cmd) {
    jrt_refuse_if_tainted(cmd, "sh.exec(cmd)");
    char* r = jrt_sh_exec_impl(cmd);
    char* e = jrt_sh_take_error();
    if (e) jade_exc_throw_typed(jrt_box_str(e), NULL);
    return r;
}
int64_t jrt_sh_run(const char* cmd) {
    jrt_refuse_if_tainted(cmd, "sh.run(cmd)");
    int64_t r = jrt_sh_run_impl(cmd);
    char* e = jrt_sh_take_error();
    if (e) jade_exc_throw_typed(jrt_box_str(e), NULL);
    return r;
}

/* fs.* raising forwarders: the Rust impls (jade-runtime src/fsf.rs) record a
 * pending error instead of throwing (a longjmp must not cross a Rust frame);
 * throw it here as a catchable exception. fs.read additionally refuses a tainted
 * path first (it is a read sink). */
static void fs_throw_pending(void) {
    char* e = jrt_fs_take_error();
    if (e) jade_exc_throw_typed(jrt_box_str(e), NULL);
}
char* jrt_fs_read(const char* path, int32_t trust) {
    if (!trust) jrt_refuse_if_tainted(path, "fs.read(path)");
    char* r = jrt_fs_read_impl(path, trust);
    fs_throw_pending();
    return r;
}
void jrt_fs_write(const char* path, const char* content)  { jrt_fs_write_impl(path, content);  fs_throw_pending(); }
void jrt_fs_append(const char* path, const char* content) { jrt_fs_append_impl(path, content); fs_throw_pending(); }
void jrt_fs_delete(const char* path)                      { jrt_fs_delete_impl(path);          fs_throw_pending(); }
void jrt_fs_mkdir(const char* path)                       { jrt_fs_mkdir_impl(path);           fs_throw_pending(); }

/* random.int(lo, hi) — the raising boundary for the Rust RNG core (random.c was
 * ported to jade-runtime src/randomf.rs). A Jade exception is a longjmp and must
 * not cross a Rust frame, so the min>max check + throw live here; the draw is
 * jrt_random_draw (Rust). Also the RNG entry point for jrt_random_choice_chunk. */
int64_t jrt_random_int(int64_t lo, int64_t hi) {
    if (lo > hi) {
        char m[96];
        snprintf(m, sizeof m, "random.int: min (%lld) > max (%lld)",
                 (long long)lo, (long long)hi);
        jade_exc_throw_typed(jrt_box_str(jrt_str_dup(m, JRT_TRUSTED)), NULL);
    }
    return jrt_random_draw(lo, hi);
}

/* array.map(arr, fn) -> new array of fn(elem). The Chunk backend's function
 * value is a boxed function pointer: the value word (TAG_PTR) points at an
 * 8-byte global holding jf_<uid>'s address, so `*box` is the callable
 * `int64_t(*)(int64_t)` (closures read captured globals via GetGlobal, so they
 * need no environment arg). Elements are tagged words. */
int64_t jrt_coll_array_map(int64_t arr_word, int64_t fn_word) {
    void* arr = jrt_unbox_ptr((jade_value_t)arr_word);
    void** box = (void**)jrt_unbox_ptr((jade_value_t)fn_word);
    int64_t (*fn)(int64_t) = (int64_t (*)(int64_t))(*box);
    void* out = jrt_karr_new();
    int64_t n = jrt_coll_array_len(arr);
    for (int64_t i = 0; i < n; i++)
        jrt_karr_push(out, fn(jrt_coll_array_get(arr, i)));
    return (int64_t)jrt_box_ptr(out);
}

/* array.filter(arr, fn) -> elements where fn(elem) is truthy. */
int64_t jrt_coll_array_filter(int64_t arr_word, int64_t fn_word) {
    void* arr = jrt_unbox_ptr((jade_value_t)arr_word);
    void** box = (void**)jrt_unbox_ptr((jade_value_t)fn_word);
    int64_t (*fn)(int64_t) = (int64_t (*)(int64_t))(*box);
    void* out = jrt_karr_new();
    int64_t n = jrt_coll_array_len(arr);
    for (int64_t i = 0; i < n; i++) {
        int64_t e = jrt_coll_array_get(arr, i);
        if (jrt_unbox_bool((jade_value_t)fn(e))) jrt_karr_push(out, e);
    }
    return (int64_t)jrt_box_ptr(out);
}

/* random.shuffle(arr): Fisher-Yates in place (C RNG). Returns nothing. */
void jrt_random_shuffle_chunk(int64_t arr_word) {
    void* arr = jrt_unbox_ptr((jade_value_t)arr_word);
    int64_t n = jrt_coll_array_len(arr);
    for (int64_t i = n - 1; i > 0; i--) {
        int64_t j = jrt_random_int(0, i);
        int64_t tmp = jrt_coll_array_get(arr, i);
        jrt_coll_array_set(arr, i, jrt_coll_array_get(arr, j));
        jrt_coll_array_set(arr, j, tmp);
    }
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


/* ── String methods ─────────────────────────────────────────────── */

int32_t jrt_str_contains(const char* haystack, const char* needle) {
    if (!haystack || !needle) return 0;
    return strstr(haystack, needle) != NULL ? 1 : 0;
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

