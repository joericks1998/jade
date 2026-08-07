/* A stand-in for a handle-based C library, for the backend parity gate.
 *
 * Handles are the one value kind no `.jde` fixture can produce: a handle only
 * ever comes from a native package, and a Jade package built with
 * `jade build --lib` has no way to mint one. So `examples/` cannot cover this
 * tag the way it covers every other, and without something like this file the
 * handle marshallers would be exactly as untested as the bytes marshaller was
 * for the three releases it was broken.
 *
 * It is deliberately not a real library. It hands out a pointer to a struct
 * Jade knows nothing about, takes it back on later calls, and reads through it —
 * which is the whole contract, and enough to catch the two ways it can break:
 * a pointer that does not survive the trip, and a type name that does not
 * travel with it.
 *
 * Built by backend-parity.sh with `cc`, which the C shim path already requires.
 * It lives here rather than under `examples/` because the built library has a
 * per-platform extension that a committed `jade.toml` would have to hard-code.
 */
#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>

#define JADE_FFI_NIL    0
#define JADE_FFI_INT    1
#define JADE_FFI_STR    4
#define JADE_FFI_ERROR  5
#define JADE_FFI_HANDLE 10
#define JADE_FFI_FN     11

typedef struct JadeHandle JadeHandle;
typedef struct JadeFn JadeFn;

typedef union {
    int64_t     as_int;
    double      as_float;
    uint8_t     as_bool;
    const char* as_str;
    uint64_t    as_nil;
    JadeHandle* as_handle;
    JadeFn*     as_fn;
} JadeValData;

typedef struct { uint8_t tag; uint8_t _pad[7]; JadeValData data; } JadeVal;
struct JadeHandle { void* ptr; const char* type_name; };
struct JadeFn {
    void* host;
    int (*invoke)(void* host, size_t argc, const JadeVal* argv, JadeVal* out);
};

typedef int (*JadeNativeFnPtr)(size_t argc, const JadeVal* argv, JadeVal* out);
typedef struct { const char* name; JadeNativeFnPtr func; } JadeBinding;
typedef struct { const char* name; const JadeBinding* bindings; size_t binding_count; } JadeNativePkg;

/* The magic word is how the fixture tells "the pointer came back intact" from
 * the weaker "the pointer came back non-null". */
#define THING_MAGIC 0x5AFEC0DEu
typedef struct { unsigned magic; char name[32]; int closed; } Thing;

/* Live objects, from the library's point of view. Jade dropping a handle must
 * not change this — that is the ownership rule, and this counter is how a Jade
 * program observes it. */
static int g_live = 0;

static JadeHandle* wrap(void* p, const char* type_name) {
    JadeHandle* h = (JadeHandle*)malloc(sizeof(JadeHandle));
    if (!h) abort();
    h->ptr = p;
    h->type_name = strdup(type_name);
    return h;
}

static int fx_open(size_t argc, const JadeVal* argv, JadeVal* out) {
    if (argc != 1 || argv[0].tag != JADE_FFI_STR) return 1;
    Thing* t = (Thing*)calloc(1, sizeof(Thing));
    if (!t) abort();
    t->magic = THING_MAGIC;
    snprintf(t->name, sizeof t->name, "%s", argv[0].data.as_str);
    g_live++;
    out->tag = JADE_FFI_HANDLE;
    out->data.as_handle = wrap(t, "Thing");
    return 0;
}

/* Reads through the handle, so a pointer that did not survive shows up as an
 * error rather than as a wrong-looking string. */
static int fx_name(size_t argc, const JadeVal* argv, JadeVal* out) {
    if (argc != 1 || argv[0].tag != JADE_FFI_HANDLE) return 1;
    const JadeHandle* h = argv[0].data.as_handle;
    if (!h || !h->ptr) return 1;
    if (!h->type_name || strcmp(h->type_name, "Thing") != 0) {
        out->tag = JADE_FFI_ERROR;
        out->data.as_str = "the handle's type name did not survive";
        return 1;
    }
    Thing* t = (Thing*)h->ptr;
    if (t->magic != THING_MAGIC) {
        out->tag = JADE_FFI_ERROR;
        out->data.as_str = "the handle's pointer did not survive";
        return 1;
    }
    out->tag = JADE_FFI_STR;
    out->data.as_str = t->name;
    return 0;
}

