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
 *                           low4 == 0b0111 ( 7) -> NIL
 *                           low4 == 0b1111 (15) -> BOOL, value in bit4
 *                                                  (false=15, true=31)
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
#define JRT_TAG_IMM    ((uint64_t)7)   /* nil or bool */
#define JRT_NIL        ((jade_value_t)0x07)  /* 0b00111 */
#define JRT_FALSE      ((jade_value_t)0x0F)  /* 0b01111 */
#define JRT_TRUE       ((jade_value_t)0x1F)  /* 0b11111 */

#define jrt_tag(v)        ((uint64_t)(v) & JRT_TAG_MASK)
#define jrt_is_int(v)     (((uint64_t)(v) & 1u) == 0)
#define jrt_is_ptr(v)     (jrt_tag(v) == JRT_TAG_PTR)
#define jrt_is_float(v)   (jrt_tag(v) == JRT_TAG_FLOAT)
#define jrt_is_str(v)     (jrt_tag(v) == JRT_TAG_STR)
#define jrt_is_nil(v)     (((uint64_t)(v) & 0xfu) == 0x7u)  /* low4 == 0b0111 */
#define jrt_is_bool(v)    (((uint64_t)(v) & 0xfu) == 0xfu)  /* low4 == 0b1111 */
/* Any heap pointer kind (non-string ptr, boxed float, or string): all untag
 * with `& ~7`. Used to decide whether a value can be dereferenced as a ptr. */
#define jrt_is_heap(v)    (jrt_is_ptr(v) || jrt_is_float(v) || jrt_is_str(v))

#define jrt_box_int(i)    ((jade_value_t)((uint64_t)(int64_t)(i) << 1))
#define jrt_unbox_int(v)  ((int64_t)(v) >> 1)
#define jrt_box_bool(b)   ((jade_value_t)((((uint64_t)((b)!=0)) << 4) | 0xfu))
#define jrt_unbox_bool(v) ((int)(((uint64_t)(v) >> 4) & 1u))
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
double       jrt_core_to_double(jade_value_t v, uint32_t* err);

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

/* ── Platform hooks (provided by the backend: posix.c / jadeos.c) ──────────
 * The shared core (common.c) is platform-agnostic except for two things the
 * backend owns: process exit and concurrency. jade_rt_exit terminates the
 * process with `code` (host: exit(); Jade OS: jade_task_exit()). jade_rt_fatal
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

/* ── Dict ─────────────────────────────────────────────────────────────── */
void*    jade_dict_create(void);
void     jade_dict_set(void* dict, const char* key, int64_t val);
int64_t  jade_dict_get(void* dict, const char* key);   /* JRT_NIL on miss */
/* Like jade_dict_get but RAISES a catchable runtime error on a missing key,
 * matching the VM's "key 'X' not found in dict". Used for `d[k]` reads. */
int64_t  jade_dict_get_checked(void* dict, const char* key);
int32_t  jade_dict_has(void* dict, const char* key);
int64_t  jade_dict_len(void* dict);
/* jade_dict_copy — shallow copy into a fresh dict (VM dict value semantics). */
void*    jade_dict_copy(void* dict);
void     jade_dict_free(void* dict);

/* ── Exceptions ───────────────────────────────────────────────────────── */
/* Registers jmpbuf (alloca'd by the LLVM-compiled caller) as the current
 * exception frame. The caller calls setjmp on the same buffer immediately
 * after and branches: 0 → try body, nonzero → catch body.      */
void    jade_exc_push_frame(void* jmpbuf);
void    jade_exc_pop(void);
void    jade_exc_throw(int64_t value);   /* longjmps to top frame or exits */
void    jade_exc_throw_typed(int64_t value, const char* type); /* type = struct name or NULL */
int64_t jade_exc_value(void);
const char* jade_exc_type(void);         /* thrown struct type name, or NULL */

/* ── LLM Inference ────────────────────────────────────────────────────── */
/* Opens the inference daemon socket, sends a stateless inference request,
 * reads TOKEN frames until DONE, returns heap-allocated NUL-terminated
 * response string.  Caller must free.  Returns NULL on error.   */
char*   jrt_prompt(const char* prompt, const char* model);

/* Like jrt_prompt but retries (up to max_retries times) using a folded
 * correction prompt until the response parses as type_name.
 * type_name: "int" | "float" | "bool" | "str"
 * Returns heap-allocated string parseable as type_name, or NULL on
 * exhaustion.  Caller must free.                                */
