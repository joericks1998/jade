#pragma once
#include <stdint.h>

/* Uniform value representation: all Jade values fit in 64 bits. */
typedef int64_t jade_value_t;

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
void*   jade_dict_create(void);
void    jade_dict_set(void* dict, const char* key, int64_t val);
int64_t jade_dict_get(void* dict, const char* key);
int64_t jade_dict_len(void* dict);
void    jade_dict_free(void* dict);

/* ── Exceptions ───────────────────────────────────────────────────────── */
/* Registers jmpbuf (alloca'd by the LLVM-compiled caller) as the current
 * exception frame. The caller calls setjmp on the same buffer immediately
 * after and branches: 0 → try body, nonzero → catch body.      */
void    jade_exc_push_frame(void* jmpbuf);
void    jade_exc_pop(void);
void    jade_exc_throw(int64_t value);   /* longjmps to top frame or exits */
int64_t jade_exc_value(void);

/* ── LLM Inference ────────────────────────────────────────────────────── */
/* Opens /dev/jade, sends a stateless inference request, reads TOKEN frames
 * until DONE, returns heap-allocated NUL-terminated response string.
 * Caller must free. Returns NULL on error.                      */
char*   jade_infer(const char* prompt, const char* model);

/* Like jade_infer but retries (up to max_retries times) using a folded
 * correction prompt until the response parses as type_name.
 * type_name: "int" | "float" | "bool" | "str"
 * Returns heap-allocated string parseable as type_name, or NULL on
 * exhaustion. Caller must free.                                 */
char*   jade_infer_typed(const char* prompt, const char* model,
                         const char* type_name, int max_retries);
