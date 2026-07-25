/* Native (C-ABI) package support for AOT binaries — the counterpart of the VM's
 * libloading FFI (jadelang/src/native.rs). Platform-agnostic: the actual dlopen/
 * dlsym primitives are backend hooks (posix.c provides the real ones; a target
 * without an in-binary loader stubs them). Everything
 * here — the registry, jade_pkg_init invocation, and value marshalling — mirrors
 * native.rs's load_native_package / vm_to_ffi / ffi_to_vm so a single .dylib
 * serves both `jade run` and `jade build`. */
#include "runtime.h"

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef int (*JadePkgInitFn)(JadeNativePkg* out);

/* A loaded package: an owned copy of the (name -> fnptr) bindings. We strdup the
 * names because the source bindings may live in the library's read-only data and
 * we want stable lookup keys. Lookups are a linear scan — native calls are not a
 * hot path and packages export only a handful of functions. */
typedef struct {
    char**          names;
    JadeNativeFnPtr* funcs;
    size_t          count;
} JadePkgHandle;

/* Raise a catchable Jade error with a formatted message (TRUSTED string), via
 * the runtime's typed-throw mechanism. Does not return. */
static void native_raise(const char* fmt, const char* arg) {
    char msg[512];
    snprintf(msg, sizeof msg, fmt, arg ? arg : "");
    jade_exc_throw_typed(jrt_box_str(jrt_str_dup(msg, JRT_TRUSTED)), NULL);
}

void* jrt_native_load(const char* path) {
    void* lib = jade_dlopen(path);
    if (!lib) {
        native_raise("could not load native library '%s'", path);
    }

    JadePkgInitFn init = (JadePkgInitFn)jade_dlsym(lib, "jade_pkg_init");
    if (!init) {
        native_raise("native library '%s' missing `jade_pkg_init` symbol", path);
    }

    JadeNativePkg pkg = {0};
    int status = init(&pkg);
    if (status != 0) {
        native_raise("jade_pkg_init in '%s' returned an error", path);
    }

    JadePkgHandle* h = (JadePkgHandle*)malloc(sizeof(JadePkgHandle));
    if (!h) jade_rt_fatal("jade: out of memory");
    h->count = (pkg.bindings && pkg.binding_count) ? pkg.binding_count : 0;
    h->names = NULL;
    h->funcs = NULL;
    if (h->count) {
        h->names = (char**)malloc(h->count * sizeof(char*));
        h->funcs = (JadeNativeFnPtr*)malloc(h->count * sizeof(JadeNativeFnPtr));
        if (!h->names || !h->funcs) jade_rt_fatal("jade: out of memory");
        size_t n = 0;
        for (size_t i = 0; i < pkg.binding_count; i++) {
            const JadeBinding* b = &pkg.bindings[i];
            if (!b->name) continue;
            char* dup = strdup(b->name);
            if (!dup) jade_rt_fatal("jade: out of memory");
            h->names[n] = dup;
            h->funcs[n] = b->func;
            n++;
        }
        h->count = n;
    }
    return h;
}

static JadeNativeFnPtr native_lookup(JadePkgHandle* h, const char* name) {
    for (size_t i = 0; i < h->count; i++) {
        if (strcmp(h->names[i], name) == 0) return h->funcs[i];
    }
    return NULL;
}

/* Whether a loaded package exports `name` — lets a caller probe for an optional
 * binding (e.g. a provider's `configure`) without a call that would raise. */
int jrt_native_has(void* handle, const char* name) {
    return handle && native_lookup((JadePkgHandle*)handle, name) != NULL ? 1 : 0;
}

/* libc strdup for a container-owned string (process-shared allocator, so the
 * peer runtime can free it). */
static const char* ffi_strdup(const char* s) {
    if (!s) s = "";
    size_t n = strlen(s) + 1;
    char* p = (char*)malloc(n);
    if (!p) jade_rt_fatal("jade: out of memory");
    memcpy(p, s, n);
    return p;
}

/* Marshal a tagged jade_value_t into a JadeVal for the native ABI. Matches
 * native.rs::vm_to_ffi: primitives convert directly; arrays/dicts are deep-copied
 * into libc-owned JadeArr/JadeMap trees; structs and other heap kinds become nil.
 * `owned_str` is set for container elements (their strings are copied so the tree
 * owns them) and clear at the top level (strings are handed over non-owning — the
 * native fn copies if it needs to retain). */
