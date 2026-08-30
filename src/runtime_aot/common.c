/* Platform-agnostic core of the Jade runtime. Shared verbatim by the host
 * backend (posix.c) and any alternate platform backend, which supply only the
 * concurrency layer (jade_spawn/await/join) and the process-exit primitive
 * jade_rt_exit. */
#include "runtime.h"

#include <assert.h>
#include <ctype.h>
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
     * info — render a safe placeholder rather than dereferencing it as text.
     *
     * Bounded by `cap`, so a caller with a small buffer truncates. Every branch
     * below is short except the float one, whose text is unbounded (Rust's `{}`
     * for f64 never uses exponent form, so 1e300 is 301 digits). Callers that
     * must not truncate — jrt_print_any, jrt_str_of_any — send floats straight
     * to jrt_render_any instead of coming through here. */
    jade_value_t v = (jade_value_t)val;
    if (jrt_is_int(v))   return snprintf(buf, cap, "%lld", (long long)jrt_unbox_int(v));
    if (jrt_is_str(v))   return snprintf(buf, cap, "%s", (const char*)jrt_unbox_ptr(v));
    if (jrt_is_float(v)) {
        /* One implementation of float text, in the shared Rust runtime. */
        char* r = jrt_render_any(val);
        int n = snprintf(buf, cap, "%s", r);
        free(r);
        return n;
    }
    if (jrt_is_bool(v))  return snprintf(buf, cap, "%s", jrt_unbox_bool(v) ? "true" : "false");
    if (jrt_is_nil(v))   return snprintf(buf, cap, "nil");
    if (jrt_is_char(v)) {
        /* Re-encode the scalar as UTF-8. Must sit after jrt_is_nil: a char is
         * an immediate sharing the nil branch of the tag space. */
        uint32_t cp = jrt_unbox_char(v);
        char u[5];
        int n = 0;
        if (cp < 0x80) {
            u[n++] = (char)cp;
        } else if (cp < 0x800) {
            u[n++] = (char)(0xC0 | (cp >> 6));
            u[n++] = (char)(0x80 | (cp & 0x3F));
        } else if (cp < 0x10000) {
            u[n++] = (char)(0xE0 | (cp >> 12));
            u[n++] = (char)(0x80 | ((cp >> 6) & 0x3F));
            u[n++] = (char)(0x80 | (cp & 0x3F));
        } else {
            u[n++] = (char)(0xF0 | (cp >> 18));
            u[n++] = (char)(0x80 | ((cp >> 12) & 0x3F));
            u[n++] = (char)(0x80 | ((cp >> 6) & 0x3F));
            u[n++] = (char)(0x80 | (cp & 0x3F));
        }
        u[n] = '\0';
        return snprintf(buf, cap, "%s", u);
    }
    return snprintf(buf, cap, "<object>");  /* non-string heap pointer */
}

