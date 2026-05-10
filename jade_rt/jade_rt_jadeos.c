/* jade_rt_jadeos.c — Jade OS kernel-backed async runtime.
 *
 * Selected by `make JADEOS=1` when cross-compiling for the Jade OS target.
 * Requires Jade OS kernel headers: <jade/task.h>, <jade/alloc.h>
 *
 * Jade OS exposes native task primitives that map 1-to-1 onto jade_rt's API,
 * making this implementation a thin shim over the kernel syscall layer.
 */

#if !defined(__JADE_OS__)
#  error "jade_rt_jadeos.c must only be compiled for the Jade OS target (pass -D__JADE_OS__)."
#endif

#include "jade_rt.h"
#include <jade/alloc.h>
#include <jade/task.h>

/* JadeOS userspace runs against musl libc — full C standard library is
 * available for dict runtime, exception handling, and /dev/jade I/O. */
#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>
#include <setjmp.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>

/* Fatal abort for Jade OS: kills the current task with exit code -1.
 * Used when a runtime invariant is violated (OOM, failed task spawn).
 * __builtin_unreachable() suppresses false "function may not return" warnings. */
#define JADE_ABORT() do { jade_task_exit(-1); __builtin_unreachable(); } while (0)

struct jade_future {
    jade_task_id_t task_id;
    jade_value_t   result;
};

typedef struct {
    jade_task_fn        fn;
    jade_value_t*       args;
    int                 n_args;
    struct jade_future* future;
} TaskCtx;

static void task_entry(void* vp) {
    TaskCtx* tc = (TaskCtx*)vp;
    jade_value_t result = tc->fn(tc->args, tc->n_args);
    jade_free(tc->args);
    struct jade_future* fut = tc->future;
    jade_free(tc);          /* matched allocation: jade_alloc(sizeof(TaskCtx)) */
    fut->result = result;
    /* Full memory barrier: ensure the result store is visible to jade_task_wait
     * callers before the task's completion is signalled by jade_task_exit. */
    __sync_synchronize();
    jade_task_exit(0);
}

jade_future_t jade_spawn(jade_task_fn fn, jade_value_t* args, int n_args) {
    struct jade_future* fut = jade_alloc(sizeof(struct jade_future));
    if (!fut) JADE_ABORT();

    jade_value_t* args_copy = NULL;
    if (n_args > 0) {
        /* Guard against overflow on 32-bit targets where size_t is 32 bits. */
        if ((size_t)n_args > SIZE_MAX / sizeof(jade_value_t)) JADE_ABORT();
        args_copy = jade_alloc(sizeof(jade_value_t) * (size_t)n_args);
        if (!args_copy) { jade_free(fut); JADE_ABORT(); }
        jade_memcpy(args_copy, args, sizeof(jade_value_t) * (size_t)n_args);
    }

    TaskCtx* tc = jade_alloc(sizeof(TaskCtx));
    if (!tc) { jade_free(args_copy); jade_free(fut); JADE_ABORT(); }
    tc->fn     = fn;
    tc->args   = args_copy;
    tc->n_args = n_args;
    tc->future = fut;

    jade_task_id_t tid = jade_task_spawn(task_entry, tc, JADE_TASK_NORMAL);
    if (tid == JADE_INVALID_TASK) {
        jade_free(tc);
        jade_free(args_copy);
        jade_free(fut);
        JADE_ABORT();
    }
    fut->task_id = tid;
    return fut;
}

jade_value_t jade_await(jade_future_t future) {
    struct jade_future* fut = future;
    jade_task_wait(fut->task_id);
    return fut->result;
}

void jade_join(jade_future_t* futures, int n, jade_value_t* results_out) {
    for (int i = 0; i < n; i++) {
        results_out[i] = jade_await(futures[i]);
    }
}

void jade_future_free(jade_future_t future) {
    jade_free(future);
}
