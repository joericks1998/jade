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

/* Marshal a tagged jade_value_t into a JadeVal for the native ABI. Matches
 * native.rs::vm_to_ffi: primitives convert directly; everything else is nil. For
 * strings we hand over the NUL-terminated data pointer (non-owning — the native
 * fn must copy if it needs to retain). */
static JadeVal to_ffi(jade_value_t v) {
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
        out.tag = JADE_FFI_STR;
        out.data.as_str = (const char*)jrt_unbox_ptr(v);
    } else {
        out.tag = JADE_FFI_NIL;
        out.data.as_nil = 0;
    }
    return out;
}

/* Marshal a JadeVal returned by a native fn back to a tagged jade_value_t.
 * Matches native.rs::ffi_to_vm: output strings are copied into TAINTED tagged
 * strings (native output is external input). JADE_FFI_ERROR raises. */
static jade_value_t from_ffi(const JadeVal* v) {
    switch (v->tag) {
        case JADE_FFI_NIL:   return JRT_NIL;
        case JADE_FFI_INT:   return jrt_box_int(v->data.as_int);
        case JADE_FFI_FLOAT: return jrt_box_float(v->data.as_float);
        case JADE_FFI_BOOL:  return jrt_box_bool(v->data.as_bool);
        case JADE_FFI_STR:
            return jrt_box_str(jrt_str_dup(v->data.as_str ? v->data.as_str : "",
                                           JRT_TAINTED));
        case JADE_FFI_ERROR:
            native_raise("%s", v->data.as_str ? v->data.as_str : "native error");
            return JRT_NIL;  /* unreachable */
        default:
            native_raise("native function returned an unknown value tag", NULL);
            return JRT_NIL;  /* unreachable */
    }
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

    if (argv != inline_buf) free(argv);

    if (status != 0) {
        if (out.tag == JADE_FFI_STR || out.tag == JADE_FFI_ERROR) {
            native_raise("%s", out.data.as_str ? out.data.as_str : "native error");
        }
        native_raise("native function '%s' returned a non-zero status", fn_name);
    }

    return from_ffi(&out);
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
