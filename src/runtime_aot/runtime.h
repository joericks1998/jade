#pragma once
#include <stddef.h>
#include <stdint.h>

/* Uniform value representation: all Jade values fit in 64 bits. */
typedef int64_t jade_value_t;

/* ── Tagged value ABI (low-bit tags) ──────────────────────────────────
 *
 * Every jade_value_t carries its runtime kind in the low bits, so a value
 * read back from a type-erased slot (dict value, array element, Unknown
 * param/return) knows whether it is an int, float, bool, nil, or a heap
 * pointer — matching the VM's dynamically-typed values. Layout:
 *
 *   bit0 == 0          -> INT    (63-bit signed; value = (int64_t)v >> 1)
 *   low3 == 0b001 (1)  -> heap POINTER, non-string (dict/array/struct/fn/future)
 *   low3 == 0b011 (3)  -> BOXED FLOAT pointer (untag, then load the double)
 *   low3 == 0b101 (5)  -> heap STRING pointer
 *   low3 == 0b111 (7)  -> IMMEDIATE: nil or bool, disambiguated by bit3:
 *                           low4 == 0b0111 ( 7) -> nil/char, split on bit4:
 *                             low5 == 0b00111 ( 7) -> NIL
 *                             low5 == 0b10111 (23) -> CHAR, scalar in bits 5..
 *                           low4 == 0b1111 (15) -> BOOL, value in bit4
 *                                                  (false=15, true=31)
 *
 * `char` claims bit 4 of the nil branch, the only unused immediate space left.
 * That is why jrt_is_nil tests five bits and not four: before v1.2.1 any word
 * ending 0b0111 was nil whatever sat above it, so a char would have read as
 * nil. This macro and jade-runtime's `JadeValue::is_nil` are two copies of one
 * rule — change them together or a char prints as nil in exactly one engine.
 *
 * Strings get their own tag (separate from other heap objects) so the runtime
 * can tell a string from a dict/array/struct without a per-object kind header:
 * jrt_eq_any/jrt_cmp_any only strcmp two real strings, and json/print can
 * render by kind. All three heap kinds (ptr/float/str) untag with `& ~7`.
 *
 * Heap allocations (malloc, jrt_str_new) are >= 8-byte aligned, so the low
 * 3 bits of a real pointer are always 0 and free for the tag. INT uses only
 * bit0, leaving add/sub of two tagged ints correct without untagging (SMI).
 * Floats are heap-boxed (a malloc per float value) — the accepted cost of
 * keeping every value 64 bits wide. The tagged-string trust byte still lives
 * at offset -1 of the *untagged* char*: untag, then read [-1]. */
#define JRT_TAG_MASK   ((uint64_t)7)
#define JRT_TAG_PTR    ((uint64_t)1)   /* non-string heap object */
#define JRT_TAG_FLOAT  ((uint64_t)3)
#define JRT_TAG_STR    ((uint64_t)5)   /* heap string */
#define JRT_TAG_IMM    ((uint64_t)7)   /* nil, char or bool */
#define JRT_NIL        ((jade_value_t)0x07)  /* 0b00111 */
#define JRT_FALSE      ((jade_value_t)0x0F)  /* 0b01111 */
#define JRT_TRUE       ((jade_value_t)0x1F)  /* 0b11111 */
#define JRT_CHAR_TAG   ((uint64_t)0x17)      /* 0b10111, scalar in bits 5.. */
#define JRT_CHAR_MASK  ((uint64_t)0x1f)
#define JRT_CHAR_SHIFT 5
/* A char taken from a tainted string is still tainted, and a char has no
 * header to keep a trust byte in the way a string does. Bit 63 is clear of the
 * 21-bit scalar and of the tag, so the flag rides there. Mirrors the `trust`
 * field on jade-runtime's `JChar`. */
#define JRT_CHAR_TAINT ((uint64_t)1 << 63)

#define jrt_tag(v)        ((uint64_t)(v) & JRT_TAG_MASK)
#define jrt_is_int(v)     (((uint64_t)(v) & 1u) == 0)
#define jrt_is_ptr(v)     (jrt_tag(v) == JRT_TAG_PTR)
#define jrt_is_float(v)   (jrt_tag(v) == JRT_TAG_FLOAT)
#define jrt_is_str(v)     (jrt_tag(v) == JRT_TAG_STR)
/* Five bits: bit4 separates nil from char. See the layout note above. */
#define jrt_is_nil(v)     (((uint64_t)(v) & JRT_CHAR_MASK) == 0x7u)
#define jrt_is_char(v)    (((uint64_t)(v) & JRT_CHAR_MASK) == JRT_CHAR_TAG)
#define jrt_is_bool(v)    (((uint64_t)(v) & 0xfu) == 0xfu)  /* low4 == 0b1111 */
/* Any heap pointer kind (non-string ptr, boxed float, or string): all untag
 * with `& ~7`. Used to decide whether a value can be dereferenced as a ptr. */
#define jrt_is_heap(v)    (jrt_is_ptr(v) || jrt_is_float(v) || jrt_is_str(v))

#define jrt_box_int(i)    ((jade_value_t)((uint64_t)(int64_t)(i) << 1))
#define jrt_unbox_int(v)  ((int64_t)(v) >> 1)
#define jrt_box_bool(b)   ((jade_value_t)((((uint64_t)((b)!=0)) << 4) | 0xfu))
#define jrt_unbox_bool(v) ((int)(((uint64_t)(v) >> 4) & 1u))
#define jrt_box_char(c)   ((jade_value_t)(((uint64_t)(uint32_t)(c) << JRT_CHAR_SHIFT) | JRT_CHAR_TAG))
/* Masks the taint flag off before shifting, so the scalar is what comes back. */
#define jrt_unbox_char(v) ((uint32_t)((((uint64_t)(v)) & ~JRT_CHAR_TAINT) >> JRT_CHAR_SHIFT))
#define jrt_box_char_trust(c, t) \
    ((jade_value_t)((uint64_t)jrt_box_char(c) | ((t) ? JRT_CHAR_TAINT : (uint64_t)0)))
#define jrt_char_trust(v) ((uint8_t)((((uint64_t)(v)) & JRT_CHAR_TAINT) ? 1u : 0u))
/* The comparable part of a char word: scalar plus tag, taint excluded. Trust is
 * provenance, not identity — two spellings of 'a' are the same character. */