/* Returns a pointer *into* its argument. Innocuous-looking, and it is what
 * caught the AOT reading freed memory: jrt_native_call used to release the
 * argument trees before converting the result, so this returned an empty string
 * compiled and the right one interpreted. */
static int fx_tag_of(size_t argc, const JadeVal* argv, JadeVal* out) {
    if (argc != 1 || argv[0].tag != JADE_FFI_HANDLE) return 1;
    out->tag = JADE_FFI_STR;
    out->data.as_str = argv[0].data.as_handle->type_name;
    return 0;
}

/* A second handle type at the same address, so the fixture covers the case the
 * type name exists for: two handles that are identical as pointers and must not
 * be interchangeable as values. */
static int fx_cursor(size_t argc, const JadeVal* argv, JadeVal* out) {
    if (argc != 1 || argv[0].tag != JADE_FFI_HANDLE) return 1;
    out->tag = JADE_FFI_HANDLE;
    out->data.as_handle = wrap(argv[0].data.as_handle->ptr, "Cursor");
    return 0;
}

static int fx_close(size_t argc, const JadeVal* argv, JadeVal* out) {
    if (argc != 1 || argv[0].tag != JADE_FFI_HANDLE) return 1;
    Thing* t = (Thing*)argv[0].data.as_handle->ptr;
    if (t->closed) {
        out->tag = JADE_FFI_ERROR;
        out->data.as_str = "already closed";
        return 1;
    }
    t->closed = 1;
    t->magic = 0;
    /* Not freed, so a double close stays well-defined and its error path is
     * testable. A real library would free here. */
    g_live--;
    out->tag = JADE_FFI_NIL;
    out->data.as_nil = 0;
    return 0;
}

static int fx_live(size_t argc, const JadeVal* argv, JadeVal* out) {
    (void)argc; (void)argv;
    out->tag = JADE_FFI_INT;
    out->data.as_int = g_live;
    return 0;
}

/* Calls a Jade function back, once per entry. The two engines re-enter in
 * completely different ways — compiled code calls a lowered function directly,
 * while the VM cannot be re-entered from a C frame at all and has to post the
 * call to the interpreter and wait — so this is exactly the kind of thing that
 * agrees in unit tests and diverges in practice. */
static int fx_visit(size_t argc, const JadeVal* argv, JadeVal* out) {
    if (argc != 2 || argv[0].tag != JADE_FFI_HANDLE || argv[1].tag != JADE_FFI_FN) return 1;
    const JadeHandle* h = argv[0].data.as_handle;
    const JadeFn* cb = argv[1].data.as_fn;
    Thing* t = (Thing*)h->ptr;
    int64_t total = 0;
    for (int i = 0; i < 3; i++) {
        JadeVal a[2];
        a[0].tag = JADE_FFI_INT; a[0].data.as_int = i;
        a[1].tag = JADE_FFI_STR; a[1].data.as_str = t->name;
        JadeVal r; r.tag = JADE_FFI_NIL; r.data.as_nil = 0;
        if (cb->invoke(cb->host, 2, a, &r) != 0) {
            /* The callback raised. Stop cleanly and report — never let it
             * unwind through these frames. */
            out->tag = JADE_FFI_ERROR;
            out->data.as_str = "callback failed";
            return 1;
        }
        if (r.tag == JADE_FFI_INT) total += r.data.as_int;
    }
    out->tag = JADE_FFI_INT;
    out->data.as_int = total;
    return 0;
}

static const JadeBinding BINDINGS[] = {
    { "visit",  fx_visit  },
    { "open",   fx_open   },
    { "name",   fx_name   },
    { "tag_of", fx_tag_of },
    { "cursor", fx_cursor },
    { "close",  fx_close  },
    { "live",   fx_live   },
};

/* Must match jade_runtime::RUNTIME_ABI_VERSION, or the loader refuses this
 * package — which is the check working, but it makes the gate fail with a
 * version message rather than a parity one. Bump this with that constant. */
uint32_t jade_pkg_abi_version(void) { return 4; }

int jade_pkg_init(JadeNativePkg* out) {
    out->name = "handlefix";
    out->bindings = BINDINGS;
    out->binding_count = sizeof(BINDINGS) / sizeof(BINDINGS[0]);
    return 0;
}