static JadeVal to_ffi_val(jade_value_t v, int owned_str) {
    JadeVal out = {0};
    if (jrt_is_int(v)) {
        out.tag = JADE_FFI_INT;
        out.data.as_int = jrt_unbox_int(v);
    } else if (jrt_is_float(v)) {
        out.tag = JADE_FFI_FLOAT;
        out.data.as_float = jrt_unbox_float(v);
    } else if (jrt_is_bool(v)) {
        out.tag = JADE_FFI_BOOL;
        out.data.as_bool = (uint8_t)jrt_unbox_bool(v);
    } else if (jrt_is_str(v)) {
        const char* s = (const char*)jrt_unbox_ptr(v);
        out.tag = JADE_FFI_STR;
        out.data.as_str = owned_str ? ffi_strdup(s) : s;
    } else if (jrt_is_ptr(v)) {
        void* p = jrt_unbox_ptr(v);
        int64_t kind = jrt_kind_of(p);
        if (kind == JK_ARRAY) {
            int64_t n = jrt_coll_array_len(p);
            JadeArr* a = (JadeArr*)malloc(sizeof(JadeArr));
            if (!a) jade_rt_fatal("jade: out of memory");
            a->len = (size_t)n;
            a->items = n ? (JadeVal*)malloc((size_t)n * sizeof(JadeVal)) : NULL;
            if (n && !a->items) jade_rt_fatal("jade: out of memory");
            for (int64_t i = 0; i < n; i++)
                a->items[i] = to_ffi_val(jrt_coll_array_get(p, i), 1);
            out.tag = JADE_FFI_ARRAY;
            out.data.as_arr = a;
        } else if (kind == JK_DICT) {
            /* No dict iterator is exported, so read entries through the sorted
             * key array. It is a fresh runtime allocation left to the arena — a
             * native-bearing program is not reference-counted, so its collections
             * live until process exit either way. */
            void* keys = jrt_coll_dict_keys(p);
            int64_t n = jrt_coll_array_len(keys);
            JadeMap* m = (JadeMap*)malloc(sizeof(JadeMap));
            if (!m) jade_rt_fatal("jade: out of memory");
            m->len  = (size_t)n;
            m->keys = n ? (const char**)malloc((size_t)n * sizeof(char*)) : NULL;
            m->vals = n ? (JadeVal*)malloc((size_t)n * sizeof(JadeVal)) : NULL;
            if (n && (!m->keys || !m->vals)) jade_rt_fatal("jade: out of memory");
            for (int64_t i = 0; i < n; i++) {
                const char* ks = (const char*)jrt_unbox_ptr(jrt_coll_array_get(keys, i));
                int64_t val = (int64_t)JRT_NIL;
                jrt_coll_dict_get(p, ks, &val);
                m->keys[i] = ffi_strdup(ks);
                m->vals[i] = to_ffi_val((jade_value_t)val, 1);
            }
            out.tag = JADE_FFI_DICT;
            out.data.as_dict = m;
        } else {
            out.tag = JADE_FFI_NIL;  /* struct / other heap kind */
        }
    } else {
        out.tag = JADE_FFI_NIL;
        out.data.as_nil = 0;
    }
    return out;
}

static JadeVal to_ffi(jade_value_t v) { return to_ffi_val(v, 0); }

/* Marshal a JadeVal returned by a native fn back to a tagged jade_value_t.
 * Matches native.rs::ffi_to_vm: output strings are copied into TAINTED tagged
 * strings (native output is external input); arrays/dicts are rebuilt as
 * kind-tagged collections. JADE_FFI_ERROR raises. */
static jade_value_t from_ffi(const JadeVal* v) {
    switch (v->tag) {
        case JADE_FFI_NIL:   return JRT_NIL;
        case JADE_FFI_INT:   return jrt_box_int(v->data.as_int);
        case JADE_FFI_FLOAT: return jrt_box_float(v->data.as_float);
        case JADE_FFI_BOOL:  return jrt_box_bool(v->data.as_bool);
        case JADE_FFI_STR:
            return jrt_box_str(jrt_str_dup(v->data.as_str ? v->data.as_str : "",
                                           JRT_TAINTED));
        case JADE_FFI_ARRAY: {
            const JadeArr* a = v->data.as_arr;
            void* arr = jrt_karr_new();
            if (a)
                for (size_t i = 0; i < a->len; i++)
                    jrt_karr_push(arr, from_ffi(&a->items[i]));
            return jrt_box_ptr(arr);
        }
        case JADE_FFI_DICT: {
            const JadeMap* m = v->data.as_dict;
            void* d = jrt_kdict_new();
            if (m)
                for (size_t i = 0; i < m->len; i++) {
                    jade_value_t kw = jrt_box_str(
                        jrt_str_dup(m->keys[i] ? m->keys[i] : "", JRT_TAINTED));
                    jrt_kdict_set(d, kw, from_ffi(&m->vals[i]));
                }
            return jrt_box_ptr(d);
        }
        case JADE_FFI_ERROR:
            native_raise("%s", v->data.as_str ? v->data.as_str : "native error");
            return JRT_NIL;  /* unreachable */
        default:
            native_raise("native function returned an unknown value tag", NULL);
            return JRT_NIL;  /* unreachable */
    }
}