#define jrt_char_bits(v)  (((uint64_t)(v)) & ~JRT_CHAR_TAINT)
#define jrt_box_ptr(p)    ((jade_value_t)((uintptr_t)(p) | JRT_TAG_PTR))
#define jrt_box_str(p)    ((jade_value_t)((uintptr_t)(p) | JRT_TAG_STR))
#define jrt_unbox_ptr(v)  ((void*)((uintptr_t)(v) & ~(uintptr_t)7))

/* Heap-box a double: allocate 8 (aligned) bytes, store the value, return the
 * pointer tagged with JRT_TAG_FLOAT. jrt_unbox_float loads it back. */
jade_value_t jrt_box_float(double d);
double       jrt_unbox_float(jade_value_t v);

/* Tag-erased arithmetic / comparison for when one or both operands are
 * statically Unknown. Each takes tagged operands and returns a tagged value,
 * dispatching on the runtime tag and following the VM's mixed int/float rules
 * (int op int -> int unless it must be float; any float -> float). cmp_any
 * returns -1/0/1; eq_any returns 1/0. */
jade_value_t jrt_add_any(jade_value_t a, jade_value_t b);
jade_value_t jrt_sub_any(jade_value_t a, jade_value_t b);
jade_value_t jrt_mul_any(jade_value_t a, jade_value_t b);
jade_value_t jrt_div_any(jade_value_t a, jade_value_t b);
jade_value_t jrt_mod_any(jade_value_t a, jade_value_t b);
jade_value_t jrt_pow_any(jade_value_t a, jade_value_t b);
jade_value_t jrt_neg_any(jade_value_t a);
int          jrt_cmp_any(jade_value_t a, jade_value_t b);
/* jrt_cmp_any_op — as jrt_cmp_any, but `op` names the source operator ("'<'")
 * so a cross-kind failure reads like the VM's message. */
int          jrt_cmp_any_op(jade_value_t a, jade_value_t b, const char* op);
double       jrt_any_to_double(jade_value_t v);
int          jrt_to_bool(jade_value_t v);

/* ── Shared-runtime dynamic-op core (jade-runtime crate, src/ops.rs) ───────
 * The dispatch above is implemented in Rust as pure `jrt_core_*` ops that
 * cannot raise (a Rust frame can't be crossed by longjmp). Instead they report
 * failure through a uint32_t out-parameter using these codes; the jrt_*_any
 * wrappers in common.c translate a code into a catchable Jade exception. */
#define JRT_OP_OK       ((uint32_t)0)
#define JRT_OP_DIVZERO  ((uint32_t)1)  /* division by zero */
#define JRT_OP_TYPE     ((uint32_t)2)  /* non-numeric / non-comparable operand */
#define JRT_OP_OVERFLOW ((uint32_t)3)  /* int + - * overflowed */
#define JRT_OP_REMZERO  ((uint32_t)4)  /* modulo by zero */

jade_value_t jrt_core_add(jade_value_t a, jade_value_t b, uint32_t* err);
jade_value_t jrt_core_sub(jade_value_t a, jade_value_t b, uint32_t* err);
jade_value_t jrt_core_mul(jade_value_t a, jade_value_t b, uint32_t* err);
jade_value_t jrt_core_div(jade_value_t a, jade_value_t b, uint32_t* err);
jade_value_t jrt_core_mod(jade_value_t a, jade_value_t b, uint32_t* err);
jade_value_t jrt_core_pow(jade_value_t a, jade_value_t b, uint32_t* err);
jade_value_t jrt_core_neg(jade_value_t a, uint32_t* err);
int          jrt_core_cmp(jade_value_t a, jade_value_t b, uint32_t* err);
int          jrt_core_eq(jade_value_t a, jade_value_t b, uint32_t* err);
/* jrt_core_eq_total — equality for *membership*, which never raises: operands of
 * different kinds are not equal. jrt_core_eq is the `==` operator and is strict
 * across kinds by design; `arr.contains(x)` needs to walk past elements of other
 * kinds rather than raise on them. */
int          jrt_core_eq_total(jade_value_t a, jade_value_t b);
/* jrt_core_type_name — a value's type name ("int", "str", "array", …), spelled
 * exactly as the VM's value_type_name spells it, so an error built here reads
 * like the interpreter's. Static storage: do not free. */
const char*  jrt_core_type_name(jade_value_t v);
double       jrt_core_to_double(jade_value_t v, uint32_t* err);

/* jrt_abi_version — the value ABI this runtime speaks. Compared at load against a
 * native package's own version (jade_pkg_abi_version, or jrt_abi_version re-exported
 * by one that links the runtime), so an incompatible package is refused by name
 * instead of failing somewhere inside a call. */
uint32_t     jrt_abi_version(void);

/* ── Tagged string ABI ────────────────────────────────────────────────
 *
 * Every JadeLang-visible string carries a trust tag in the byte at offset -1
 * relative to its data pointer. The data pointer is **8-byte aligned** (an
 * 8-byte header precedes it) so its low 3 bits are free for the value tag (see
 * "Tagged value ABI" above — a string is a JRT_TAG_PTR value). Layout:
 *
 *   [7 pad bytes][trust:1 byte][data:N bytes][NUL:1 byte]
 *                              ^
 *                              returned pointer (8-aligned); trust at data[-1]
 *
 * jrt_str_new returns malloc+8; jrt_str_free frees data-8. Codegen literals use
 * the same 8-byte header on an 8-aligned global.
 *
 * 0 = TRUSTED  (literal in source, derived purely from trusted data)
 * 1 = TAINTED  (originated from an LLM, network, file, shell, or stdin)
 *
 * The runtime refuses tainted strings at code-execution sinks (sh.exec,
 * http.get, fs.read). Codegen emits literals as pre-tagged globals;
 * propagation through `+`, f-strings, str.trim/replace/split, json.parse
 * etc. preserves the maximum of input trust bytes. */

#define JRT_TRUSTED ((uint8_t)0)
#define JRT_TAINTED ((uint8_t)1)

/* Allocate `len` bytes of data with `trust` tag. Returns the data pointer
 * (one byte past the header). NUL-terminator already written at data[len].
 * Aborts on allocation failure. */
char*   jrt_str_new(size_t len, uint8_t trust);

/* Read the trust byte of a tagged string. Returns TRUSTED for NULL. */
uint8_t jrt_trust_of(const char* s);

/* Allocate a tagged copy of a NUL-terminated string. */
char*   jrt_str_dup(const char* s, uint8_t trust);