char*   jrt_prompt_typed(const char* prompt, const char* model,
                         const char* type_name, int max_retries);

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
char*   jrt_prompt_stream_ex(const char* prompt, const char* model,
                              const char* pattern_or_null,
                              const char* anchor_or_null,
                              const char* stop_or_null,
                              int start_muted);

/* Resolve the active model name. Reads $JADE_MODEL; returns heap copy or
 * empty string if unset. Caller must free. */
char*   jrt_get_model(void);

/* llm.keep_anchors(b) — sticky session flag: when set, prompt requests ask the
 * daemon to make tool-span boundaries observable in-band (keep_anchors on the
 * wire). Mirrors the VM's VmState.keep_anchors.                              */
void    jrt_llm_keep_anchors(int on);

/* llm.total_tokens() — cumulative session token count via a `stats_only`
 * request (the DONE frame carries the running total). Mirrors the VM.        */
int64_t jrt_total_tokens(void);

/* llm.health() — daemon health snapshot via a `health_only` request whose
 * 0x05 JSON frames are accumulated and parsed. Returns a tagged dict, or
 * JRT_NIL if the daemon sent no parseable JSON.                              */
jade_value_t jrt_llm_health(void);

/* llm.tool_grammar() — the canonical tool-call body GBNF (compiled in from
 * jadelang/grammars/tool_call.gbnf), as a TRUSTED heap string.               */
char*   jrt_tool_grammar(void);

/* llm.find_tool_call(text) — first tool call in `text` per the active model's
 * profile, as a tagged {name, args} dict, or JRT_NIL when none/no profile.   */
jade_value_t jrt_llm_find_tool_call(const char* text);

/* llm.find_tool_calls(text) — every tool call in `text`, in order, as a tagged
 * array of {name, args} dicts (empty array when none/no profile).            */
jade_value_t jrt_llm_find_tool_calls(const char* text);

/* llm.profile() — the active model's profile as a tagged dict
 * { model, tool_call:{open,close,name_field}, spans:[…] }, or JRT_NIL.       */
jade_value_t jrt_llm_profile(void);

/* ── String methods ───────────────────────────────────────────────── */
/* jrt_str_contains — 1 if needle found in haystack, else 0.         */
int32_t jrt_str_contains(const char* haystack, const char* needle);
/* jrt_str_split — split str on delim, returns jade_array* of char*. */
void*   jrt_str_split(const char* str, const char* delim);
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
int64_t jrt_float_any(int64_t val);
int64_t jrt_bool_any(int64_t val);

/* ── Kind-tagged heap objects (Chunk backend collections) ──────────────────
 * The Chunk backend can't recover the static array/dict/struct type the way the
 * legacy path does, so its collections carry a runtime kind tag at offset 0
 * (JK_ARRAY/JK_DICT/JK_STRUCT). These live alongside the legacy headerless
 * objects (a program is wholly one path). `len` is at offset 8, matching
 * JrtArrayHdr, so jrt_len_any/jrt_len_unknown read either. */
#define JK_ARRAY  1
#define JK_DICT   2
#define JK_STRUCT 3
/* jrt_kind_of — kind byte of a kind-tagged object pointer (offset 0). */
int64_t jrt_kind_of(void* p);
/* jrt_karr_new/push — build a kind-tagged array (elements are tagged words). */
void*   jrt_karr_new(void);
void    jrt_karr_push(void* arr, int64_t val);
/* jrt_kdict_new/set — build a kind-tagged dict (string keys, tagged values).
 * jrt_kdict_set takes the key as a tagged-string word (copied). */
void*   jrt_kdict_new(void);
void    jrt_kdict_set(void* dict, int64_t key_word, int64_t val);
/* jrt_kstruct_new/set — build a kind-tagged struct with a type name and named
 * fields (field names are compile-time C strings). Reference semantics. */
void*   jrt_kstruct_new(const char* type_name);
void    jrt_kstruct_set(void* s, const char* field, int64_t val);
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
/* jrt_len_any — len() on an Unknown-typed value, dispatched on the runtime tag. */
int64_t jrt_len_any(jade_value_t v);
/* jrt_len_unknown — len() for codegen's Unknown arm: strlen for STRING-tagged
 * values, array-header .len (offset 8) for every other word — including a raw
 * untagged heap pointer (a stdlib Ret::Ptr result). NULL → 0. */
int64_t jrt_len_unknown(int64_t val);

/* ── Array methods ────────────────────────────────────────────────── */
/* jrt_array_new — allocate an empty jade_array header (cap 0; push grows it).
 * Box with jrt_box_ptr to surface it as a value. Returns NULL on OOM.       */
