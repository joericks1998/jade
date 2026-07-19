/* Host (POSIX) backend for the Jade runtime: the concurrency layer over
 * pthreads plus the process-exit primitive. Everything else lives in the
 * platform-agnostic common.c. Compiled by build.rs for macOS/Linux hosts. */
#ifndef __JADE_OS__

#include "runtime.h"

#include <assert.h>
#include <dlfcn.h>
#include <pthread.h>
#include <setjmp.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

void jade_rt_exit(int code) { exit(code); }

/* ── Dynamic loading (native packages) ────────────────────────────────── */
void* jade_dlopen(const char* path) { return dlopen(path, RTLD_NOW | RTLD_LOCAL); }
void* jade_dlsym(void* handle, const char* sym) { return dlsym(handle, sym); }

/* ── Async tasks (pthread-backed) ─────────────────────────────────────── */

struct jade_future {
    pthread_mutex_t mutex;
    pthread_cond_t  cond;
    jade_value_t    result;
    jade_value_t    error;      /* raised value, valid when `failed` */
    const char*     error_type; /* struct type name of the raised value, or NULL */
    int             failed;  /* nonzero if the task raised an uncaught exception */
    int             done;
};

typedef struct {
    jade_task_fn        fn;
    jade_value_t*       args;   /* heap copy; freed after the task returns */
    int                 n_args;
    struct jade_future* future;
} TaskCtx;

static void* task_runner(void* vp) {
    TaskCtx* tc = (TaskCtx*)vp;

    /* Exception frames are per-thread (exc_stack is _Thread_local), so an
     * exception raised in the task body can't longjmp to the awaiting thread's
     * try/catch. Catch it here at the task boundary and stash the raised value;
     * jade_await re-raises it on the awaiting thread, which owns the frame. */
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
        /* Preserve the struct type name so the awaiting thread re-raises with it
         * intact — typed `catch <Type>` arms over `await` match on this name. */
        error_type = jade_exc_type();
    }

    free(tc->args);
    struct jade_future* fut = tc->future;
    free(tc);

    pthread_mutex_lock(&fut->mutex);
    fut->result     = result;
    fut->error      = error;
    fut->error_type = error_type;
    fut->failed     = failed;
    fut->done       = 1;
    pthread_cond_broadcast(&fut->cond);
    pthread_mutex_unlock(&fut->mutex);

    return NULL;
}

jade_future_t jade_spawn(jade_task_fn fn, jade_value_t* args, int n_args) {
    struct jade_future* fut = calloc(1, sizeof(struct jade_future));
    if (!fut) abort();

    if (pthread_mutex_init(&fut->mutex, NULL) != 0) {
        free(fut);
        abort();
    }
    if (pthread_cond_init(&fut->cond, NULL) != 0) {
        pthread_mutex_destroy(&fut->mutex);
        free(fut);
        abort();
    }

    jade_value_t* args_copy = NULL;
    if (n_args > 0) {
        /* Guard against overflow on 32-bit targets where size_t is 32 bits. */
        if ((size_t)n_args > SIZE_MAX / sizeof(jade_value_t)) abort();
        args_copy = malloc(sizeof(jade_value_t) * (size_t)n_args);
        if (!args_copy) abort();
        memcpy(args_copy, args, sizeof(jade_value_t) * (size_t)n_args);
    }

    TaskCtx* tc = malloc(sizeof(TaskCtx));
    if (!tc) {
        free(args_copy);
        pthread_mutex_destroy(&fut->mutex);
        pthread_cond_destroy(&fut->cond);
        free(fut);
        abort();
    }
    tc->fn     = fn;
    tc->args   = args_copy;
    tc->n_args = n_args;
    tc->future = fut;

    pthread_t tid;
    if (pthread_create(&tid, NULL, task_runner, tc) != 0) {
        free(tc);
        free(args_copy);
        pthread_mutex_destroy(&fut->mutex);
        pthread_cond_destroy(&fut->cond);
        free(fut);
        abort();
    }
    pthread_detach(tid);

    return fut;
}

jade_value_t jade_await(jade_future_t future) {
    struct jade_future* fut = future;
    pthread_mutex_lock(&fut->mutex);
    while (!fut->done) {
        pthread_cond_wait(&fut->cond, &fut->mutex);
    }
    jade_value_t result     = fut->result;
    int          failed     = fut->failed;
    jade_value_t error      = fut->error;
    const char*  error_type = fut->error_type;
    pthread_mutex_unlock(&fut->mutex);
    if (failed) {
        /* Re-raise on this (awaiting) thread, where the try/catch frame lives.
         * Carry the type name so typed catch arms still match. */
        jade_exc_throw_typed(error, error_type);
    }
    return result;
}

void jade_future_free(jade_future_t future) {
    struct jade_future* fut = future;
    assert(fut->done && "jade_future_free called before jade_await");
    pthread_mutex_destroy(&fut->mutex);
    pthread_cond_destroy(&fut->cond);
    free(fut);
}

#endif /* !__JADE_OS__ */