/* Free a string previously returned by jrt_str_new / jrt_str_dup.
 * Safe on NULL. (Not currently used by codegen — JadeLang has no explicit
 * free — but provided for runtime internals that intentionally release a
 * tagged buffer.) */
void    jrt_str_free(char* s);

/* Compare two tagged jade values for equality (both statically Unknown at
 * codegen time). Dispatches on the low-bit tag: identical bits → equal; two
 * STRINGS (JRT_TAG_STR) → byte compare; numeric cross-kind (int/float/bool) →
 * value compare. Two non-string heap objects (dicts/arrays) only compare equal
 * by identity — the distinct string tag means we never strcmp a non-string, so
 * there is no OOB. Returns 1 on equal, 0 otherwise. */
int     jrt_eq_any(uint64_t a, uint64_t b);

/* Format a tagged Jade value into buf (snprintf semantics), dispatching on the
 * low-bit tag to match the VM's value_to_display: INT → %lld(decoded), STRING →
 * %s, boxed FLOAT → jrt_snprintf_float, BOOL → true/false, NIL → nil. A
 * non-string heap object (printed without static type info) renders "<object>".
 * Used by print / f-string interpolation when the static type is Unknown. */
int     jrt_snprintf_any(char* buf, size_t cap, int64_t val);

/* Print a tagged Jade value to stdout followed by `suffix`, dispatching on the
 * low-bit tag like jrt_snprintf_any — but strings are written unbounded (no
 * scratch buffer), so a long Unknown-typed string isn't truncated. Used by
 * print() for statically-Unknown args. */
void    jrt_print_any(int64_t val, const char* suffix);
/* jrt_write_any — `write(x)`: print with no newline, then flush. The flush
 * matches the VM; unflushed no-newline output sits in a line-buffered stdout. */
void    jrt_write_any(int64_t val);

/* Format a Jade float into buf (snprintf semantics) the way the VM displays
 * it: the shortest decimal that round-trips to the same double, with a
 * trailing ".0" on integer-valued floats (so 4.0 prints as "4.0", not "4").
 * Used by print() and f-string interpolation for statically-Float values. */
int     jrt_snprintf_float(char* buf, size_t cap, double val);

/* Integer exponentiation: base**exp by squaring, matching the VM's Int result
 * for math.pow(int, int) with a non-negative exponent. Negative exponents
 * (which the VM returns as a float) yield 0 here — the AOT result type is fixed
 * at codegen time, so a fractional result can't be represented as Int. */
int64_t jrt_ipow(int64_t base, int64_t exp);

/* ── Platform hooks (provided by the platform backend, e.g. posix.c) ───────
 * The shared core (common.c) is platform-agnostic except for two things the
 * backend owns: process exit and concurrency. jade_rt_exit terminates the
 * process with `code` (host: exit(); other targets: a platform exit primitive).
 * jade_rt_fatal
 * (defined in common.c) prints a message and calls it. */
#if defined(__GNUC__) || defined(__clang__)
__attribute__((noreturn))
#endif
void jade_rt_exit(int code);
#if defined(__GNUC__) || defined(__clang__)
__attribute__((noreturn))
#endif
void jade_rt_fatal(const char* msg);

/* Opaque future handle returned by jade_spawn. */
typedef struct jade_future* jade_future_t;

/* Function-pointer type for async task bodies. */
typedef jade_value_t (*jade_task_fn)(jade_value_t* args, int n_args);

/*
 * jade_spawn — start a task immediately and return a handle.
 * `args` and `n_args` describe the argument array.  The runtime copies
 * `args` before jade_spawn returns, so the caller's stack array is safe
 * to discard.
 */
jade_future_t jade_spawn(jade_task_fn fn_ptr, jade_value_t* args, int n_args);

/*
 * jade_await — block until the task associated with `future` completes
 * and return its result.  Calling jade_await twice on the same future is
 * undefined behaviour.
 */
jade_value_t jade_await(jade_future_t future);

/*
 * jade_join — await all `n` futures in `futures` and write results to
 * `results_out` in the same order.  All tasks must already be running
 * (spawned earlier) when jade_join is called.
 */
void jade_join(jade_future_t* futures, int n, jade_value_t* results_out);

/*
 * jade_future_free — release resources held by `future`.  Must only be
 * called after jade_await (or jade_join) has returned for this future.
 */
void jade_future_free(jade_future_t future);

/* ── Exceptions ───────────────────────────────────────────────────────── */
/* Registers jmpbuf (alloca'd by the LLVM-compiled caller) as the current
 * exception frame. The caller calls setjmp on the same buffer immediately
 * after and branches: 0 → try body, nonzero → catch body.      */
void    jade_exc_push_frame(void* jmpbuf);
void    jade_exc_pop(void);
/* jade_exc_depth / jade_exc_restore — scope the handler stack to a call frame.
 * Codegen snapshots the depth in each function's prologue and restores it on
 * every return, so a `try` exited by `return` (which skips the emitter's
 * PopHandler) cannot leave a frame pointing at a dead jmp_buf. Restore only
 * ever unwinds; it never raises the depth. */
int32_t jade_exc_depth(void);
void    jade_exc_restore(int32_t depth);
void    jade_exc_throw(int64_t value);   /* longjmps to top frame or exits */
void    jade_exc_throw_typed(int64_t value, const char* type); /* type = struct name or NULL */
/* jrt_throw_io — raise an I/O failure the way the VM does: a `RuntimeError`
 * struct whose `message` is "I/O error: <detail>". The fs/http/uhttp/sh
 * forwarders use it, since their Rust halves record a pending error instead of
 * throwing. Takes ownership of nothing; the caller frees `detail`. */
void    jrt_throw_io(const char* detail);
/* jrt_throw_runtime — raise codegen's own failures (zero divisor, overflow) as
 * the same `RuntimeError` struct. A user's `raise x` does NOT come through here:
 * that throws x itself, which is what the VM does too. */
void    jrt_throw_runtime(const char* msg);
int64_t jade_exc_value(void);
const char* jade_exc_type(void);         /* thrown struct type name, or NULL */

/* ── LLM Inference ────────────────────────────────────────────────────── */
/* Opens the inference daemon socket, sends a stateless inference request,
 * reads TOKEN frames until DONE, returns heap-allocated NUL-terminated
 * response string.  Caller must free.  Returns NULL on error.   */
char*   jrt_prompt(const char* prompt, const char* model);

