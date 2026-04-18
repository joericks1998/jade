/* jade_rt_pthread.c — pthread-backed async runtime for Linux / macOS. */

#include "jade_rt.h"

#include <assert.h>
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

struct jade_future {
    pthread_mutex_t mutex;
    pthread_cond_t  cond;
    jade_value_t    result;
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

    jade_value_t result = tc->fn(tc->args, tc->n_args);

    free(tc->args);
    struct jade_future* fut = tc->future;
    free(tc);

    pthread_mutex_lock(&fut->mutex);
    fut->result = result;
    fut->done   = 1;
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
    jade_value_t result = fut->result;
    pthread_mutex_unlock(&fut->mutex);
    return result;
}

void jade_join(jade_future_t* futures, int n, jade_value_t* results_out) {
    for (int i = 0; i < n; i++) {
        results_out[i] = jade_await(futures[i]);
    }
}

void jade_future_free(jade_future_t future) {
    struct jade_future* fut = future;
    assert(fut->done && "jade_future_free called before jade_await");
    pthread_mutex_destroy(&fut->mutex);
    pthread_cond_destroy(&fut->cond);
    free(fut);
}
