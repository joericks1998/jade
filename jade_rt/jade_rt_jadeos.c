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
    free(tc);
    fut->result = result;
    jade_task_exit(0);
}

jade_future_t jade_spawn(jade_task_fn fn, jade_value_t* args, int n_args) {
    struct jade_future* fut = jade_alloc(sizeof(struct jade_future));

    jade_value_t* args_copy = NULL;
    if (n_args > 0) {
        args_copy = jade_alloc(sizeof(jade_value_t) * (size_t)n_args);
        jade_memcpy(args_copy, args, sizeof(jade_value_t) * (size_t)n_args);
    }

    TaskCtx* tc = jade_alloc(sizeof(TaskCtx));
    tc->fn     = fn;
    tc->args   = args_copy;
    tc->n_args = n_args;
    tc->future = fut;

    fut->task_id = jade_task_spawn(task_entry, tc, JADE_TASK_NORMAL);
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