/* Like jrt_prompt but retries (up to max_retries times) using a folded
 * Single-shot: grammar-constrained sampling already shapes the reply.
 * type_name: "int" | "float" | "bool" | "str"
 * Returns heap-allocated string parseable as type_name, or NULL if the
 * reply doesn't parse.  Caller must free.                       */
char*   jrt_prompt_typed(const char* prompt, const char* model,
                         const char* type_name);

/* Like jrt_prompt but constrains sampling with a GBNF grammar string.
 * Returns heap-allocated response string.  Caller must free.   */
char*   jrt_prompt_grammar(const char* prompt, const char* model,
                           const char* grammar);

/* Like jrt_prompt_grammar but with anchor and stop-anchor tokens.     */
char*   jrt_prompt_grammar_ex(const char* prompt, const char* model,
                              const char* pattern,
                              const char* anchor_or_null,
                              const char* stop_or_null);

/* Stream tokens to stdout as they arrive, with prefix-aware muting.
 * - `pattern`/`anchor`/`stop` may be NULL for unconstrained inference.
 * - `start_muted` non-zero suppresses output from the first token (no anchor).
 * - `anchor` strings enter muted mode when matched (anchor itself suppressed).
 * - `stop` strings exit muted mode when matched (stop itself suppressed).
 * Returns the full collected text (muted+visible). Caller frees. NULL on error. */
/* Struct-typed prompt deref (`?p |> City`): ask, coerce, raise on failure.
 * jrt_struct_field builds the type -> field table it coerces against (emitted
 * once at startup, in declaration order); jrt_coerce_struct is the non-raising
 * builder in the shared Rust runtime. Single-shot, like the VM. */
void    jrt_struct_field(const char* type_name, const char* field,
                         int64_t default_word, int has_default);
int64_t jrt_coerce_struct(const char* json, const char* type_name);
int64_t jrt_prompt_struct(const char* prompt, const char* model,
                          const char* type_name);

/* jrt_prompt_typed_checked — jrt_prompt_typed, but raises a catchable Jade
 * error instead of returning NULL when the reply doesn't coerce. Codegen calls
 * this; tagging a NULL as a string crashed the program. */
char*   jrt_prompt_typed_checked(const char* prompt, const char* model,
                                 const char* type_name);
char*   jrt_prompt_stream_ex(const char* prompt, const char* model,
                              const char* pattern_or_null,
                              const char* anchor_or_null,
                              const char* stop_or_null,
                              int start_muted);

/* Grammar objects (jade-runtime, src/grammarf.rs). Grammar.new(pattern[,anchor
 * [,stop]]) -> a Grammar object (ObjKind::Grammar; NULL optional args => None).
 * jrt_prompt_grammar_obj reads it, converts the pattern to GBNF (as the VM does),
 * and calls jrt_prompt_grammar_ex above — the one Rust->C runtime back-reference. */
void*   jrt_grammar_new(const char* pattern, const char* anchor, const char* stop);
char*   jrt_prompt_grammar_obj(const char* prompt, const char* model, const void* grammar_obj);

/* The whole `use llm` package left the language — health, model, keep_anchors,
 * token counting, tool-call parsing, and model profiles all moved to the daemon
 * (shipped as Jade packages there). Prompts (`?p`, `?p |> Type`) remain the only
 * inference surface, so there are no jrt_llm_* entry points here anymore.      */

/* ── String methods ───────────────────────────────────────────────── */
/* jrt_str_contains — 1 if needle found in haystack, else 0.         */
int32_t jrt_str_contains(const char* haystack, const char* needle);
/* jrt_str_trim — heap copy with leading/trailing whitespace stripped. */
char*   jrt_str_trim(const char* str);
/* jrt_str_replace — replace all occurrences of from with to. */
char*   jrt_str_replace(const char* str, const char* from, const char* to);
/* jrt_str_upper / jrt_str_lower — ASCII case conversion (preserves trust).  */
char*   jrt_str_upper(const char* str);
char*   jrt_str_lower(const char* str);
/* jrt_str_starts_with / jrt_str_ends_with — 1 if prefix/suffix matches, else 0. */
int32_t jrt_str_starts_with(const char* str, const char* prefix);
int32_t jrt_str_ends_with(const char* str, const char* suffix);
/* jrt_str_of_any — render a type-erased value to a freshly-allocated tagged
 * string, returning its data pointer (past the 8-byte header). A string value
 * is returned as-is (trust byte preserved); scalars format via jrt_snprintf_any
 * as TRUSTED. Used by the Chunk backend's f-string builder (BuildFStr). */
char* jrt_str_of_any(int64_t val);
/* jrt_int_any/jrt_float_any/jrt_bool_any — the dynamic int()/float()/bool()
 * conversion builtins, dispatched on the runtime tag (mirror the VM's coerce).
 * int()/float() raise a catchable error on a non-numeric string; bool() never
 * raises. All take and return tagged value words. */
int64_t jrt_int_any(int64_t val);
int64_t jrt_char_any(int64_t val);

/* ── bytes ────────────────────────────────────────────────────────────────
 * A binary blob (ObjKind::Bytes). The Rust side owns the representation
 * (jade-runtime, src/bytesf.rs); these are the entry points codegen calls.
 * `decode` can fail on invalid UTF-8, so it reports through the pending-error
 * channel and jk_bytes_decode below turns that into a catchable raise — a Jade
 * raise is a longjmp and must not unwind through a Rust frame. */
void*    jrt_bytes_new(const unsigned char* src, size_t len, unsigned char trust);
int64_t  jrt_bytes_len(const void* p);
int64_t  jrt_bytes_get(const void* p, int64_t i);
const unsigned char* jrt_bytes_data(const void* p);
unsigned char jrt_bytes_trust(const void* p);
void*    jrt_bytes_encode(const unsigned char* s);
char*    jrt_bytes_decode(const void* p);
void*    jrt_bytes_slice(const void* p, int64_t s, int64_t e);
char*    jrt_bytes_take_error(void);
/* Raising wrappers used by codegen. */
int64_t  jk_bytes_decode(int64_t recv);
/* fs byte I/O. Mirrors jrt_fs_read/write/append but over blobs: `read` goes
 * through read_to_string and so cannot read a PNG at all. The content is
 * TAINTED for the same reason fs.read's is — it comes from outside. */
