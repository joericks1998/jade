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

    /* Refuse a package built against a value ABI this runtime cannot talk to.
     *
     * Mirrors check_package_abi in src/native/mod.rs: prefer
     * jade_pkg_abi_version, which `jade build --lib` emits into every package;
     * fall back to jrt_abi_version, which packages published before that symbol
     * existed re-export from the runtime they link. Neither present means the
     * library does not link the Jade runtime at all — a plain C library wrapped
     * by `jade pkg add --c-abi`, with no value ABI to disagree about.
     *
     * Without this, a provider built before v1.1.31 — when the inference request
     * became a struct it cannot read — failed with "native function returned an
     * unknown value tag" from inside the call, naming neither the version nor the
     * fix. */
    uint32_t (*abi)(void) = (uint32_t (*)(void))jade_dlsym(lib, "jade_pkg_abi_version");
    if (!abi) abi = (uint32_t (*)(void))jade_dlsym(lib, "jrt_abi_version");
    if (abi) {
        uint32_t theirs = abi();
        uint32_t ours = jrt_abi_version();
        if (theirs != ours) {
            native_raise(
                theirs < ours
                    ? "native package '%s' speaks an older value ABI than this Jade. Rebuild it "
                      "with this toolchain, or reinstall the providers that ship with your Jade "
                      "release."
                    : "native package '%s' speaks a newer value ABI than this Jade. Upgrade with "
                      "`jade upgrade`.",
                path);
        }
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

/* Copy a struct's type name into the libc heap under the name it crosses the ABI
 * with: its source name, minus any trailing "$<digits>" import-mangling suffix.
 *
 * aot/imports.rs renames a module-global `Foo` to `Foo$2` when it flattens
 * imports, so two imported modules can each declare one. That number describes
 * the importing program's module graph, not the type, so it means nothing on the
 * other side of an FFI call: a provider package built with `use ovata::infer`
 * would hand back a frame named `Token$0` and the caller would not recognise its
 * own protocol. `$` is not legal in a Jade identifier, so the suffix is never part
 * of a name someone wrote. abi_type_name in src/native/mod.rs strips the same
 * thing on the Rust side of the boundary. */
static const char* ffi_strdup_abi_type(const char* name) {
    if (!name) name = "";
    size_t n = strlen(name), cut = n, i = n;
    while (i > 0 && name[i - 1] >= '0' && name[i - 1] <= '9') i--;
    /* At least one digit, after a '$', with something before it: stripping a name
     * that is only a suffix would leave an empty type name. */
    if (i < n && i > 1 && name[i - 1] == '$') cut = i - 1;
    char* p = (char*)malloc(cut + 1);
    if (!p) jade_rt_fatal("jade: out of memory");
    memcpy(p, name, cut);
    p[cut] = 0;
    return p;
}

/* Marshal a tagged jade_value_t into a JadeVal for the native ABI. Matches
 * native.rs::vm_to_ffi: primitives convert directly; arrays, dicts, structs and
 * bytes are deep-copied into libc-owned trees; remaining heap kinds (functions,
 * futures, prompts) become nil, having no ABI representation.
 * `owned_str` is set for container elements (their strings are copied so the tree
 * owns them) and clear at the top level (strings are handed over non-owning — the
 * native fn copies if it needs to retain). It does not apply to bytes, which are
 * copied at every level. */
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
        } else if (kind == JK_STRUCT) {
            /* Same shape as the dict arm, plus the type name — which is the whole
             * point of sending a struct rather than a dict: the receiver can tell
             * an InferRequest from something that merely has the right keys.
             * Fields keep declaration order (jrt_coll_struct_keys does not sort). */
            void* keys = jrt_coll_struct_keys(p);
            int64_t n = jrt_coll_array_len(keys);
            JadeStruct* st = (JadeStruct*)malloc(sizeof(JadeStruct));
            if (!st) jade_rt_fatal("jade: out of memory");
            st->len  = (size_t)n;
            st->keys = n ? (const char**)malloc((size_t)n * sizeof(char*)) : NULL;
            st->vals = n ? (JadeVal*)malloc((size_t)n * sizeof(JadeVal)) : NULL;
            if (n && (!st->keys || !st->vals)) jade_rt_fatal("jade: out of memory");
            /* jrt_get_type_name returns a fresh tagged string (NUL-terminated);
             * copy it into the libc heap the rest of the tree lives in, and free
             * the original — it is owned by this call, not by the value. */
            char* tn = jrt_get_type_name(v);
            st->type_name = ffi_strdup_abi_type(tn);
            jrt_str_free(tn);
            for (int64_t i = 0; i < n; i++) {
                const char* ks = (const char*)jrt_unbox_ptr(jrt_coll_array_get(keys, i));
                int64_t val = (int64_t)JRT_NIL;
                jrt_coll_struct_get(p, ks, &val);
                st->keys[i] = ffi_strdup(ks);
                st->vals[i] = to_ffi_val((jade_value_t)val, 1);
            }
            out.tag = JADE_FFI_STRUCT;
            out.data.as_struct = st;
        } else if (kind == JK_BYTES) {
            /* Copied into the libc heap unconditionally, ignoring `owned_str`:
             * the far side may free it, and this process holds two mimalloc
             * instances that must not free each other's allocations. Same rule
             * as JadeArr/JadeMap above, and the same one native.rs applies —
             * vm_to_ffi hands over only *strings* borrowed at the top level, so
             * bytes are owned at every level and ffi_free_node reclaims them.
             * The trust byte does not cross: JadeBytes is data + length, and
             * anything coming back in is TAINTED regardless. */
            const unsigned char* src = jrt_bytes_data(p);
            size_t n = (size_t)jrt_bytes_len(p);
            JadeBytes* bx = (JadeBytes*)malloc(sizeof(JadeBytes));
            if (!bx) jade_rt_fatal("jade: out of memory");
            /* malloc(0) may return NULL legitimately, which the free path
             * cannot tell from failure — ask for a byte instead. */
            bx->data = (unsigned char*)malloc(n ? n : 1);
            if (!bx->data) jade_rt_fatal("jade: out of memory");
            bx->len = n;
            if (n && src) memcpy(bx->data, src, n);
            out.tag = JADE_FFI_BYTES;
            out.data.as_bytes = bx;
        } else if (kind == JK_HANDLE) {
            /* A fresh wrapper each time, so ffi_free_node has something of its
             * own to release, with the name copied like every other
             * container-owned string. The pointer goes straight back to the
             * package that issued it: not copied, and never freed here. */
            JadeHandle* hx = (JadeHandle*)malloc(sizeof(JadeHandle));
            if (!hx) jade_rt_fatal("jade: out of memory");
            hx->ptr = jrt_handle_ptr(p);
            hx->type_name = ffi_strdup(jrt_handle_type(p));
            out.tag = JADE_FFI_HANDLE;
            out.data.as_handle = hx;
        } else {
            out.tag = JADE_FFI_NIL;  /* function / future / other heap kind */
        }
    } else {
        out.tag = JADE_FFI_NIL;
        out.data.as_nil = 0;
    }
    return out;
}