void*   jrt_array_new(void);
/* jrt_array_push — append val (as i64) to jade_array, grow if needed. */
void    jrt_array_push(void* arr, int64_t val);
/* jrt_array_pop — remove and return last element; returns 0 if empty. */
int64_t jrt_array_pop(void* arr);
/* jrt_array_len — element count (NULL → 0). */
int64_t jrt_array_len(void* arr);
/* jrt_array_get — element i as a tagged value; JRT_NIL if out of range. */
int64_t jrt_array_get(void* arr, int64_t i);
/* jrt_array_set — store tagged val at element i (no-op if out of range). */
void    jrt_array_set(void* arr, int64_t i, int64_t val);
/* jrt_array_reverse — reverse in place (arr.reverse(), returns Nil). */
void    jrt_array_reverse(void* arr);
/* jrt_array_sort — sort in place by the VM value ordering (arr.sort(), Nil). */
void    jrt_array_sort(void* arr);
/* jrt_array_map — new array of fn(elem) (array.map). `fnval` is a jade_fn_t*. */
void*   jrt_array_map(void* arr, void* fnval);
/* jrt_array_filter — elements where fn(elem) is true (array.filter). */
void*   jrt_array_filter(void* arr, void* fnval);

/* ── Dict methods ─────────────────────────────────────────────────── */
/* jrt_dict_keys — keys sorted ascending, as a jade_array* of TRUSTED char*. */
void*   jrt_dict_keys(void* dict);
/* jrt_dict_values — values ordered by key, as a jade_array* (tagged). */
void*   jrt_dict_values(void* dict);
/* jrt_dict_merge — new dict = d1 then d2 (d2 wins on conflict); inputs unchanged. */
void*   jrt_dict_merge(void* d1, void* d2);

/* ── JSON ─────────────────────────────────────────────────────────── */
/* jrt_json_parse — parse a JSON object string into a JadeDict*.       */
void*   jrt_json_parse(const char* json_str);
/* jrt_json_esc_str — return heap `"escaped"` string (with quotes).    */
char*   jrt_json_esc_str(const char* s);
/* jrt_json_arr_dicts — serialize jade_array* of JadeDicts to JSON.    */
char*   jrt_json_arr_dicts(int64_t arr_i64);

/* ── Conversions ──────────────────────────────────────────────────── */
/* jrt_bool_of_str — parse a string to a bool, matching the VM's bool():
 * case-insensitive "false" or "" → 0, any other string → 1. */
int32_t jrt_bool_of_str(const char* s);

/* ── LLM ──────────────────────────────────────────────────────────── */
/* jrt_count_tokens — ask jade-tree to count tokens; returns count.    */
int64_t jrt_count_tokens(const char* str);

/* ── Internal: shared taint gate ──────────────────────────────────── */
/* Refuse a tainted string at a code-execution / IO sink (process exit on
 * violation). Lives in the always-linked core; used by the fs/ and sh/ modules. */
void    jrt_refuse_if_tainted(const char* arg, const char* sink_name);

/* ── Stdlib leaf modules (one folder per std:: module) ────────────────
 * Each module's ABI is declared in its own header and implemented in its own
 * folder (built by build.rs). They depend only on the core symbols declared
 * above. The data-structure modules (dict/array/json) stay in common.c with the
 * heap object model they manipulate. */
#include "fs/fs.h"
#include "sh/sh.h"
#include "env/env.h"
#include "path/path.h"
#include "http/http.h"
#include "time/time.h"
#include "random/random.h"

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
 * AOT. Its tags (0..5) are an independent ABI, distinct from the jade_value_t
 * low-bit tags above. Marshalling supports the same primitives as the VM's
 * vm_to_ffi/ffi_to_vm; non-primitive args become nil. */
#define JADE_FFI_NIL   0
#define JADE_FFI_INT   1
#define JADE_FFI_FLOAT 2
#define JADE_FFI_BOOL  3
#define JADE_FFI_STR   4   /* null-terminated UTF-8 (non-owning pointer) */
#define JADE_FFI_ERROR 5   /* like STR, but the string is an error message */

typedef union {
    int64_t     as_int;
    double      as_float;
    uint8_t     as_bool;
    const char* as_str;
    uint64_t    as_nil;
} JadeValData;

typedef struct {
    uint8_t     tag;
    uint8_t     _pad[7];
    JadeValData data;
} JadeVal;

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

/* Platform hooks for dynamic loading (posix.c: real dlopen/dlsym; jadeos.c:
 * stubs returning NULL, so native packages are host-only). */
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
jade_value_t jrt_native_call(void* handle, const char* fn_name,
                             const jade_value_t* args, int64_t argc);