int64_t  jk_fs_read_bytes(const char* path, int32_t trust);
void     jk_fs_write_bytes(const char* path, int64_t blob);
void     jk_fs_append_bytes(const char* path, int64_t blob);
int64_t  jk_fs_read_stdin_bytes(void);
void     jk_fs_write_stdout_bytes(int64_t blob);
int64_t jrt_float_any(int64_t val);
int64_t jrt_bool_any(int64_t val);

/* ── Kind-tagged heap objects (Chunk backend collections) ──────────────────
 * Collections carry a runtime kind tag so the backend can recover their
 * array/dict/struct kind at runtime. The storage lives in the shared Rust
 * runtime crate (jade-runtime, src/coll.rs) behind an ObjHeader; these
 * `jrt_*`/`jrt_coll_*` symbols resolve against its staticlib.
 * JK_ARRAY/JK_DICT/JK_STRUCT equal the Rust ObjKind discriminants (heap.rs:
 * Array=2, Dict=3, Struct=4) — jrt_kind_of returns that byte. Objects carry
 * `len` at ObjHeader offset 4; the Chunk path reads a collection's length via
 * jrt_coll_len. */
#define JK_ARRAY  2
#define JK_DICT   3
#define JK_STRUCT 4
#define JK_BYTES  10
#define JK_PROMPT 7

/* jrt_require_kind — the receiver guard the Chunk backend emits ahead of a
 * primitive method call (`recv.push(x)`, `recv.keys()`, `recv.upper()`, …).
 * `want` is a bitmask of the kinds that method accepts; a receiver outside it
 * RAISES the VM's "struct '<kind>' has no field '<method>'" rather than being
 * untagged and dereferenced as a kind it is not. Returns normally on a match.
 * A method name never proves its receiver's kind — see common.c for why the
 * frontend cannot settle this and the VM checks at runtime too. */
#define JRT_WANT_STR   0x1
#define JRT_WANT_ARRAY 0x2
#define JRT_WANT_DICT  0x4
void    jrt_require_kind(int64_t recv, int32_t want, const char* method);
/* jrt_require_str_arg — the same guard for a str method's *argument*, which is
 * untagged to a char* just like the receiver. Raises the VM's argument wording
 * ("type error: str.<method>") rather than the has-no-field wording. */
void    jrt_require_str_arg(int64_t val, const char* method);

/* ── Prompt values ────────────────────────────────────────────────────────
 *
 * A prompt is its own kind, not the bare string it wraps: `?p` dereferences it,
 * it type-names as "prompt", and it prints as <prompt> — all of which the VM
 * already did. Codegen boxes with jrt_prompt_new at MakePrompt and unwraps with
 * jrt_prompt_text wherever the text itself is needed (the inference entry points
 * below still take a plain char*). */
/* jrt_prompt_new — box a tagged string word as a prompt. Returns the raw pointer
 * (codegen tags it); the prompt takes a reference to the text. */
void*   jrt_prompt_new(int64_t text);
/* jrt_prompt_text — the tagged string word inside a prompt, borrowed. A value
 * that is not a prompt is returned unchanged, so the unwrap is harmless. */
int64_t jrt_prompt_text(int64_t v);
/* jrt_kind_of — ObjKind byte of a kind-tagged object pointer (from its header). */
int64_t jrt_kind_of(void* p);
/* jrt_coll_len — element/field count from the object header (O(1)); the Chunk
 * backend's len() on a collection. */
int64_t jrt_coll_len(void* p);
/* jrt_len_chunk — len() for the Chunk backend's Unknown arm: strlen for a STRING
 * word, ObjHeader.len (offset 4) for a kind-tagged collection, else 0. */
int64_t jrt_len_chunk(int64_t word);

/* Heap accounting (jade-runtime, src/gc.rs) — the cycle collector's instrument.
 * Every collection allocation bumps a global live-object count; the destructor
 * (a later brick) decrements it. difftest cannot observe a leak or a premature
 * free, so this count is how those bugs are made visible.
 * jrt_heap_live_count — allocations minus frees (a leak == positive at exit).
 * jrt_heap_report — codegen emits a call just before main returns; prints the
 * live count to stderr iff JADE_HEAP_REPORT is set (else a no-op getenv). */
int64_t jrt_heap_live_count(void);
void    jrt_heap_report(void);

/* Reference counting (jade-runtime, src/gc.rs). Emitted by codegen ONLY for a
 * "collections-only" program (no async/prompt/native — every TAG_PTR word is then
 * a header-carrying collection or fn-box, so these dispatch on ObjKind and never
 * touch a header-less allocation).
 * jrt_rc_enable — turn on the runtime builders' element retention (called once at
 *   jade_toplevel entry).
 * jrt_incref/jrt_decref — retain / release a collection word (kind-gated no-op on
 *   fn-boxes and non-pointers); decref frees + cascades at zero.
 * jrt_rc_replace(old,new) — release a slot's old reference before overwrite,
 *   skipped when old==new (in-place array mutation). */
void jrt_rc_enable(void);
void jrt_incref(int64_t word);
void jrt_decref(int64_t word);
void jrt_rc_replace(int64_t old, int64_t neww);
/* jrt_coll_* — raw storage helpers for the C forwarders below (never raise; the
 * forwarder owns the bounds/type checks + throw). array_get/set are unchecked
 * (caller bounds-checks via array_len); dict/struct _get write *out and return
 * found?; dict_copy is the value-semantic clone (fresh header). */
int64_t jrt_coll_array_len(void* p);
int64_t jrt_coll_array_get(void* p, int64_t i);
void    jrt_coll_array_set(void* p, int64_t i, int64_t val);
int32_t jrt_coll_dict_get(void* p, const char* key, int64_t* out);
void*   jrt_coll_dict_copy(void* p);
/* jrt_coll_dict_keys — new ObjHeader array of the dict's keys as TRUSTED tagged
 * strings, sorted ascending (matches the VM's dict.keys). */
void*   jrt_coll_dict_keys(void* p);
int32_t jrt_coll_struct_get(void* p, const char* field, int64_t* out);
/* jrt_coll_struct_keys — the struct's field names in DECLARATION order as a
 * kind-tagged string array (dict keys come back sorted; struct fields do not,
 * so a package sees them the way the shared definition writes them). The FFI
 * marshaller walks this to copy a struct across the boundary. */
void*   jrt_coll_struct_keys(void* p);
/* jrt_karr_new/push — build a kind-tagged array (elements are tagged words). */
void*   jrt_karr_new(void);
void    jrt_karr_push(void* arr, int64_t val);

