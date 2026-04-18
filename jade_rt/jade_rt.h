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
