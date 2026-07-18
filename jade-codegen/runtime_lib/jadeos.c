/* Jade OS backend for the Jade runtime: the concurrency layer over the kernel
 * task scheduler (jade_task_*) plus the process-exit primitive. Everything else
 * lives in the platform-agnostic common.c. This file is compiled by the Jade OS
 * build (not by the host build.rs, which compiles posix.c); it needs the kernel
 * headers <jade/alloc.h> / <jade/task.h>. */
#ifdef __JADE_OS__

#include "runtime.h"
#include <jade/alloc.h>
#include <jade/task.h>
#include <setjmp.h>
#include <stdint.h>
#include <stddef.h>

void jade_rt_exit(int code) { jade_task_exit(code); __builtin_unreachable(); }

/* ── Dynamic loading (native packages) ────────────────────────────────────
 * Jade OS has no in-binary dynamic loader, so native packages are unsupported
 * here. Returning NULL makes jrt_native_load raise a catchable Jade error. */
void* jade_dlopen(const char* path) { (void)path; return NULL; }
void* jade_dlsym(void* handle, const char* sym) { (void)handle; (void)sym; return NULL; }

/* Fatal abort: kills the current task. __builtin_unreachable() suppresses
 * false "function may not return" warnings after jade_task_exit. */
#define JADE_ABORT() do { jade_task_exit(-1); __builtin_unreachable(); } while (0)

/* ── Async tasks (kernel-scheduler-backed) ────────────────────────────── */

struct jade_future {
    jade_task_id_t task_id;
    jade_value_t   result;
    jade_value_t   error;      /* raised value, valid when `failed` */
    const char*    error_type; /* struct type name of the raised value, or NULL */
    int            failed;     /* nonzero if the task raised an uncaught exception */
    int            done;       /* set when the task has finished writing result/error */
};

typedef struct {
    jade_task_fn        fn;
    jade_value_t*       args;   /* heap copy; freed after the task returns */
    int                 n_args;
    struct jade_future* future;
} TaskCtx;

static void task_entry(void* vp) {
    TaskCtx* tc = (TaskCtx*)vp;

    /* Catch an exception raised in the task body at the task boundary and stash
     * it; jade_await re-raises on the awaiting task, which owns the try/catch
     * frame (exception frames are per-task, mirroring the host backend). */
    jmp_buf task_buf;
    jade_value_t result = 0;
    int          failed = 0;
    jade_value_t error  = 0;
    const char*  error_type = NULL;

    if (setjmp(task_buf) == 0) {
        jade_exc_push_frame(&task_buf);
        result = tc->fn(tc->args, tc->n_args);
        jade_exc_pop();
    } else {
        failed     = 1;
        error      = jade_exc_value();
        error_type = jade_exc_type();
    }

    jade_free(tc->args);
    struct jade_future* fut = tc->future;
    jade_free(tc);          /* matched allocation: jade_alloc(sizeof(TaskCtx)) */

    fut->result     = result;
    fut->error      = error;
    fut->error_type = error_type;
    fut->failed     = failed;
    fut->done       = 1;
    /* Full memory barrier: ensure the result stores are visible to
     * jade_task_wait callers before completion is signalled by jade_task_exit. */
    __sync_synchronize();
    jade_task_exit(0);
}

jade_future_t jade_spawn(jade_task_fn fn, jade_value_t* args, int n_args) {
    struct jade_future* fut = jade_alloc(sizeof(struct jade_future));
    if (!fut) JADE_ABORT();
    fut->result = 0; fut->error = 0; fut->error_type = NULL; fut->failed = 0; fut->done = 0;

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
    /* Acquire fence to pair with task_entry's release __sync_synchronize before
     * jade_task_exit: ensures the completed task's stores to fut->{result,error,
     * error_type,failed} are visible here before we read them. (Don't assume
     * jade_task_wait is itself a full barrier.) */
    __sync_synchronize();
    if (fut->failed) {
        /* Re-raise on this (awaiting) task, where the try/catch frame lives. */
        jade_exc_throw_typed(fut->error, fut->error_type);
    }
    return fut->result;
}

void jade_future_free(jade_future_t future) {
    struct jade_future* fut = future;
    /* Contract: free only after jade_await has returned (the task finished and
     * wrote its result). Freeing earlier would let the still-running task_entry
     * write into freed memory. Matches the host backend's assert. */
    if (!fut->done) JADE_ABORT();
    jade_free(future);
}

#endif /* __JADE_OS__ */