/* ── Generator buffers ────────────────────────────────────────────────────
 * A `yield`ing function fills a buffer and returns it; a stream *is* that
 * buffer, which is why reading one twice gives the same values twice. The
 * buffer is an ordinary kind-tagged array, so len/index/for/print over a
 * stream need no new code at all.
 *
 * A stack, not a slot: a generator may call another generator, and each
 * `yield` has to land in its own function's buffer. Mirrors `VmState::
 * yield_stack` in the interpreter. */
void    jrt_yield_push(void);           /* begin a generator frame */
void    jrt_yield_append(int64_t val);  /* one `yield` */
int64_t jrt_yield_pop(void);            /* end it; returns the tagged array */
/* jrt_kdict_new/set — build a kind-tagged dict (string keys, tagged values).
 * jrt_kdict_set takes the key as a tagged-string word (copied). */
void*   jrt_kdict_new(void);
void    jrt_kdict_set(void* dict, int64_t key_word, int64_t val);
/* jrt_kstruct_new/set — build a kind-tagged struct with a type name and named
 * fields (field names are compile-time C strings). Reference semantics. */
void*   jrt_kstruct_new(const char* type_name);
void    jrt_kstruct_set(void* s, const char* field, int64_t val);
/* jrt_bind_method_new — bind an extend method to a receiver, yielding a
 * callable value laid out {fn_ptr@0, ObjKind::BoundMethod@8, self@16}. NULL when
 * the receiver's type has no such method (the caller raises). */
void*   jrt_bind_method_new(int64_t recv_word, const char* method);
/* jrt_get_field/jrt_set_field — struct data-field access (field is a compile-time
 * C string); raise on a missing field or a non-struct. (Method dispatch — a
 * field that is actually an extend/primitive method — is not handled here; the
 * Chunk backend declines method-call programs.) */
int64_t jrt_get_field(int64_t obj, const char* field);
void    jrt_set_field(int64_t obj, const char* field, int64_t val);
/* jrt_get_type_name — the struct type name of `obj` as a fresh tagged string, or
 * the empty string if `obj` is not a struct (VM `GetTypeName` for typed catch). */
char*   jrt_get_type_name(int64_t obj);
/* jrt_val_index — GetIndex: string char-index, array element, or dict lookup,
 * dispatched on the object's tag/kind; raises on out-of-range / missing key. */
int64_t jrt_val_index(int64_t obj, int64_t idx);
/* jrt_val_set_index — SetIndex: returns the (possibly new) container word. An
 * array is mutated in place (same word); a dict is COPIED then updated (VM value
 * semantics) and the new word returned. Raises on out-of-range / wrong type. */
int64_t jrt_val_set_index(int64_t obj, int64_t idx, int64_t val);
/* jrt_render_any — value_to_display of any value into a fresh malloc'd C string
 * (plain, caller frees): arrays render `[a, b]`, recursively. */
char*   jrt_render_any(int64_t val);
/* jrt_in_any — the `in` operator: substring (str haystack), element membership
 * (array, by value equality), or key membership (dict). Returns 0/1; raises on a
 * wrong needle/haystack type. */
int32_t jrt_in_any(int64_t needle, int64_t haystack);
/* Chunk-backend collection-producing stdlib (ObjHeader outputs). */
void*   jrt_coll_sh_output(const char* cmd);          /* -> {stdout,stderr,code} dict */
void*   jrt_coll_fs_list_dir(const char* path, int32_t* err); /* -> array | null+err */
int64_t jrt_fs_list_dir_chunk(const char* path);      /* raises on err; tagged ptr word */
int64_t jrt_random_choice_chunk(int64_t arr_word);    /* random element word */
void    jrt_random_shuffle_chunk(int64_t arr_word);   /* Fisher-Yates in place */
int64_t jrt_coll_array_map(int64_t arr_word, int64_t fn_word);    /* -> new array */
int64_t jrt_coll_array_filter(int64_t arr_word, int64_t fn_word); /* -> new array */

/* ── JSON ─────────────────────────────────────────────────────────── */
/* jrt_json_parse_chunk — parse a (tagged) JSON string into an ObjHeader value
 * word (dict/array/scalar), or JRT_NIL on invalid JSON. Chunk-path native. */
jade_value_t jrt_json_parse_chunk(const char* s);
/* jrt_json_stringify_chunk — render an ObjHeader value word to a fresh TRUSTED
 * tagged string (compact, or 2-space pretty when pretty != 0).               */
char*   jrt_json_stringify_chunk(jade_value_t word, int pretty);

/* ── Conversions ──────────────────────────────────────────────────── */
/* jrt_bool_of_str — parse a string to a bool, matching the VM's bool():
 * case-insensitive "false" or "" → 0, any other string → 1. */
int32_t jrt_bool_of_str(const char* s);

/* ── Internal: shared taint gate ──────────────────────────────────── */
/* Refuse a tainted string at a code-execution / IO sink (process exit on
 * violation). Lives in the always-linked core; used by the fs/ and sh/ modules. */
void    jrt_refuse_if_tainted(const char* arg, const char* sink_name);

/* ── Stdlib leaf modules (one folder per std:: module) ────────────────
 * Each module's ABI is declared in its own header and implemented in its own
 * folder (built by build.rs). They depend only on the core symbols declared
 * above. The data-structure modules (dict/array/json) stay in common.c with the
 * heap object model they manipulate. */

/* http (std::http) is implemented in Rust now (jade-runtime, src/httpf.rs), over
 * the curl binary. Each verb returns an ObjHeader dict { status, body:TAINTED }.
 * Only transport failure raises: the impls record a pending error, the C
 * forwarders (common.c) throw it. */
int64_t      jrt_http_get_impl(const char* url, void* headers);
int64_t      jrt_http_post_impl(const char* url, const char* body, void* headers);
int64_t      jrt_http_put_impl(const char* url, const char* body, void* headers);
int64_t      jrt_http_delete_impl(const char* url, void* headers);
int64_t      jrt_http_head_impl(const char* url, void* headers);
char*        jrt_http_take_error(void);
jade_value_t jrt_http_get(const char* url, void* headers);
jade_value_t jrt_http_post(const char* url, const char* body, void* headers);
jade_value_t jrt_http_put(const char* url, const char* body, void* headers);
jade_value_t jrt_http_delete(const char* url, void* headers);
jade_value_t jrt_http_head(const char* url, void* headers);