static JadeVal to_ffi(jade_value_t v) { return to_ffi_val(v, 0); }

/* Marshal a JadeVal returned by a native fn back to a tagged jade_value_t.
 * Matches native.rs::ffi_to_vm: output strings and bytes are copied in as
 * TAINTED (native output is external input); arrays, dicts and structs are
 * rebuilt as kind-tagged collections; a handle keeps the pointer and copies its
 * type name. JADE_FFI_ERROR raises. */
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
        case JADE_FFI_STRUCT: {
            const JadeStruct* st = v->data.as_struct;
            if (!st) return JRT_NIL;
            void* obj = jrt_kstruct_new(st->type_name ? st->type_name : "");
            for (size_t i = 0; i < st->len; i++)
                jrt_kstruct_set(obj, st->keys[i] ? st->keys[i] : "", from_ffi(&st->vals[i]));
            return jrt_box_ptr(obj);
        }
        case JADE_FFI_BYTES: {
            const JadeBytes* b = v->data.as_bytes;
            if (!b) return JRT_NIL;
            const unsigned char* d = b->data;
            size_t n = (d && b->len) ? b->len : 0;
            /* TAINTED: data from a native package is from outside the program,
             * exactly as a file read is. Matches native.rs::ffi_to_vm. */
            return jrt_box_ptr(jrt_bytes_new(d, n, JRT_TAINTED));
        }
        case JADE_FFI_HANDLE: {
            const JadeHandle* h = v->data.as_handle;
            if (!h) return JRT_NIL;
            /* No trust byte: a handle carries no data to taint. The pointer is
             * kept as issued; jrt_handle_new copies the name and never reads
             * through the pointer. An unnamed handle gets the empty name, which
             * matches nothing — a binding that forgot its type fails a check
             * instead of passing for anything. */
            return jrt_box_ptr(jrt_handle_new(h->ptr, h->type_name ? h->type_name : ""));
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
        case JADE_FFI_STRUCT: {
            JadeStruct* st = v->data.as_struct;
            if (st) {
                for (size_t i = 0; i < st->len; i++) {
                    free((void*)st->keys[i]);
                    ffi_free_node(&st->vals[i]);
                }
                free((void*)st->type_name);
                free((void*)st->keys);
                free(st->vals);
                free(st);
            }
            break;
        }
        case JADE_FFI_BYTES: {
            JadeBytes* b = v->data.as_bytes;
            if (b) {
                free(b->data);
                free(b);
            }
            break;
        }
        case JADE_FFI_HANDLE: {
            JadeHandle* h = v->data.as_handle;
            if (h) {
                /* The name and the wrapper. NOT h->ptr — that belongs to the
                 * package, and releasing it here would free the library's memory
                 * through the wrong allocator. See JADE_FFI_HANDLE. */
                free((void*)h->type_name);
                free(h);
            }
            break;
        }
        default:
            break;  /* scalar: nothing owned */
    }
}

