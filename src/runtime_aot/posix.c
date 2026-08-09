/* Host (POSIX) backend for the Jade runtime: the concurrency layer over
 * pthreads plus the process-exit primitive. Everything else lives in the
 * platform-agnostic common.c. Compiled by build.rs for macOS/Linux hosts. */
#ifndef __JADE_KERNEL__

/* `dladdr` and `Dl_info` are a GNU extension, and glibc hides both behind this
 * unless it is defined. macOS declares them unconditionally, so the Linux build
 * was the only one that failed, and it failed inside cc-rs — which reports a
 * compiler error against a file nobody edited. It has to come before any header
 * is read, which is why it sits above runtime.h rather than beside dlfcn.h. */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

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
/* The loader's own account of why the last dlopen failed — "slice is not valid
 * mach-o file", a missing dependent library, an architecture mismatch. Worth
 * a shim of its own because a load failure without it names the file and
 * nothing else, which is the same message for every cause. */
const char* jade_dlerror(void) { const char* e = dlerror(); return e ? e : "unknown error"; }
void* jade_dlsym(void* handle, const char* sym) { return dlsym(handle, sym); }

/* The directory of the image this code is compiled into.
 *
 * `dladdr` on the address of a function defined *here* answers for whichever
 * image that copy of the runtime was linked into — the host executable when the
 * host asks, a package's own dylib when the package asks. That is why this uses
 * dladdr rather than /proc/self/exe or _NSGetExecutablePath: those two always
 * name the executable, which is the wrong answer for a package, and they need a
 * platform split that this does not.
 *
 * NULL when the loader cannot say. The buffer is _Thread_local for the reason
 * the errno buffer is: Jade tasks are real OS threads. */
const char* jade_image_dir(void) {
    static _Thread_local char buf[PATH_MAX];
    Dl_info info;
    if (!dladdr((const void*)(uintptr_t)&jade_image_dir, &info) || !info.dli_fname) return NULL;
    size_t n = strlen(info.dli_fname);
    if (n >= sizeof buf) return NULL;
    memcpy(buf, info.dli_fname, n + 1);
    char* slash = strrchr(buf, '/');
    if (!slash) return ".";
    /* A library at the filesystem root would otherwise become the empty
     * string, which is not a directory anything can be joined onto. */
    if (slash == buf) {
        buf[1] = '\0';
    } else {
        *slash = '\0';
    }
    return buf;
}

/* The canonical path of an existing file, or NULL when there is none.
 *
 * Load-bearing rather than cosmetic. dlopen keys a loaded image by the path it
 * was asked for, so two spellings of one file — a symlinked directory, a `..`
 * segment — produce two independent instances with two sets of globals. For a
 * library that owns a device that is not a waste of memory, it is two devices.
 * Canonicalizing first is what makes "the same dependency" mean one thing. */
const char* jade_realpath(const char* path) {
    /* PATH_MAX exactly: glibc's realpath writes up to that much and its
     * fortified build aborts on a smaller destination regardless of the path
     * it is actually resolving. See the note in runtime.h. */
    static _Thread_local char buf[PATH_MAX];
    if (!path) return NULL;
    return realpath(path, buf);
}

/* ── Async tasks ──────────────────────────────────────────────────────────
 *
 * The scheduler now lives in the Rust runtime (`jade-runtime/src/task.rs`):
 * a bounded worker pool instead of one detached pthread per spawn, and a
 * future that carries an ObjHeader so it can be refcounted like any other
 * value. What stays here is the part Rust cannot express.
 *
 * Jade exceptions are setjmp/longjmp over a _Thread_local frame stack, so an
 * exception raised inside a task cannot jump to the awaiting thread's
 * try/catch — and longjmp past a Rust frame is undefined behavior regardless.
 * So the task body runs inside a jump frame *here*, on the worker thread, and
 * the raised value travels back to the awaiter as data. `jade_await` re-raises
 * it on the awaiting thread, where the frame actually lives.
 */

extern void  jrt_set_task_invoker(int (*f)(jade_task_fn, jade_value_t*, int,
                                           jade_value_t*, jade_value_t*, const char**));