/* uhttp (std::uhttp) — HTTP/1.1 over a Unix domain socket (jade-runtime,
 * src/uhttpf.rs). Same shape as http: each verb returns an ObjHeader dict
 * { status, body:TAINTED }; only transport failure raises (impls record a
 * pending error, the C forwarders below throw it). The url is a pseudo-URL
 * `unix://<socket-path>:<request-path>`. */
int64_t      jrt_uhttp_get_impl(const char* url, void* headers);
int64_t      jrt_uhttp_post_impl(const char* url, const char* body, void* headers);
int64_t      jrt_uhttp_put_impl(const char* url, const char* body, void* headers);
int64_t      jrt_uhttp_delete_impl(const char* url, void* headers);
int64_t      jrt_uhttp_head_impl(const char* url, void* headers);
char*        jrt_uhttp_take_error(void);
jade_value_t jrt_uhttp_get(const char* url, void* headers);
jade_value_t jrt_uhttp_post(const char* url, const char* body, void* headers);
jade_value_t jrt_uhttp_put(const char* url, const char* body, void* headers);
jade_value_t jrt_uhttp_delete(const char* url, void* headers);
jade_value_t jrt_uhttp_head(const char* url, void* headers);

/* uhttp.stream — a streaming read over a Unix socket, one Jade handler call per
 * body line. The handle API is Rust (jade-runtime, src/uhttpf.rs `Stream`); the
 * driver loop that calls the handler is jrt_uhttp_stream in common.c, because
 * the call goes back into Jade and a raising handler must not longjmp through a
 * Rust frame. `_next` returns 1 on a line (writing a tagged TAINTED string word
 * to *out), 0 at end of stream, -1 on failure with a pending error set. */
void*   jrt_uhttp_stream_open(const char* url, void* headers);
int64_t jrt_uhttp_stream_status(void* h);
int32_t jrt_uhttp_stream_next(void* h, int64_t* out);
void    jrt_uhttp_stream_close(void* h);
int64_t jrt_uhttp_stream(const char* url, int64_t fn_word, void* headers);

/* sh (std::sh) is implemented in Rust now (jade-runtime, src/shf.rs). exec/run
 * refuse tainted input (a code-execution sink) — that check + throw stays in the
 * C forwarders (common.c); the impls never raise. */
char*   jrt_sh_exec_impl(const char* cmd);
int64_t jrt_sh_run_impl(const char* cmd);
char*   jrt_sh_take_error(void);        /* drain pending error, or NULL */
char*   jrt_sh_exec(const char* cmd);   /* forwarder: refuse-if-tainted + impl + throw */
int64_t jrt_sh_run(const char* cmd);    /* forwarder: refuse-if-tainted + impl + throw */

/* fs (std::fs) is implemented in Rust now (jade-runtime, src/fsf.rs). The
 * fallible ops record a pending error instead of throwing (a longjmp must not
 * cross a Rust frame); the C forwarders below (common.c) throw it. exists never
 * fails and exports from Rust directly. */
int32_t jrt_fs_exists(const char* path);
char*   jrt_fs_take_error(void);            /* drain pending error, or NULL */
char*   jrt_fs_read_impl(const char* path, int32_t trust);
void*   jrt_fs_read_bytes_impl(const char* path, int32_t trust);
void    jrt_fs_write_bytes_impl(const char* path, const unsigned char* data, size_t len);
void    jrt_fs_append_bytes_impl(const char* path, const unsigned char* data, size_t len);
void*   jrt_fs_read_stdin_bytes_impl(void);
void    jrt_fs_write_stdout_bytes_impl(const unsigned char* data, size_t len);
void    jrt_fs_write_impl(const char* path, const char* content);
void    jrt_fs_append_impl(const char* path, const char* content);
void    jrt_fs_delete_impl(const char* path);
void    jrt_fs_mkdir_impl(const char* path);
/* Codegen-facing raising forwarders (common.c). */
char*   jrt_fs_read(const char* path, int32_t trust);
void    jrt_fs_write(const char* path, const char* content);
void    jrt_fs_append(const char* path, const char* content);
void    jrt_fs_delete(const char* path);
void    jrt_fs_mkdir(const char* path);

/* time (std::time) is implemented in Rust now (jade-runtime, src/timef.rs) —
 * no C leaf. Declared here for ABI reference. None raise. */
int64_t jrt_time_now(void);
int64_t jrt_time_now_ms(void);
void    jrt_time_sleep(double secs);
char*   jrt_time_local(const char* tz);

/* random (std::random) is implemented in Rust now (jade-runtime, src/randomf.rs).
 * jrt_random_int is a C forwarder (common.c) that throws on min>max, then calls
 * the non-raising Rust draw; float/seed export directly from Rust. */
int64_t jrt_random_int(int64_t lo, int64_t hi);
int64_t jrt_random_draw(int64_t lo, int64_t hi);
double  jrt_random_float(void);
void    jrt_random_seed(int64_t n);

/* env (std::env) and path (std::path) are implemented in Rust now
 * (jade-runtime, src/envf.rs + src/pathf.rs) — no C leaf. Codegen resolves these
 * jrt_* symbols against the shared staticlib; declared here for ABI reference.
 * None raise (so no C forwarder). */
char*        jrt_env_cwd(void);
char*        jrt_env_get(const char* name);
void         jrt_env_set(const char* name, const char* value);
void         jrt_set_args(int argc, char** argv);
jade_value_t jrt_env_args(void);
char*        jrt_path_basename(const char* p);
char*        jrt_path_ext(const char* p);
char*        jrt_path_join(const char* a, const char* b);
char*        jrt_path_dirname(const char* p);
char*        jrt_path_stem(const char* p);
char*        jrt_path_abs(const char* p);
int32_t      jrt_path_is_abs(const char* p);

/* ── Input ─────────────────────────────────────────────────────────── */
/* jrt_readline — display prompt then read a line from stdin (strips newline). */
char*   jrt_readline(const char* prompt);