void jade_ffi_free(JadeVal* v) {
    /* Only owning kinds are reclaimed; top-level scalars (incl. non-owning
     * strings) are left untouched, so this is safe to call on any JadeVal.
     * Bytes and handles belong here even though neither is a container:
     * to_ffi_val builds a fresh wrapper for each at every level, top included,
     * so an unfreed one leaks its header on every call. For a handle that is the
     * wrapper and the type name only — never the pointee. */
    if (v->tag == JADE_FFI_ARRAY || v->tag == JADE_FFI_DICT ||
        v->tag == JADE_FFI_STRUCT || v->tag == JADE_FFI_BYTES ||
        v->tag == JADE_FFI_HANDLE) ffi_free_node(v);
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

    /* Read `out` BEFORE releasing the argument trees, matching native.rs's
     * NativeLibFn::call. A native function may legitimately return a pointer
     * *into* one of its arguments — `tag_of(h)` returning the handle's type
     * name, and anything of that shape — and freeing the args first turns the
     * result into a read of freed memory. It did: the compiled binary printed
     * an empty string where the interpreter printed the name. */
    char errbuf[512];
    int have_err = 0;
    jade_value_t result = JRT_NIL;

    if (status != 0) {
        /* Copy the message out before the free, for the same reason, and
         * because native_raise longjmps — a raise placed before the free would
         * leak every argument tree instead. */
        if ((out.tag == JADE_FFI_STR || out.tag == JADE_FFI_ERROR) && out.data.as_str) {
            snprintf(errbuf, sizeof errbuf, "%s", out.data.as_str);
        } else {
            snprintf(errbuf, sizeof errbuf, "native function '%s' returned a non-zero status",
                     fn_name);
        }
        have_err = 1;
    } else {
        result = from_ffi(&out);
    }

    /* The call has returned and its result has been read; release the
     * marshalled argument trees, then the return tree. */
    for (int64_t i = 0; i < argc; i++) jade_ffi_free(&argv[i]);
    if (argv != inline_buf) free(argv);
    jade_ffi_free(&out);  /* release a container return tree (no-op for scalars) */

    if (have_err) {
        native_raise("%s", errbuf);
    }
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
 * thin externs expose them. Same conversions, same limits: arrays, dicts,
 * structs, strings and bytes all cross; a function, future or prompt has no ABI
 * representation and becomes nil in both directions. */
jade_value_t jrt_ffi_to_tagged(const JadeVal* v) {
    return from_ffi(v);
}

void jrt_ffi_from_tagged(jade_value_t v, JadeVal* out) {
    *out = to_ffi(v);
}