extern void* jrt_spawn(jade_task_fn fn, const jade_value_t* args, int n);
extern jade_value_t jrt_await_impl(void* fut, int* failed, jade_value_t* err, const char** ty);
extern jade_value_t jrt_await_word(jade_value_t w, int* failed, jade_value_t* err, const char** ty);
extern void  jrt_join_words(const jade_value_t* ws, int n, jade_value_t* out,
                            int* failed, jade_value_t* err, const char** ty);
extern void  jrt_join_impl(void* const* futs, int n, jade_value_t* out,
                           int* failed, jade_value_t* err, const char** ty);

/* Run a task body inside a jump frame. Returns nonzero if it raised, and then
 * reports the thrown value plus its struct type name so the awaiting thread can
 * re-raise with the type intact — typed `catch <Type>` arms over `await` match
 * on that name. */
static int jade_task_invoke(jade_task_fn fn, jade_value_t* args, int n,
                            jade_value_t* out_result, jade_value_t* out_err,
                            const char** out_type) {
    jmp_buf task_buf;
    if (setjmp(task_buf) == 0) {
        jade_exc_push_frame(&task_buf);
        *out_result = fn(args, n);
        jade_exc_pop();
        return 0;
    }
    *out_err  = jade_exc_value();
    *out_type = jade_exc_type();
    return 1;
}

/* Hand the shim to the Rust pool before main runs. This translation unit also
 * defines jade_rt_exit, so it is always pulled in from the static archive and
 * the constructor is never dropped. */
__attribute__((constructor))
static void jade_register_task_invoker(void) {
    jrt_set_task_invoker(jade_task_invoke);
}

/* Re-raise a task's failure on the calling (awaiting) thread.
 *
 * The DoubleAwait / NotAFuture texts are copied verbatim from the VM's
 * JadeError Display impl (src/frontend/error.rs): the parity gate diffs stdout
 * byte-for-byte, so a reworded message here reads as a backend divergence. */
static void jade_task_rethrow(int failed, jade_value_t err, const char* ty) {
    switch (failed) {
        case 1:
            jade_exc_throw_typed(err, ty);
            break;
        case 2:
            jade_exc_throw_typed(
                jrt_box_str(jrt_str_dup("cannot await the same Future more than once",
                                        JRT_TRUSTED)), NULL);
            break;
        case 3:
            jade_exc_throw_typed(
                jrt_box_str(jrt_str_dup("'await' applied to a non-Future value",
                                        JRT_TRUSTED)), NULL);
            break;
        default:
            break;
    }
}

jade_future_t jade_spawn(jade_task_fn fn, jade_value_t* args, int n_args) {
    return (jade_future_t)jrt_spawn(fn, args, n_args);
}

jade_value_t jade_await(jade_future_t future) {
    int failed = 0; jade_value_t err = 0; const char* ty = NULL;
    jade_value_t r = jrt_await_impl((void*)future, &failed, &err, &ty);
    jade_task_rethrow(failed, err, ty);
    return r;
}

/* Word-taking entry points. Codegen now passes futures as ordinary tagged
 * values, so these check the tag before touching the pointer — which is what
 * turns `await 5` from a segfault into a raised error. */
jade_value_t jade_await_word(jade_value_t w) {
    int failed = 0; jade_value_t err = 0; const char* ty = NULL;
    jade_value_t r = jrt_await_word(w, &failed, &err, &ty);
    jade_task_rethrow(failed, err, ty);
    return r;
}

void jade_join_words(const jade_value_t* ws, int n, jade_value_t* results_out) {
    int failed = 0; jade_value_t err = 0; const char* ty = NULL;
    jrt_join_words(ws, n, results_out, &failed, &err, &ty);
    jade_task_rethrow(failed, err, ty);
}

void jade_join(jade_future_t* futures, int n, jade_value_t* results_out) {
    int failed = 0; jade_value_t err = 0; const char* ty = NULL;
    jrt_join_impl((void* const*)futures, n, results_out, &failed, &err, &ty);
    jade_task_rethrow(failed, err, ty);
}

#endif /* !__JADE_KERNEL__ */