/* ── Native (C-ABI) packages ──────────────────────────────────────────
 *
 * AOT counterpart of the VM's libloading FFI (jadelang/src/native.rs). A native
 * package is a shared library exporting `jade_pkg_init`; the AOT binary dlopens
 * it at startup (jrt_native_load) and dispatches calls dynamically
 * (jrt_native_call) — the Jade-name -> function-pointer map is only knowable by
 * calling jade_pkg_init at runtime, so link-time resolution is impossible.
 *
 * The FFI value type (JadeVal) is a 16-byte tagged union that MUST byte-match
 * `JadeVal` in jadelang/src/native.rs so the same .dylib serves both the VM and
 * AOT. Its tags (0..8) are an independent ABI, distinct from the jade_value_t
 * low-bit tags above. Scalars convert directly; arrays, dicts, and structs are
 * deep-copied into nested JadeArr/JadeMap/JadeStruct trees (see below).
 * Remaining heap kinds (functions, futures, prompts) become nil. */
#define JADE_FFI_NIL   0
#define JADE_FFI_INT   1
#define JADE_FFI_FLOAT 2
#define JADE_FFI_BOOL  3
#define JADE_FFI_STR   4   /* null-terminated UTF-8 (non-owning pointer) */
#define JADE_FFI_ERROR 5   /* like STR, but the string is an error message */
#define JADE_FFI_ARRAY 6   /* data.as_arr  -> JadeArr  (deep-copied, owned) */
#define JADE_FFI_DICT  7   /* data.as_dict -> JadeMap  (deep-copied, owned) */
#define JADE_FFI_STRUCT 8  /* data.as_struct -> JadeStruct (deep-copied, owned) */
/* data.as_bytes -> JadeBytes (copied, owned). Counted rather than riding on
 * JADE_FFI_STR, because a blob may contain NUL bytes and need not be valid
 * UTF-8: a char* would truncate one and corrupt the other. Added in v1.2.2,
 * which is why the runtime ABI version moved to 3. */
#define JADE_FFI_BYTES 9

/* Nested container payloads for JADE_FFI_ARRAY / JADE_FFI_DICT.
 *
 * A collection cannot cross the boundary by pointer: the process holds two
 * `jade-runtime` instances (the VM binary and each dlopen'd package), each with
 * its own mimalloc, so a word owned by one runtime must never be freed by the
 * other. Instead a collection is *deep-copied* into a tree whose every node — the
 * JadeArr/JadeMap header, its element arrays, and any strings *inside* a
 * container — is allocated with libc malloc/strdup. libc's allocator is shared
 * process-wide (mimalloc is a Rust #[global_allocator], it does not override the
 * C malloc), so either side can release the whole tree with `jade_ffi_free`.
 * Top-level scalar strings keep the non-owning contract. Cyclic collections are
 * not supported (the copy would not terminate). */
typedef struct JadeArr JadeArr;
typedef struct JadeMap JadeMap;
typedef struct JadeStruct JadeStruct;
typedef struct JadeBytes JadeBytes;

typedef union {
    int64_t     as_int;
    double      as_float;
    uint8_t     as_bool;
    const char* as_str;
    uint64_t    as_nil;
    JadeArr*    as_arr;
    JadeMap*    as_dict;
    JadeStruct* as_struct;
    JadeBytes*  as_bytes;
} JadeValData;

typedef struct {
    uint8_t     tag;
    uint8_t     _pad[7];
    JadeValData data;
} JadeVal;

struct JadeArr { JadeVal* items; size_t len; };
struct JadeBytes { unsigned char* data; size_t len; };
struct JadeMap { const char** keys; JadeVal* vals; size_t len; };

/* A JadeMap plus the struct's type name, fields in declaration order. The name
 * is what makes a typed contract enforceable across the boundary: a receiver can
 * refuse a struct that is not the type it expects, where a bare dict with the
 * wrong keys reads as a set of nils and fails silently. Same ownership rules as
 * JadeMap — every node libc-owned, released by jade_ffi_free. */
struct JadeStruct {
    const char*  type_name;
    const char** keys;
    JadeVal*     vals;
    size_t       len;
};

/* Release a JadeVal tree built by the marshaller (`to_ffi`/`jrt_ffi_from_tagged`
 * / the VM's vm_to_ffi). Frees only the libc-owned parts — JadeArr/JadeMap/
 * JadeStruct nodes,
 * their element arrays, and copied strings inside a container — so it is a no-op
 * on scalars and safe to call on any JadeVal. The consumer of a native call frees
 * both the argument trees it built and a container return value with it. */
void jade_ffi_free(JadeVal* v);

typedef int (*JadeNativeFnPtr)(size_t argc, const JadeVal* argv, JadeVal* out);

typedef struct {
    const char*     name;
    JadeNativeFnPtr func;
} JadeBinding;

typedef struct {
    const char*        name;
    const JadeBinding* bindings;
    size_t             binding_count;
} JadeNativePkg;

/* Platform hooks for dynamic loading (posix.c: real dlopen/dlsym; a target
 * without an in-binary loader stubs these, so native packages are host-only). */
void* jade_dlopen(const char* path);
void* jade_dlsym(void* handle, const char* sym);

/* Load a native package: dlopen `path`, call its jade_pkg_init, and build a
 * name -> function-pointer registry. Returns an opaque handle. Raises a
 * catchable Jade error on failure (missing file, missing jade_pkg_init,
 * non-zero init status), mirroring the VM raising on load. */
void* jrt_native_load(const char* path);

/* Resolve `fn_name` in `handle`, marshal `argc` tagged args into JadeVal, invoke
 * the native function, and marshal the result back to a tagged jade_value_t.
 * Native output strings are TAINTED. Raises a catchable Jade error on an unknown
 * function, a non-zero native status, or a JADE_FFI_ERROR result. */
/* Marshal one JadeVal into a tagged value, and back. Used by the wrappers that
 * `jade build --lib` emits around exported Jade functions — the mirror of
 * jrt_native_call's inbound direction. Non-primitives become nil either way. */
jade_value_t jrt_ffi_to_tagged(const JadeVal* v);
void         jrt_ffi_from_tagged(jade_value_t v, JadeVal* out);

jade_value_t jrt_native_call(void* handle, const char* fn_name,
                             const jade_value_t* args, int64_t argc);

/* Nonzero if a loaded package exports `name` — probe for an optional binding
 * (e.g. a provider package's `configure`) without a raising call. */
int jrt_native_has(void* handle, const char* name);

/* Active inference-provider slot (implemented in jade-runtime's `provider`
 * module). `jrt_provider_available` is nonzero when a provider package is
 * installed; the path/config accessors return malloc'd NUL-terminated strings the
 * caller frees (or NULL). See runtime_aot/infer/provider.h. */
int   jrt_provider_available(void);
char* jrt_provider_active_lib_path(void);
char* jrt_provider_active_config(void);