void jrt_print_any(int64_t val, const char* suffix) {
    /* Print a type-erased value to stdout, then `suffix`. Used by print() for
     * statically-Unknown args. Strings are written directly (unbounded) — unlike
     * routing through jrt_snprintf_any + a fixed scratch buffer, which truncates
     * a long Unknown string (e.g. a large llm reply). The remaining scalars —
     * int, bool, nil, char — are short and format into a small buffer with no
     * allocation, which is what keeps print() in a loop cheap. */
    jade_value_t v = (jade_value_t)val;
    if (jrt_is_str(v)) {
        fputs((const char*)jrt_unbox_ptr(v), stdout);
    } else if (jrt_is_ptr(v) || jrt_is_float(v)) {
        /* A kind-tagged collection — render `[…]`/`{…}` (recursive, unbounded).
         * A float joins it: its text comes from the same shared Rust renderer
         * the VM uses, and is unbounded too (1e300 is 301 digits), so it must
         * not go through the scratch buffer below. */
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

/* jrt_write_any — the `write(x)` builtin: `print` with no newline, flushed.
 *
 * The flush is the whole point and is not optional. `write` exists for output
 * that has no trailing newline (a progress counter, a prompt the user types
 * after), and stdout to a terminal is line-buffered — without the flush the
 * bytes sit in the buffer until something later emits a newline, so the text
 * appears late or, at exit, out of order. The VM's `write` flushes for the same
 * reason (stdio::write_str_flush); this is the compiled half of that. */
void jrt_write_any(int64_t val) {
    jrt_print_any(val, NULL);
    fflush(stdout);
}

/* ── Dynamic conversion builtins int()/float()/bool() (Chunk backend) ──────
 * Mirror the VM's `coerce_builtin` (vm.rs) EXACTLY on the tagged runtime kind.
 * int()/float() raise a catchable error on a non-numeric string or a
 * non-convertible kind (longjmp stays in C); bool() never raises. */
#include <errno.h>
#include <strings.h>            /* strcasecmp */
static void throw_msg(const char* m);   /* defined below; used by int()/float() */
static void bytes_throw_pending(const char* fallback); /* defined below; used by set-index */

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
    /* A character's Unicode scalar. This is what lets a program read a
     * fixed-size C field: `char mnemonic[32]` arrives as thirty-two characters,
     * NUL padding included, and `int(c) == 0` finds where the text stops.
     * Mirrors the VM's `vm_type_call` "int" arm. */
    if (jrt_is_char(v))  return jrt_box_int((int64_t)jrt_unbox_char(v));
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

/* char(x) — mirrors the VM's `vm_type_call` "char" arm exactly. Accepts a char
 * unchanged, or a string of exactly one character; anything else raises. The
 * one-character rule means the conversion can never silently drop input, and
 * the string's trust byte moves into the char word. */
int64_t jrt_char_any(int64_t val) {
    jade_value_t v = (jade_value_t)val;
    if (jrt_is_char(v)) return val;
    /* The other direction, for building a fixed-size C field from numbers.
     * Refused rather than replaced when the number is not a character, because
     * a silent substitution corrupts what it claims to convert. */
    if (jrt_is_int(v)) {
        int64_t i = jrt_unbox_int(v);
        if (i >= 0 && i <= 0x10FFFF && !(i >= 0xD800 && i <= 0xDFFF))
            return jrt_box_char((uint32_t)i);
        char msg[96];
        snprintf(msg, sizeof msg, "char(): %lld is not a Unicode scalar", (long long)i);
        throw_msg(msg);
    }
    if (jrt_is_str(v)) {
        const char* s = (const char*)jrt_unbox_ptr(v);
        unsigned char c = (unsigned char)s[0];
        if (c) {
            int step = (c >= 0xF0) ? 4 : (c >= 0xE0) ? 3 : (c >= 0xC0) ? 2 : 1;
            /* Exactly one character: the sequence must end the string. */
            if (s[step] == '\0') {
                uint32_t cp;
                switch (step) {
                    case 1: cp = (uint32_t)(c & 0x7F); break;
                    case 2: cp = (uint32_t)(c & 0x1F); break;
                    case 3: cp = (uint32_t)(c & 0x0F); break;
                    default: cp = (uint32_t)(c & 0x07); break;
                }
                for (int k = 1; k < step; k++) {
                    cp = (cp << 6) | (uint32_t)((unsigned char)s[k] & 0x3F);
                }
                return (int64_t)jrt_box_char_trust(cp, jrt_trust_of(s));
            }
        }
        char msg[128];
        snprintf(msg, sizeof msg,
                 "char(): expected a string of exactly one character, got \"%s\"", s);
        throw_msg(msg);
    }
    throw_msg("char(): cannot convert value to char");
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
     * a short scalar formats via the shared jrt_snprintf_any as a TRUSTED
     * string. (A non-string heap object renders as jrt_snprintf_any's
     * "<object>" placeholder — the same ObjKind gap the Chunk backend
     * documents.) */
    jade_value_t v = (jade_value_t)val;
    if (jrt_is_str(v)) {
        /* A *copy*, not the caller's pointer. This used to hand back the string
         * itself, which made the result's ownership depend on the value's type
         * — fresh for an int, borrowed for a string — and there is no way for a
         * caller to fold parts of both kinds without getting one of them wrong.
         * It did: an f-string whose only part was a string stored that very
         * pointer as a second owner, and once strings became reference-counted
         * that was a double free. The header says "freshly-allocated"; now it
         * is. Trust travels with the copy. */
        char* s = (char*)jrt_unbox_ptr(v);
        return jrt_str_dup(s, jrt_trust_of(s));
    }
    if (jrt_is_ptr(v) || jrt_is_float(v)) {
        /* Render a kind-tagged collection into a fresh TRUSTED tagged string.
         * A float takes the same path: str(48000.0) is "48000.0", from the one
         * renderer both engines share, and its text has no length bound. */
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

/* Returns an **owned** reference — the caller takes it as-is and must not
 * retain it again.
 *
 * A data field is borrowed from the struct or dict holding it, so it is
 * increfed on the way out. A method used as a value is not borrowed from
 * anything: `jrt_bind_method_new` mints a fresh object, already at one. Making
 * both owned is what lets the one caller (codegen's `GetField`) treat the
 * result uniformly. It used to retain unconditionally instead, which was right
 * for a field and wrong for a method — every `let f = obj.method` left an
 * over-retained bound method that could never be reclaimed. */
int64_t jrt_get_field(int64_t obj, const char* field) {
    jade_value_t v = (jade_value_t)obj;
    /* A caught runtime error is a bare (message) string in the AOT, whereas the
     * VM models it as a RuntimeError struct with a `.message` field. So
     * `error.message` on a string yields the string itself — matching the VM. */
    if (jrt_is_str(v) && strcmp(field, "message") == 0) {
        jrt_incref(obj);
        return obj;
    }
    if (jrt_is_ptr(v)) {
        void* p = jrt_unbox_ptr(v);
        if (jrt_kind_of(p) == JK_STRUCT) {
            int64_t out;
            if (jrt_coll_struct_get(p, field, &out)) {
                jrt_incref(out);
                return out;
            }
            /* Not a data field — try an extend method, which yields a bound
             * method value (`let greet = person.greet`). Data fields win, which
             * is the VM's order too (vm.rs GetField: field lookup first, then
             * extend_methods). Method *calls* never reach here: codegen elides
             * the producing GetField when the result is immediately called, so
             * what arrives is exactly a method used as a value. */
            void* bm = jrt_bind_method_new(obj, field);
            if (bm) return jrt_box_ptr(bm);
            /* Name the type and the field, the way the interpreter does. */
            {
                char msg[256];
                char* tn = jrt_get_type_name(obj);
                snprintf(msg, sizeof msg, "struct '%s' has no field '%s'",
                         tn ? tn : "", field ? field : "");
                jrt_str_free(tn);
                throw_msg(msg);
            }
        }
        /* A dict reads `d.key` as `d["key"]`, matching the VM (vm/dispatch.rs
         * GetField, the VmValue::Dict arm). Without this arm every dot-access on
         * a dict raised "value has no fields" when compiled and worked fine
         * interpreted — which took out `sh.output(cmd).code` and every package
         * namespace reached by a dot. Method *calls* never arrive here: codegen
         * elides the producing GetField when the result is immediately called,
         * so `d.keys()` was never affected and the divergence only showed up on
         * data keys. */
        if (jrt_kind_of(p) == JK_DICT) {
            int64_t out;
            if (jrt_coll_dict_get(p, field, &out)) {
                jrt_incref(out);
                return out;
            }
            throw_msg("undefined field");
        }
    }
    throw_msg("value has no fields");
    return JRT_NIL;
}

/* A value reached through an indirect call has to be something callable.
 *
 * Codegen used to untag whatever word was in the callee register and load a
 * function pointer out of it, so `fn go(f) { f(1) }; go(5)` dereferenced the
 * integer 5 and took the process down with SIGSEGV. The VM raises a catchable
 * "value is not callable" for the same program, which is the behaviour this
 * restores — the message matches JadeError::NotCallable, minus the span (a
 * compiled binary has no span at runtime, the same as every other raise here).
 *
 * Callable means a TAG_PTR whose ObjKind byte is Fn (a static fn box or a
 * native fn value) or BoundMethod. Everything else — an int, nil, a string, an
 * array, a struct — is not. */
void jrt_require_callable(int64_t v) {
    jade_value_t w = (jade_value_t)v;
    if (jrt_is_ptr(w)) {
        int64_t k = jrt_kind_of(jrt_unbox_ptr(w));
        if (k == JK_FN || k == JK_BOUND_METHOD) { return; }
    }
    throw_msg("value is not callable");
}

/* Guards for the places codegen untags a word on the strength of a *static*
 * type rather than the word's own tag.
 *
 * Inference is not always precise enough to make that safe. A function whose
 * branches return different types takes the first branch's type; an Unknown
 * value pushed into a typed container keeps the container's element type. When
 * the static type is then wrong, the compiled binary read an int as a `char*`
 * or as a boxed double and died, where the interpreter checks the value it
 * actually has and raises. These restore that check at the two sites that
 * dereference, wording it the way the VM does. */
static const char* value_kind_name(jade_value_t v);  /* defined below */

void jrt_require_str_val(int64_t v) {
    if (jrt_is_str((jade_value_t)v)) { return; }
    throw_msg("type error: expected str");
}

void jrt_require_float_val(int64_t v) {
    if (jrt_is_float((jade_value_t)v)) { return; }
    throw_msg("type error: expected float");
}

/* A dict key is used as a `char*`, so a non-string key is the same hazard one
 * container along. `{true: "y"}` took the process down; the VM names the kind
 * it got, and so does this. */
void jrt_require_dict_key(int64_t v) {
    if (jrt_is_str((jade_value_t)v)) { return; }
    char msg[128];
    snprintf(msg, sizeof msg, "type error: dict key must be str, got %s",
             value_kind_name((jade_value_t)v));
    throw_msg(msg);
}

/* Call `recv.name(args)` when the call site's static guess about the receiver
 * did not hold.
 *
 * Deliberately routed through jrt_get_field rather than straight at the method
 * registry, because that function already implements the interpreter's order: a
 * *data field* wins over a method of the same name, so a struct with a field
 * holding a function calls that function. Going to the registry first ran
 * another type's method instead. What comes back is a value, so jrt_call_value
 * enters it — a bound method gets its receiver, and arity is checked by the
 * callee's own entry. */
int64_t jrt_method_fallback(int64_t recv, const char* name, int64_t argc, const int64_t* argv) {
    int64_t callee = jrt_get_field(recv, name);   /* raises if there is neither */
    int64_t r = jrt_call_value(callee, argc, argv);
    jrt_decref(callee);                           /* jrt_get_field returns owned */
    return r;
}

/* Resolve `recv.method` against the receiver's runtime type, raising the way the
 * interpreter does when there is nothing to call.
 *
 * The Rust half reports failure through `status` instead of raising, because a
 * raise is a longjmp and must not unwind through a Rust frame. So the wording
 * lives here:
 *   - a receiver that is not a struct gets the same "<kind> has no method '<m>'"
 *     that jrt_require_kind raises for a primitive method;
 *   - a struct whose type declares no such method gets the interpreter's
 *     "struct '<Type>' has no field '<m>'", since `obj.m` is a field read first.
 *
 * Jumping through the null this used to return was a segfault the program could
 * not catch. */
void* jrt_method_resolve_or_raise(int64_t recv, const char* method) {
    int32_t status = 0;
    void* f = (void*)jrt_method_resolve(recv, method, &status);
    if (status == 0 && f) { return f; }
    char msg[256];
    if (status == 1) {
        snprintf(msg, sizeof msg, "%s has no method '%s'",
                 value_kind_name((jade_value_t)recv), method ? method : "");
    } else {
        char* tn = jrt_get_type_name(recv);
        snprintf(msg, sizeof msg, "struct '%s' has no field '%s'",
                 tn ? tn : "", method ? method : "");
        jrt_str_free(tn);
    }
    throw_msg(msg);
    return NULL;
}

/* ── Calling a value ──────────────────────────────────────────────────────
 *
 * Every callable a Jade program can hold — a plain function, a bound method, a
 * native package binding — is entered the same way:
 *
 *     int64_t entry(int64_t argc, const int64_t* argv)
 *
 * Codegen emits one `jf_ind_<uid>` entry per function that can become a value.
 * The entry is what checks arity and fills the defaults the caller left off,
 * because it is the only place that knows the callee's parameter list. A call
 * site cannot: it does not know which function the value holds.
 *
 * Before this, a call site built a fixed-arity call from the *arguments it had*
 * and jumped straight at the body. So `fn one(a) {…}; f(1, 2)` through a value
 * silently dropped an argument, `f()` read an uninitialised register as `a`,
 * and a function with a default parameter got garbage for it. The interpreter
 * raised on the first two and filled the third. */
void jrt_throw_arity(int64_t expected, int64_t got) {
    char m[128];
    snprintf(m, sizeof m, "wrong number of arguments: expected %lld, got %lld",
             (long long)expected, (long long)got);
    throw_msg(m);
}

typedef int64_t (*jade_entry_t)(int64_t argc, const int64_t* argv);

/* No real Jade call comes near this. It exists so a corrupt `argc` cannot turn
 * the receiver-prepending buffer below into an unbounded stack allocation. */
#define JRT_MAX_CALL_ARGS 256

int64_t jrt_call_value(int64_t callee, int64_t argc, const int64_t* argv) {
    jrt_require_callable(callee);
    void* box = jrt_unbox_ptr((jade_value_t)callee);
    if (argc < 0 || argc > JRT_MAX_CALL_ARGS) { throw_msg("too many arguments"); }

    if (jrt_kind_of(box) == JK_BOUND_METHOD) {
        /* {header@0, fn_ptr@16, self@24} — the receiver goes in front, so a
         * bound method reaches its implementation with the same `self` a direct
         * `obj.m(x)` would pass. */
        int64_t* w = (int64_t*)box;
        jade_entry_t fn = (jade_entry_t)(void*)(intptr_t)w[2];
        /* A VLA rather than malloc: the callee may raise, and a raise is a
         * longjmp that would skip the free. */
        int64_t buf[argc + 1];
        buf[0] = w[3];
        for (int64_t i = 0; i < argc; i++) { buf[i + 1] = argv[i]; }
        return fn(argc + 1, buf);
    }

    /* {entry@0, kind@8, [env@16]}. A native binding is told apart by the
     * sentinel address at offset 0, the way an indirect call always has. */
    void** slot = (void**)box;
    if (slot[0] == (void*)jrt_native_call) {
        void** env = (void**)slot[2];
        void*  handle = *(void**)env[0];
        return (int64_t)jrt_native_call(handle, (const char*)env[1],
                                        (const jade_value_t*)argv, argc);
    }
    return ((jade_entry_t)slot[0])(argc, argv);
}

/* `len(x)`, raising for a value that has no length the way the interpreter does.
 * jrt_len_chunk answers -1 for those rather than raising, because a raise is a
 * longjmp and must not cross a Rust frame. Reading the header's `len` field
 * unconditionally, as this used to, answered for values that have no length:
 * `len(some_fn)` was 1 and `len(5)` was 0. */
int64_t jade_len(int64_t w) {
    int64_t n = jrt_len_chunk(w);
    if (n < 0) { throw_msg("type error: len"); }
    return n;
}

/* ── Core builtins as values ──────────────────────────────────────────────
 *
 * `len`, `str` and the rest are ordinary names, so a program can hold one
 * without calling it: `let f = len`, `xs.map(str)`. The interpreter has a value
 * for each; a compiled binary had none, so the name loaded an empty global cell
 * and evaluated to nil. Printing it said `nil` and calling it crashed.
 *
 * Each gets the same shape a compiled function gets: an entry with the uniform
 * `(argc, argv)` signature, in a `{entry, kind}` box. The boxes are static
 * constants, like the ones codegen emits for a `fn`, so handing one out costs
 * nothing and the refcount ops skip it on the ObjKind byte.
 *
 * The bodies are here rather than in codegen because this is where the symbols
 * they forward to already live, and because each is one line. Keep the list in
 * `jrt_builtin_value` and codegen's `BUILTIN_VALUES` in step: a name in one and
 * not the other either crashes at startup or silently reads a nil global. */
static void jrt_want_one(int64_t argc) {
    if (argc != 1) { jrt_throw_arity(1, argc); }
}

static int64_t jb_len(int64_t argc, const int64_t* argv) {
    jrt_want_one(argc);
    return (int64_t)jrt_box_int(jade_len(argv[0]));
}
static int64_t jb_str(int64_t argc, const int64_t* argv) {
    jrt_want_one(argc);
    return (int64_t)jrt_box_str(jrt_str_of_any(argv[0]));
}
static int64_t jb_int(int64_t argc, const int64_t* argv) {
    jrt_want_one(argc);
    return jrt_int_any(argv[0]);
}
static int64_t jb_float(int64_t argc, const int64_t* argv) {
    jrt_want_one(argc);
    return jrt_float_any(argv[0]);
}
static int64_t jb_bool(int64_t argc, const int64_t* argv) {
    jrt_want_one(argc);
    return jrt_bool_any(argv[0]);
}
static int64_t jb_char(int64_t argc, const int64_t* argv) {
    jrt_want_one(argc);
    return jrt_char_any(argv[0]);
}
/* `print(x)` and `print(x, end)`. The second argument replaces the newline,
 * which is how `print("a", "")` writes without one. Fixing this at one argument
 * made a compiled `p("a", "-")` raise where the interpreter printed. The count
 * in the error is 1, matching what the interpreter reports for its own print. */
static int64_t jb_print(int64_t argc, const int64_t* argv) {
    if (argc < 1 || argc > 2) { jrt_throw_arity(1, argc); }
    if (argc == 1) {
        jrt_print_any(argv[0], "\n");
        return JRT_NIL;
    }
    jade_value_t end = (jade_value_t)argv[1];
    if (jrt_is_str(end)) {
        jrt_print_any(argv[0], (const char*)jrt_unbox_ptr(end));
    } else {
        char* r = jrt_render_any(argv[1]);
        jrt_print_any(argv[0], r ? r : "");
        free(r);
    }
    return JRT_NIL;
}
static int64_t jb_write(int64_t argc, const int64_t* argv) {
    jrt_want_one(argc);
    jrt_write_any(argv[0]);
    return JRT_NIL;
}

/* {entry@0, kind@8, name@16} — codegen's static fn box plus the name, so
 * `print(len)` can say `<builtin len>` the way the interpreter does. */
typedef struct { void* entry; int64_t kind; const char* name; } jrt_fnbox_t;
#define JRT_FN_BOX(sym, fn, sub, nm) \
    static const jrt_fnbox_t sym = { (void*)(fn), JRT_FN_KIND(sub), nm }

/* The interpreter does not model these alike, and the names it prints say so:
 * `len` and `write` are plain builtins, the type constructors are type
 * references, and `print` is a native fn. The sub-kind carries that across. */
JRT_FN_BOX(jb_box_len,   jb_len,   JRT_FN_BUILTIN,  "len");
JRT_FN_BOX(jb_box_write, jb_write, JRT_FN_BUILTIN,  "write");
JRT_FN_BOX(jb_box_str,   jb_str,   JRT_FN_TYPEREF,  "str");
JRT_FN_BOX(jb_box_int,   jb_int,   JRT_FN_TYPEREF,  "int");
JRT_FN_BOX(jb_box_float, jb_float, JRT_FN_TYPEREF,  "float");
JRT_FN_BOX(jb_box_bool,  jb_bool,  JRT_FN_TYPEREF,  "bool");
JRT_FN_BOX(jb_box_char,  jb_char,  JRT_FN_TYPEREF,  "char");
JRT_FN_BOX(jb_box_print, jb_print, JRT_FN_NATIVEFN, "print");

int64_t jrt_builtin_value(const char* name) {
    const jrt_fnbox_t* box = NULL;
    if      (strcmp(name, "len")   == 0) box = &jb_box_len;
    else if (strcmp(name, "str")   == 0) box = &jb_box_str;
    else if (strcmp(name, "int")   == 0) box = &jb_box_int;
    else if (strcmp(name, "float") == 0) box = &jb_box_float;
    else if (strcmp(name, "bool")  == 0) box = &jb_box_bool;
    else if (strcmp(name, "char")  == 0) box = &jb_box_char;
    else if (strcmp(name, "print") == 0) box = &jb_box_print;
    else if (strcmp(name, "write") == 0) box = &jb_box_write;
    else return JRT_NIL;
    /* Casting the const away is safe: nothing writes to a fn box. It is const
     * so that a stray write faults instead of corrupting a shared object. */
    return (int64_t)jrt_box_ptr((void*)(uintptr_t)box);
}

/* The `...base` of a copy-with struct literal has to be a struct.
 *
 * Called once per literal even when every field is named and there is nothing
 * left to copy, because the VM checks the base unconditionally and the two
 * engines have to fail the same way. Message matches JadeError::NotAStruct. */
void jrt_require_struct(int64_t v) {
    jade_value_t b = (jade_value_t)v;
    if (jrt_is_ptr(b) && jrt_kind_of(jrt_unbox_ptr(b)) == JK_STRUCT) { return; }
    throw_msg("value is not a struct");
}

/* Copy one field out of a copy-with base into the struct being built.
 *
 * A field the base does not carry is not an error: it falls through to the
 * type's declared default, which the caller sets afterwards. jrt_kstruct_set
 * retains, so the new struct owns its share of a heap value it now shares. */
void jrt_kstruct_copy_field(void* dest, int64_t base, const char* field) {
    jade_value_t b = (jade_value_t)base;
    if (!jrt_is_ptr(b)) { return; }
    void* p = jrt_unbox_ptr(b);
    if (jrt_kind_of(p) != JK_STRUCT) { return; }
    int64_t out;
    if (jrt_coll_struct_get(p, field, &out)) { jrt_kstruct_set(dest, field, out); }
}

/* Set a declared default only where a `...base` left the field empty.
 *
 * The plain setter overwrites, and the defaults are written after the base is
 * copied, so using it here would undo every field the copy just supplied. */
void jrt_kstruct_set_if_absent(void* s, const char* field, int64_t val) {
    int64_t out;
    if (jrt_coll_struct_get(s, field, &out)) {
        /* The caller built `val` before knowing it would not be wanted — a
         * prompt, a string, a fresh collection — and nothing else will ever
         * hold it. Dropping it on the floor here leaked one object per copy. */
        jrt_decref(val);
        return;
    }
    jrt_kstruct_set(s, field, val);
}

void jrt_set_field(int64_t obj, const char* field, int64_t val) {
    jade_value_t v = (jade_value_t)obj;
    if (jrt_is_ptr(v)) {
        void* p = jrt_unbox_ptr(v);
        if (jrt_kind_of(p) == JK_STRUCT) { jrt_kstruct_set(p, field, val); return; }
    }
    throw_msg("value does not support field assignment");
}

/* i-th UTF-8 codepoint of a tagged string as a char immediate, preserving the
 * source's trust; raises on out-of-range.
 *
 * Returned a fresh one-character string until v1.2.1, which allocated once per
 * character of every string scan. A char is an immediate, so this now allocates
 * nothing. The trust byte moves from the string header into bit 63 of the word,
 * because a character of a tainted string is still tainted. */
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
            /* Decode the UTF-8 sequence to a scalar. */
            uint32_t cp;
            switch (step) {
                case 1: cp = (uint32_t)(c & 0x7F); break;
                case 2: cp = (uint32_t)(c & 0x1F); break;
                case 3: cp = (uint32_t)(c & 0x0F); break;
                default: cp = (uint32_t)(c & 0x07); break;
            }
            for (int k = 1; k < step; k++) {
                cp = (cp << 6) | (uint32_t)((unsigned char)p[k] & 0x3F);
            }
            return (int64_t)jrt_box_char_trust(cp, jrt_trust_of(s));
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
        if (kind == JK_BYTES) {
            /* An octet is an int in 0..=255, not a char: a byte is not a
             * Unicode scalar, and making b[0] look like s[0] would hide that
             * they differ on any non-ASCII input. */
            if (!jrt_is_int((jade_value_t)idx)) throw_msg("bytes index must be int");
            int64_t r = jrt_bytes_get(p, jrt_unbox_int((jade_value_t)idx));
            if (r < 0) throw_msg("bytes index out of bounds");
            return jrt_box_int(r);
        }
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
        if (kind == JK_BYTES) {
            /* A blob is reference-semantic like an array, so the write lands in
             * place and the SAME container word goes back. Handing back a fresh
             * pointer would make codegen decref the original and free a buffer
             * the program still holds.
             *
             * The range checks live in the Rust half (jade-runtime,
             * src/bytesf.rs `set`) so the two engines word them identically; a
             * program can catch this, which makes the text part of the language. */
            if (!jrt_is_int((jade_value_t)idx)) throw_msg("bytes index must be int");
            if (!jrt_is_int((jade_value_t)val)) throw_msg("bytes value must be int");
            if (!jrt_bytes_set(p, jrt_unbox_int((jade_value_t)idx),
                               jrt_unbox_int((jade_value_t)val))) {
                bytes_throw_pending("bytes index assignment failed");
            }
            return obj;
        }
        if (kind == JK_DICT) {
            /* Dicts are value-semantic, so a write has to leave any alias of
             * this dict alone — the caller rebinds its variable to whatever
             * comes back. That used to mean copying on every single write,
             * which made filling a dict quadratic in its size.
             *
             * The copy is only observable when someone else is holding the
             * dict, and the refcount answers precisely that. Sole owner, write
             * in place and hand the same container back; shared, copy exactly
             * as before. Identical semantics either way. */
            if (!jrt_is_str((jade_value_t)idx)) throw_msg("dict index must be str");
            if (jrt_obj_unique(p)) {
                jrt_kdict_set(p, idx, val);
                return obj;
            }
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

/* ── Primitive-method receiver guard ──────────────────────────────────────
 *
 * The name of a primitive method does NOT prove the receiver's kind. The Chunk
 * backend used to assume it did — `chunk_val_method_supported` picked the arm
 * from the method name alone and then untagged the receiver straight to a
 * pointer — so `v.keys()` on a string dereferenced a char* as a DictObj and
 * `v.upper()` on an int dereferenced a small integer. The frontend cannot close
 * this: with an untyped parameter (`fn f(v) { v.keys() }`) the receiver's kind
 * is only known at runtime, which is exactly why the VM checks there and raises
 * UndefinedField. This is that check, for the compiled path.
 *
 * It must be a raise and not an abort: the idiomatic Jade type test is
 * `try { v.keys(); return true } catch e { return false }`, and a `catch` can
 * only see a thrown value. A segfault, or a Rust-side panic in a `no_mangle`
 * function, unwinds nothing and takes the process down. */
static const char* value_kind_name(jade_value_t v) {
    /* Order matters: jrt_is_int tests bit0 only, so it must come after the
     * tagged kinds whose low bits also clear it. */
    if (jrt_is_nil(v))   return "nil";
    if (jrt_is_bool(v))  return "bool";
    if (jrt_is_str(v))   return "str";
    if (jrt_is_float(v)) return "float";
    if (jrt_is_int(v))   return "int";
    if (jrt_is_ptr(v)) {
        void* p = jrt_unbox_ptr(v);
        if (!p) return "nil";
        switch (jrt_kind_of(p)) {
            case JK_ARRAY:  return "array";
            case JK_DICT:   return "dict";
            case JK_BYTES:  return "bytes";
            case JK_PROMPT: return "prompt";
            case JK_STRUCT: {
                /* A fresh tagged string; leaked deliberately — this path raises
                 * and the message borrows the bytes. */
                char* n = jrt_get_type_name((int64_t)v);
                return (n && *n) ? n : "struct";
            }
            default: return "value";
        }
    }
    return "value";
}

/* Which JRT_WANT_* bit `v` satisfies (0 for a kind no primitive method takes). */
static int32_t value_want_bit(jade_value_t v) {
    if (jrt_is_str(v)) return JRT_WANT_STR;
    if (jrt_is_ptr(v)) {
        void* p = jrt_unbox_ptr(v);
        if (!p) return 0;
        if (jrt_kind_of(p) == JK_ARRAY) return JRT_WANT_ARRAY;
        if (jrt_kind_of(p) == JK_DICT)  return JRT_WANT_DICT;
        if (jrt_kind_of(p) == JK_BYTES) return JRT_WANT_BYTES;
    }
    return 0;
}

void jrt_require_kind(int64_t recv, int32_t want, const char* method) {
    jade_value_t v = (jade_value_t)recv;
    if (value_want_bit(v) & want) return;
    /* Word for word what the interpreter raises. It said "struct" for every
     * receiver until v1.3.21 — an int is not a struct and has no fields — and
     * the two engines have to agree on the text, which the parity fixture
     * `examples/exceptions/error_values` asserts by substring. */
    /* A dict reads `d.name` as a key lookup before it looks for a method, so
     * the interpreter names both when neither is there. Every other kind has
     * only methods to miss. */
    char msg[256];
    const char* kind = value_kind_name(v);
    if (strcmp(kind, "dict") == 0) {
        snprintf(msg, sizeof msg, "dict has no key or method '%s'", method ? method : "");
    } else {
        snprintf(msg, sizeof msg, "%s has no method '%s'", kind, method ? method : "");
    }
    throw_msg(msg);
}

/* The same hazard one level in: a str method's arguments are untagged to char*
 * too, so `"abc".starts_with(42)` would read a small integer as a string. The
 * VM reports a bad argument differently from a bad receiver ("type error:
 * str.starts_with", not "has no field"), so this raises that wording. */
void jrt_require_str_arg(int64_t val, const char* method) {
    if (jrt_is_str((jade_value_t)val)) return;
    char msg[128];
    snprintf(msg, sizeof msg, "type error: str.%s", method ? method : "");
    throw_msg(msg);
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
        int64_t kind = jrt_kind_of(p);
        if (kind == JK_ARRAY) {
            int64_t n = jrt_coll_array_len(p);
            for (int64_t i = 0; i < n; i++) {
                /* jrt_core_eq_total, not jrt_eq_any: membership asks whether any
                 * element *is* the needle, and an element of another kind answers
                 * "no" rather than raising. jrt_eq_any is the `==` operator, which
                 * is strict across kinds by design — using it here made
                 * `[1, "a"].contains("a")` raise in a compiled binary while the VM
                 * answered true. Mixed arrays were unwritable until v1.1.32, so
                 * nothing reached it. */
                if (jrt_core_eq_total((int64_t)jrt_coll_array_get(p, i), (int64_t)needle)) return 1;
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
    if (e) { jrt_throw_io(e); jrt_str_free(e); }
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
/* A wrong-typed body is a *type* error, not an I/O one — the request never
 * happens. So it is checked and thrown here rather than routed through the
 * pending-error channel, which prefixes "I/O error: ". The sentence matches the
 * VM's (`src/uhttp/mod.rs`, `src/http/mod.rs`) minus the source span a compiled
 * binary does not carry, the way throw_cmp_type already handles comparisons. */
static void require_bytes_body(int64_t body, const char* fn) {
    jade_value_t v = (jade_value_t)body;
    if (jrt_is_ptr(v) && jrt_kind_of(jrt_unbox_ptr(v)) == JK_BYTES) return;
    char msg[160];
    snprintf(msg, sizeof msg, "%s expects bytes, got %s", fn, jrt_type_name_of(body));
    throw_msg(msg);
}
jade_value_t jrt_http_get_bytes(const char* url, void* headers) {
    jade_value_t r = (jade_value_t)jrt_http_get_bytes_impl(url, headers);
    http_throw_pending();
    return r;
}
jade_value_t jrt_http_post_bytes(const char* url, int64_t body, void* headers) {
    require_bytes_body(body, "http.post_bytes");
    jade_value_t r = (jade_value_t)jrt_http_post_bytes_impl(url, body, headers);
    http_throw_pending();
    return r;
}
jade_value_t jrt_http_head(const char* url, void* headers) {
    jade_value_t r = (jade_value_t)jrt_http_head_impl(url, headers);
    http_throw_pending();
    return r;
}

/* uhttp.* forwarders: same contract as http.* — the Rust impls (jade-runtime
 * src/uhttpf.rs) record a pending error on transport failure (connect/timeout/
 * malformed response) instead of throwing; throw it here. A 4xx/5xx is a normal
 * status, not an error. */
static void uhttp_throw_pending(void) {
    char* e = jrt_uhttp_take_error();
    if (e) { jrt_throw_io(e); jrt_str_free(e); }
}

/* uhttp.stream(url, handler[, headers]) -> status.
 *
 * The driver loop lives here rather than in Rust because it calls back into
 * Jade: a function value's box holds the raw function pointer at offset 0, the
 * same convention jrt_coll_array_map uses. Keeping the call on this side also
 * keeps a raising handler's longjmp from unwinding through a Rust frame, which
 * is undefined behavior — the reason every other uhttp entry point records a
 * pending error instead of throwing.
 *
 * A handler returning `false` stops the stream early; closing the handle drops
 * the socket, which is how the server learns to stop sending. Matches the VM
 * (vm/call.rs, NativeFnId::UhttpStream), including returning the status even
 * when the handler stopped it early. */
int64_t jrt_uhttp_stream(const char* url, int64_t fn_word, void* headers) {
    void* h = jrt_uhttp_stream_open(url, headers);
    if (!h) {
        uhttp_throw_pending();     /* open failed → the pending error is set */
        return 0;
    }
    void** box = (void**)jrt_unbox_ptr((jade_value_t)fn_word);
    int64_t (*fn)(int64_t) = (int64_t (*)(int64_t))(*box);
    int64_t status = jrt_uhttp_stream_status(h);

    for (;;) {
        int64_t line = 0;
        int32_t r = jrt_uhttp_stream_next(h, &line);
        if (r == 0) break;                       /* end of stream */
        if (r < 0) {                             /* read failure */
            jrt_uhttp_stream_close(h);
            uhttp_throw_pending();
            return status;
        }
        /* Only an explicit `false` stops the stream. Any other return — nil from
         * a handler that just prints, a number, a string — keeps it running, so
         * a handler need not end in a boolean. */
        if ((jade_value_t)fn(line) == JRT_FALSE) break;
    }
    jrt_uhttp_stream_close(h);
    return status;
}
jade_value_t jrt_uhttp_get(const char* url, void* headers) {
    jade_value_t r = (jade_value_t)jrt_uhttp_get_impl(url, headers);
    uhttp_throw_pending();
    return r;
}
jade_value_t jrt_uhttp_post(const char* url, const char* body, void* headers) {
    jade_value_t r = (jade_value_t)jrt_uhttp_post_impl(url, body, headers);
    uhttp_throw_pending();
    return r;
}
jade_value_t jrt_uhttp_put(const char* url, const char* body, void* headers) {
    jade_value_t r = (jade_value_t)jrt_uhttp_put_impl(url, body, headers);
    uhttp_throw_pending();
    return r;
}
jade_value_t jrt_uhttp_delete(const char* url, void* headers) {
    jade_value_t r = (jade_value_t)jrt_uhttp_delete_impl(url, headers);
    uhttp_throw_pending();
    return r;
}
jade_value_t jrt_uhttp_head(const char* url, void* headers) {
    jade_value_t r = (jade_value_t)jrt_uhttp_head_impl(url, headers);
    uhttp_throw_pending();
    return r;
}
jade_value_t jrt_uhttp_get_bytes(const char* url, void* headers) {
    jade_value_t r = (jade_value_t)jrt_uhttp_get_bytes_impl(url, headers);
    uhttp_throw_pending();
    return r;
}
jade_value_t jrt_uhttp_post_bytes(const char* url, int64_t body, void* headers) {
    require_bytes_body(body, "uhttp.post_bytes");
    jade_value_t r = (jade_value_t)jrt_uhttp_post_bytes_impl(url, body, headers);
    uhttp_throw_pending();
    return r;
}

/* sh.exec/run forwarders: refuse a tainted command (a code-execution sink), call
 * the Rust impl (jade-runtime src/shf.rs), then throw any pending error it
 * recorded — sh.exec raises on a non-zero exit, run/exec on a spawn failure. */
char* jrt_sh_exec(const char* cmd) {
    jrt_refuse_if_tainted(cmd, "sh.exec(cmd)");
    char* r = jrt_sh_exec_impl(cmd);
    char* e = jrt_sh_take_error();
    if (e) { jrt_throw_io(e); jrt_str_free(e); }
    return r;
}
int64_t jrt_sh_run(const char* cmd) {
    jrt_refuse_if_tainted(cmd, "sh.run(cmd)");
    int64_t r = jrt_sh_run_impl(cmd);
    char* e = jrt_sh_take_error();
    if (e) { jrt_throw_io(e); jrt_str_free(e); }
    return r;
}
/* sh.output is the same sink and takes the same refusal. It has no pending-error
 * step because jrt_coll_sh_output reports a spawn failure as {"","",-1} rather
 * than raising; only the taint check is added here. Codegen calls this rather
 * than jrt_coll_sh_output directly, so the check cannot be bypassed by the
 * lowering forgetting about it. */
void* jrt_sh_output(const char* cmd) {
    jrt_refuse_if_tainted(cmd, "sh.output(cmd)");
    return jrt_coll_sh_output(cmd);
}

/* json.parse's raising forwarder. The Rust half records serde's complaint
 * instead of throwing (a longjmp must not cross a Rust frame), and this turns it
 * into the same catchable I/O error the VM raises.
 *
 * Without it a compiled binary answered nil for input the interpreter refused,
 * so a program took the success branch on malformed JSON and every try/catch
 * written around a parse stopped running. */
extern jade_value_t jrt_json_parse_impl(const char* s);
extern char* jrt_json_take_error(void);

jade_value_t jrt_json_parse(const char* s) {
    jade_value_t w = jrt_json_parse_impl(s);
    char* e = jrt_json_take_error();
    if (e) { jrt_throw_io(e); jrt_str_free(e); }
    return w;
}

/* fs.* raising forwarders: the Rust impls (jade-runtime src/fsf.rs) record a
 * pending error instead of throwing (a longjmp must not cross a Rust frame);
 * throw it here as a catchable exception. fs.read additionally refuses a tainted
 * path first (it is a read sink). */
static void fs_throw_pending(void) {
    char* e = jrt_fs_take_error();
    if (e) { jrt_throw_io(e); jrt_str_free(e); }
}
char* jrt_fs_read(const char* path, int32_t trust) {
    if (!trust) jrt_refuse_if_tainted(path, "fs.read(path)");
    char* r = jrt_fs_read_impl(path, trust);
    fs_throw_pending();
    return r;
}
/* Turn whatever the Rust half recorded into a catchable raise.
 *
 * throw_msg and NOT jrt_throw_io: the latter prepends "I/O error: ", and none of
 * these is an I/O failure. The wording has to match what the VM raises, because
 * a Jade program can catch it. (The message leaks on this path, exactly as the
 * fs forwarders' does — throw_msg longjmps, so nothing after it runs.) */
static void bytes_throw_pending(const char* fallback) {
    char* e = jrt_bytes_take_error();
    if (e) throw_msg(e);
    throw_msg(fallback);
}

/* bytes.zeros(n) — n zeroed octets. Raises on a negative or oversized length. */
int64_t jk_bytes_zeros(int64_t n) {
    if (!jrt_is_int((jade_value_t)n)) throw_msg("bytes.zeros() expects an int");
    void* p = jrt_bytes_zeros(jrt_unbox_int((jade_value_t)n));
    if (!p) bytes_throw_pending("bytes.zeros() failed");
    return (int64_t)jrt_box_ptr(p);
}

/* bytes.from_ints(arr) — a blob from an array of octet values. Takes the whole
 * tagged word: the Rust half checks the tag and the object kind, so a dict or a
 * struct is reported rather than read as an ArrayObj. */
int64_t jk_bytes_from_ints(int64_t arr_word) {
    void* p = jrt_bytes_from_ints(arr_word);
    if (!p) bytes_throw_pending("bytes.from_ints() failed");
    return (int64_t)jrt_box_ptr(p);
}

/* bytes.concat(a, b) — a new blob, tainted if either input is. */
int64_t jk_bytes_concat(int64_t a, int64_t b) {
    require_bytes_body(a, "bytes.concat");
    require_bytes_body(b, "bytes.concat");
    void* p = jrt_bytes_concat(jrt_unbox_ptr((jade_value_t)a), jrt_unbox_ptr((jade_value_t)b));
    return (int64_t)jrt_box_ptr(p);
}

/* `b.decode()` — raise on invalid UTF-8 rather than substituting replacement
 * characters. Same pending-error shape as the fs wrappers above: the Rust side
 * cannot longjmp, so it records and returns NULL and the throw happens here. */
int64_t jk_bytes_decode(int64_t recv) {
    void* p = jrt_unbox_ptr((jade_value_t)recv);
    char* s = jrt_bytes_decode(p);
    if (!s) {
        char* e = jrt_bytes_take_error();
        if (e) { jrt_throw_io(e); jrt_str_free(e); }
        throw_msg("bytes.decode(): not valid UTF-8");
    }
    return (int64_t)jrt_box_str(s);
}

int64_t jk_fs_read_bytes(const char* path, int32_t trust) {
    if (!trust) jrt_refuse_if_tainted(path, "fs.read_bytes(path)");
    void* p = jrt_fs_read_bytes_impl(path, trust);
    fs_throw_pending();
    return (int64_t)jrt_box_ptr(p);
}
/* copy/rename/rmdir/size — the Rust half records a pending error and returns a
 * neutral value, and these are what turn it into a Jade raise. Without the
 * forwarder the compiled program answers success where the interpreter raises,
 * and the two agree on every *passing* run, so only a failing case can see it. */
void jrt_fs_copy(const char* src, const char* dst) {
    jrt_fs_copy_impl(src, dst);
    fs_throw_pending();
}
void jrt_fs_rename(const char* src, const char* dst) {
    jrt_fs_rename_impl(src, dst);
    fs_throw_pending();
}
void jrt_fs_rmdir(const char* path) {
    jrt_fs_rmdir_impl(path);
    fs_throw_pending();
}
int64_t jrt_fs_size(const char* path) {
    int64_t n = jrt_fs_size_impl(path);
    fs_throw_pending();
    return n;
}

void jk_fs_write_bytes(const char* path, int64_t blob) {
    const void* b = jrt_unbox_ptr((jade_value_t)blob);
    jrt_fs_write_bytes_impl(path, jrt_bytes_data(b), (size_t)jrt_bytes_len(b));
    fs_throw_pending();
}
void jk_fs_append_bytes(const char* path, int64_t blob) {
    const void* b = jrt_unbox_ptr((jade_value_t)blob);
    jrt_fs_append_bytes_impl(path, jrt_bytes_data(b), (size_t)jrt_bytes_len(b));
    fs_throw_pending();
}

int64_t jk_fs_read_stdin_bytes(void) {
    void* p = jrt_fs_read_stdin_bytes_impl();
    fs_throw_pending();
    return (int64_t)jrt_box_ptr(p);
}
void jk_fs_write_stdout_bytes(int64_t blob) {
    const void* b = jrt_unbox_ptr((jade_value_t)blob);
    jrt_fs_write_stdout_bytes_impl(jrt_bytes_data(b), (size_t)jrt_bytes_len(b));
    fs_throw_pending();
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
        throw_msg(m);
    }
    return jrt_random_draw(lo, hi);
}

/* array.map(arr, fn) -> new array of fn(elem). Goes through jrt_call_value
 * rather than reading a function pointer out of the box itself, so a bound
 * method (`xs.map(obj.scale)`) and a native binding work here for the same
 * reason they work at an ordinary call site — and so the callee's arity is
 * checked once, in one place. Elements are tagged words. */
/* The package spelling `array.map(a, f)` reaches here with whatever the program
 * passed; the method spelling `a.map(f)` has already had its receiver checked.
 * Without this the first untagged a string as an array and walked it. */
static void jrt_require_array(int64_t w, const char* who) {
    if (jrt_is_ptr((jade_value_t)w)
        && jrt_kind_of(jrt_unbox_ptr((jade_value_t)w)) == JK_ARRAY) {
        return;
    }
    char m[128];
    snprintf(m, sizeof m, "type error: %s: first argument must be an array, got %s",
             who, value_kind_name((jade_value_t)w));
    throw_msg(m);
}

int64_t jrt_coll_array_map(int64_t arr_word, int64_t fn_word) {
    jrt_require_array(arr_word, "array.map");
    void* arr = jrt_unbox_ptr((jade_value_t)arr_word);
    void* out = jrt_karr_new();
    int64_t n = jrt_coll_array_len(arr);
    for (int64_t i = 0; i < n; i++) {
        int64_t e = jrt_coll_array_get(arr, i);
        int64_t r = jrt_call_value(fn_word, 1, &e);
        /* push retains, and the callback already handed back a reference of its
         * own, so releasing here is what keeps a mapped-to-collection from
         * leaking one object per element. */
        jrt_karr_push(out, r);
        jrt_decref(r);
    }
    return (int64_t)jrt_box_ptr(out);
}

/* array.filter(arr, fn) -> elements where fn(elem) is truthy. */
int64_t jrt_coll_array_filter(int64_t arr_word, int64_t fn_word) {
    jrt_require_array(arr_word, "array.filter");
    void* arr = jrt_unbox_ptr((jade_value_t)arr_word);
    void* out = jrt_karr_new();
    int64_t n = jrt_coll_array_len(arr);
    for (int64_t i = 0; i < n; i++) {
        int64_t e = jrt_coll_array_get(arr, i);
        int64_t r = jrt_call_value(fn_word, 1, &e);
        /* A predicate has to answer a bool. Feeding whatever came back to
         * jrt_unbox_bool read a tag bit as the truth value, so a predicate
         * returning `x % 2` quietly dropped every element instead of raising. */
        if (!jrt_is_bool((jade_value_t)r)) {
            char m[128];
            snprintf(m, sizeof m,
                     "type error: array.filter: predicate must return a bool, got %s",
                     value_kind_name((jade_value_t)r));
            jrt_decref(r);
            throw_msg(m);
        }
        if (jrt_unbox_bool((jade_value_t)r)) { jrt_karr_push(out, e); }
        jrt_decref(r);
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

/* jrt_snprintf_float is gone. It formatted with "%.*g" at the fewest
 * significant digits that round-tripped, and %g switches to exponent form
 * whenever the exponent reaches the precision — so print(10.0) produced
 * "1e+01" and str(48000.0) produced "4.8e+04" under `jade build` while the VM
 * printed "10.0" and "48000.0". The ".0" suffix was then suppressed by its own
 * strpbrk guard, because an 'e' was present.
 *
 * Float text now comes from the shared Rust renderer (jade-runtime,
 * src/render.rs format_float), which is what the VM already uses. See
 * jrt_print_any / jrt_str_of_any. */

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

/* math.abs / math.pow are overflow-checked in the shared Rust core
 * (jade-runtime, src/mathf.rs). Rust cannot longjmp, so the core reports
 * overflow through an out-param and these forwarders raise it here — as the
 * same "integer overflow" the VM raises for `+`/`-`/`*`, which is what both
 * engines previously failed to do (this one silently wrapped; the VM panicked
 * with a raw Rust overflow message). */
extern int64_t jrt_math_abs(int64_t w, uint32_t* err);
/* The float -> int conversions report the same way: `math.floor(math.inf())`
 * saturates to a whole number no tagged word can hold. */
extern int64_t jrt_math_floor(int64_t w, uint32_t* err);
extern int64_t jrt_math_ceil(int64_t w, uint32_t* err);
extern int64_t jrt_math_round(int64_t w, uint32_t* err);
extern int64_t jrt_math_trunc(int64_t w, uint32_t* err);
extern int64_t jrt_math_pow(int64_t a, int64_t b, uint32_t* err);

static void throw_msg(const char* m);

#define JADE_MATH_CHECKED1(name)                       \
    int64_t jade_math_##name(int64_t w) {               \
        uint32_t err = 0;                               \
        int64_t r = jrt_math_##name(w, &err);           \
        if (err) throw_msg("integer overflow");         \
        return r;                                       \
    }
JADE_MATH_CHECKED1(floor)
JADE_MATH_CHECKED1(ceil)
JADE_MATH_CHECKED1(round)
JADE_MATH_CHECKED1(trunc)

int64_t jade_math_abs(int64_t w) {
    uint32_t err = 0;
    int64_t r = jrt_math_abs(w, &err);
    if (err) throw_msg("integer overflow");
    return r;
}

int64_t jade_math_pow(int64_t a, int64_t b) {
    uint32_t err = 0;
    int64_t r = jrt_math_pow(a, b, &err);
    if (err) throw_msg("integer overflow");
    return r;
}

/* Raise "<op> requires numeric operands" (matches the VM's TypeError text). */
static void throw_num_type(const char* op) {
    char msg[64];
    snprintf(msg, sizeof msg, "%s requires numeric operands", op);
    throw_msg(msg);
}

/* Raise "'<op>' cannot compare <a> and <b>", the VM's wording for a comparison
 * across kinds (see map_dynop_err in src/vm/ops.rs).
 *
 * Comparisons used to share throw_num_type with arithmetic, so `1 == "x"` in a
 * compiled binary reported "'==' requires numeric operands" — misleading, since
 * the problem is that the kinds differ, not that they are non-numeric, and
 * divergent from what the VM says about the same program. Mixed arrays made this
 * easy to hit, so the two now agree apart from the source span, which a compiled
 * binary does not carry. */
static void throw_cmp_type(const char* op, jade_value_t a, jade_value_t b) {
    char msg[128];
    snprintf(msg, sizeof msg, "%s cannot compare %s and %s",
             op, jrt_core_type_name((int64_t)a), jrt_core_type_name((int64_t)b));
    throw_msg(msg);
}

/* Raise a runtime failure as the VM raises one: a `RuntimeError` struct with a
 * single `message` field, thrown under that type name.
 *
 * The VM funnels every non-`Exception` error through `make_vm_runtime_error`
 * (vm/exceptions.rs), so `catch e` binds a struct and `e.message` is the text.
 * This side threw the bare string instead, so the same `try` saw a str under
 * `jade build` and a struct under `jade run`: `e.message` raised compiled, and
 * `catch RuntimeError e` never matched at all. Any program that inspected a
 * caught error rather than just reporting it behaved differently once compiled.
 *
 * The message still carries no `[line:col]` prefix — compiled code has no span
 * at runtime — which is the one part of the text that stays different. Every
 * other raise here already omitted it, so nothing regresses; the wording after
 * the prefix now matches, including the `I/O error: ` that the VM's IoError
 * display adds (see `jrt_throw_io`). */
static void throw_msg(const char* m) {
    void* e = jrt_kstruct_new("RuntimeError");
    jrt_kstruct_set(e, "message", jrt_box_str(jrt_str_dup(m, JRT_TRUSTED)));
    jade_exc_throw_typed((int64_t)jrt_box_ptr(e), "RuntimeError");
}

/* The same wrapper, reachable from codegen. `Lowerer::throw` raises a bare value
 * because that is right for a user's `raise x` — the value thrown is the value
 * written. But codegen also raises its *own* failures (a zero divisor, integer
 * overflow), and those are runtime errors like any other, so they go through
 * here rather than arriving as a str the VM would have delivered as a struct. */
void jrt_throw_runtime(const char* msg) {
    throw_msg(msg);
}

/* An I/O failure, matching the VM's `IoError` display: the same `I/O error: `
 * prefix ahead of the detail, then wrapped as a RuntimeError like everything
 * else. Used by the fs/http/uhttp/sh forwarders, whose Rust halves record a
 * pending error rather than throwing (a longjmp must not cross a Rust frame). */
void jrt_throw_io(const char* detail) {
    char msg[1024];
    snprintf(msg, sizeof msg, "I/O error: %s", detail ? detail : "");
    throw_msg(msg);
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

/* Three-way ordering. `op` is the source operator ("'<'", "'>='", …) so a failure
 * names it the way the VM does; codegen passes it per call site. */
int jrt_cmp_any_op(jade_value_t a, jade_value_t b, const char* op) {
    uint32_t err = JRT_OP_OK;
    int c = jrt_core_cmp(a, b, &err);
    if (err == JRT_OP_TYPE) throw_cmp_type(op, a, b);
    else if (err) throw_op_err(err, op);
    return c;
}

int jrt_cmp_any(jade_value_t a, jade_value_t b) {
    return jrt_cmp_any_op(a, b, "comparison");
}

/* Dynamic ==/!=. VM-strict: cross-kind operands (e.g. int vs float) raise a
 * catchable TypeError instead of silently comparing. codegen calls this for
 * both == and != (negating the result), so both raise on a kind mismatch.
 *
 * Membership does not come through here — see jrt_core_eq_total. */
int jrt_eq_any(uint64_t a, uint64_t b) {
    uint32_t err = JRT_OP_OK;
    int r = jrt_core_eq((jade_value_t)a, (jade_value_t)b, &err);
    if (err == JRT_OP_TYPE) throw_cmp_type("'=='", (jade_value_t)a, (jade_value_t)b);
    else if (err) throw_op_err(err, "'=='");
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

/* ── Per-frame scoping of the handler stack ───────────────────────────────
 *
 * `jade_exc_pop` only runs where codegen emits `PopHandler`, which the emitter
 * places on the try body's normal fall-through exit. A `return` inside a `try`
 * does not take that exit:
 *
 *     fn is_dict(v) { try { v.keys(); return true } catch e { return false } }
 *
 * so the frame stayed on the stack while the C frame holding its `jmp_buf`
 * disappeared with the return. A later `raise` then longjmp'd into a dead stack
 * frame — a segfault inside `_longjmp` when the buffer was clearly garbage, an
 * infinite spin when the stale bytes happened to look like a valid jump target,
 * and a spurious error when it landed in the wrong live handler. Which one you
 * got depended on what had run since, which is what made it look like heap
 * corruption rather than a leaked handler.
 *
 * The VM never had this: its `handlers` vec is a local of the dispatch call
 * frame (vm/dispatch.rs), so a function's handlers die with it. These two give
 * the compiled path the same scoping — codegen snapshots the depth on entry and
 * restores it on every return, so a leaked frame cannot outlive its function. */
int32_t jade_exc_depth(void) { return (int32_t)exc_depth; }

void jade_exc_restore(int32_t depth) {
    /* Only ever unwinds. A restore that would *raise* the depth means a frame
     * was popped that this function did not push, which must not resurrect a
     * dead buffer. */
    if (depth >= 0 && depth < exc_depth) exc_depth = depth;
}

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
        /* A raised struct carrying a `message` field reports that message. Every
         * runtime failure is now a `RuntimeError` struct rather than a bare
         * string, and without this an uncaught one degraded from
         * "jade: division by zero" to "jade: uncaught RuntimeError" — the type
         * name, and none of the information. Any struct with a str `message`
         * qualifies, so a user's own `raise MyError { message: … }` reads the
         * same way. */
        if (jrt_is_ptr((jade_value_t)value)) {
            void* p = jrt_unbox_ptr((jade_value_t)value);
            if (p && jrt_kind_of(p) == JK_STRUCT) {
                int64_t m = 0;
                if (jrt_coll_struct_get(p, "message", &m) && jrt_is_str((jade_value_t)m)) {
                    char buf[1056];
                    snprintf(buf, sizeof buf, "jade: %s",
                             (const char*)jrt_unbox_ptr((jade_value_t)m));
                    jade_rt_fatal(buf);
                }
            }
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

/* ── Recursion depth ──────────────────────────────────────────────────── */
/*
 * 10000 matches the VM's own MAX_CALL_DEPTH (src/vm/call.rs) so a program
 * that recurses past the limit fails identically under `jade run` and a
 * compiled binary, rather than one engine accepting a depth the other
 * aborts on (TOOLCHAIN-BUGS #10). This backend was already clean at 10000+
 * frames before this limit existed — native calls cost far less stack per
 * frame than the VM's dispatch loop — so the number is set for parity with
 * the VM, not because this engine needed the headroom.
 *
 * Thread-local for the same reason the exception stack is: `spawn` runs each
 * task on its own OS thread, and a deep call chain on one must not count
 * against the budget of an unrelated one.
 */
#define JRT_RECUR_MAX_DEPTH 10000

static _Thread_local int32_t recur_depth = 0;

void jrt_recur_enter(void) {
    if (recur_depth >= JRT_RECUR_MAX_DEPTH) {
        throw_msg("recursion limit exceeded");
        /* unreachable: throw_msg longjmps or, with no active handler, exits. */
    }
    recur_depth++;
}

int32_t jrt_recur_depth(void) { return recur_depth; }

/* Only ever unwinds — see jade_exc_restore, whose reasoning this mirrors: a
 * restore that would *raise* the depth means some caller's own decrement
 * already ran, and letting it move backwards would double-count. */
void jrt_recur_restore(int32_t depth) {
    if (depth >= 0 && depth < recur_depth) recur_depth = depth;
}

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
/* jrt_str_upper / jrt_str_lower moved to the shared Rust runtime
 * (jade-runtime, src/strf.rs). The versions here used byte-wise toupper/
 * tolower, which is ASCII-only and writes one output byte per input byte — so
 * every non-ASCII string differed from the VM ("café".upper() was "CAFé"), and
 * mappings like ß -> SS could not be expressed at all. */

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
/* Refuse a tainted value at a code-execution sink.
 *
 * This *raises* rather than exiting. It used to fprintf and jade_rt_exit(1),
 * which meant the same program was catchable under `jade run` — the VM raises a
 * normal exception — and fatal when built. A `try { sh.exec(x) } catch e { … }`
 * therefore ran the handler in one engine and killed the process in the other.
 * The VM is the reference for what the language does, so this follows it.
 *
 * The message is byte-identical to `jade_runtime::trust::refusal_message`, with
 * no "jade: " prefix, because the parity gate diffs output. */
void jrt_refuse_if_tainted(const char* arg, const char* sink_name) {
    if (jrt_trust_of(arg) != JRT_TRUSTED) {
        char msg[256];
        snprintf(msg, sizeof msg,
            "refused tainted string in %s — value derived from an "
            "untrusted source (LLM, network, file, stdin) and cannot flow "
            "to a code-execution sink",
            sink_name);
        throw_msg(msg);
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