/* Recursively free a container-owned JadeVal node (its owned string, or its
 * nested arrays/dicts). Reached only through a container root; a container's
 * string elements are libc-owned (ffi_strdup), so they are freed here. */
static void ffi_free_node(JadeVal* v) {
    switch (v->tag) {
        case JADE_FFI_STR:
        case JADE_FFI_ERROR:
            free((void*)v->data.as_str);
            break;
        case JADE_FFI_ARRAY: {
            JadeArr* a = v->data.as_arr;
            if (a) {
                for (size_t i = 0; i < a->len; i++) ffi_free_node(&a->items[i]);
                free(a->items);
                free(a);
            }
            break;
        }
        case JADE_FFI_DICT: {
            JadeMap* m = v->data.as_dict;
            if (m) {
                for (size_t i = 0; i < m->len; i++) {
                    free((void*)m->keys[i]);
                    ffi_free_node(&m->vals[i]);
                }
                free((void*)m->keys);
                free(m->vals);
                free(m);
            }
            break;
        }
        default:
            break;  /* scalar: nothing owned */
    }
}

void jade_ffi_free(JadeVal* v) {
    /* Only container roots own heap; top-level scalars (incl. non-owning strings)
     * are left untouched, so this is safe to call on any JadeVal. */
    if (v->tag == JADE_FFI_ARRAY || v->tag == JADE_FFI_DICT) ffi_free_node(v);
}

jade_value_t jrt_native_call(void* handle, const char* fn_name,
                             const jade_value_t* args, int64_t argc) {
    JadePkgHandle* h = (JadePkgHandle*)handle;
    JadeNativeFnPtr fn = native_lookup(h, fn_name);
    if (!fn) {
        native_raise("native function '%s' not found in package", fn_name);
    }

    /* Marshal args. argc is small (function arity); a fixed inline buffer covers
     * the common case, with a heap fallback for the rare wide call. */
    JadeVal inline_buf[8];
    JadeVal* argv = inline_buf;
    if (argc > (int64_t)(sizeof inline_buf / sizeof inline_buf[0])) {
        argv = (JadeVal*)malloc((size_t)argc * sizeof(JadeVal));
        if (!argv) jade_rt_fatal("jade: out of memory");
    }
    for (int64_t i = 0; i < argc; i++) argv[i] = to_ffi(args[i]);

    JadeVal out = {0};
    out.tag = JADE_FFI_NIL;
    int status = fn((size_t)argc, argv, &out);

    /* The call has returned; release the marshalled argument trees. */
    for (int64_t i = 0; i < argc; i++) jade_ffi_free(&argv[i]);
    if (argv != inline_buf) free(argv);

    if (status != 0) {
        if (out.tag == JADE_FFI_STR || out.tag == JADE_FFI_ERROR) {
            native_raise("%s", out.data.as_str ? out.data.as_str : "native error");
        }
        native_raise("native function '%s' returned a non-zero status", fn_name);
    }

    jade_value_t result = from_ffi(&out);
    jade_ffi_free(&out);  /* release a container return tree (no-op for scalars) */
    return result;
}

/* ── Exported marshalling, for Jade-authored packages (`jade build --lib`) ──
 *
 * `jrt_native_call` above marshals in the *consumer* direction: a Jade program
 * calling into someone else's C library. A Jade package compiled to a shared
 * library needs the mirror image — its generated `jade_pkg_init` wrappers take
 * `JadeVal` arguments from a host and hand them to lowered `jf_<uid>` functions,
 * which speak tagged `jade_value_t`.
 *
 * `to_ffi`/`from_ffi` are static, and codegen cannot call a static, so these
 * thin externs expose them. Same conversions, same limits: non-primitives
 * become nil in both directions. */
jade_value_t jrt_ffi_to_tagged(const JadeVal* v) {
    return from_ffi(v);
}

void jrt_ffi_from_tagged(jade_value_t v, JadeVal* out) {
    *out = to_ffi(v);
}
