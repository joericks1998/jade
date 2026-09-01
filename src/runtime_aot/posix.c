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

extern void  jrt_set_task_invoker(int (*f)(jade_task_fn, jade_value_t*, int, int,
                                           jade_value_t*, jade_value_t*, const char**));
extern void* jrt_spawn(jade_task_fn fn, const jade_value_t* args, int n);
extern int32_t jrt_recur_enter_task(void);
extern void    jrt_recur_leave_task(int32_t saved);
extern jade_value_t jrt_await_impl(void* fut, int* failed, jade_value_t* err, const char** ty);
extern jade_value_t jrt_await_word(jade_value_t w, int* failed, jade_value_t* err, const char** ty);
extern int          jrt_future_ready(int64_t w);
extern int          jrt_future_cancel(int64_t w);
extern int          jrt_wait_any(const int64_t* words, int n);
extern int          jrt_join_settle(const int64_t* ws, int n, int64_t* out_vals, int* out_ok);
extern void  jrt_join_words(const jade_value_t* ws, int n, jade_value_t* out,
                            int* failed, jade_value_t* err, const char** ty);
extern void  jrt_join_impl(void* const* futs, int n, jade_value_t* out,
                           int* failed, jade_value_t* err, const char** ty);

/* Run a task body inside a jump frame. Returns nonzero if it raised, and then
 * reports the thrown value plus its struct type name so the awaiting thread can
 * re-raise with the type intact — typed `catch <Type>` arms over `await` match
 * on that name. */
static int jade_task_invoke(jade_task_fn fn, jade_value_t* args, int n,
                            int fresh_budget,
                            jade_value_t* out_result, jade_value_t* out_err,
                            const char** out_type) {
    jmp_buf task_buf;
    /* Save what belongs to whoever is running this body, because that may be a
     * thread with work of its own: a pool worker is reused for task after task,
     * and an awaiting thread now runs a task inline rather than park (see
     * `await_one` in runtime/src/task.rs). Neither should inherit the body's
     * leftovers, and the body should not inherit theirs.
     *
     * `saved_exc` is a floor to unwind back to, not a slot to start from — the
     * body's handler frames must sit *above* the caller's, so the throw path
     * finds the shim's frame before any of theirs. */
    int32_t saved_exc = jade_exc_depth();
    /* A fresh budget only where the body really does start a stack: a pool
     * worker. An awaiting thread running the body inline is genuinely deeper on
     * a stack already in use, so it keeps counting and the recursion limit stays
     * the thing that stops a runaway before the OS does. */
    int32_t saved_recur = fresh_budget ? jrt_recur_enter_task() : jrt_recur_depth();

    if (setjmp(task_buf) == 0) {
        jade_exc_push_frame(&task_buf);
        *out_result = fn(args, n);
        jade_exc_pop();
        jade_exc_restore(saved_exc);
        if (fresh_budget) jrt_recur_leave_task(saved_recur);
        else              jrt_recur_restore(saved_recur);
        return 0;
    }
    *out_err  = jade_exc_value();
    *out_type = jade_exc_type();
    /* A raise unwinds one frame per throw and lands here, so the depth is
     * already back at `saved_exc` on the ordinary path. A `return` out of a
     * `try` inside the body can still leave one behind, and that frame's
     * jmp_buf died with the C frame holding it. */
    jade_exc_restore(saved_exc);
    if (fresh_budget) jrt_recur_leave_task(saved_recur);
    else              jrt_recur_restore(saved_recur);
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
        /* The task machinery's own failures are runtime errors, so they are
         * `RuntimeError` structs like every other one. Raising them as bare
         * strings meant `catch e` bound a string and `catch RuntimeError e`
         * did not match at all, so a program that handled a double await
         * interpreted died on it compiled. `jrt_throw_runtime` is what every
         * other runtime failure here already uses. */
        case 2:
            jrt_throw_runtime("cannot await the same Future more than once");
            break;
        case 3:
            jrt_throw_runtime("'await' applied to a non-Future value");
            break;
        case 4:
            jrt_throw_runtime("awaiting a cancelled task");
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
/* `f.ready()` — has the task finished, without waiting for it.
 *
 * `await` is otherwise the only way to read a future, and it blocks, which
 * makes a task useless to anything with a loop it cannot stop. The receiver
 * guard runs at the call site, so by here `w` is a future and the sentinel
 * cannot come back; checked anyway, because this is the boundary and a value
 * arriving from anywhere else is not the guard's business. Routing the failure
 * through `jrt_require_kind` is what gets the interpreter's wording for free. */
/* `time.sleep(secs)` and `time.after(secs)` take the tagged word, so an int
 * argument works — codegen used to unbox a float straight from it, which
 * null-dereferenced on `time.sleep(0)`. Both raise the interpreter's wording
 * for an argument that is not a number. */
extern int     jrt_time_sleep_word(int64_t w);
extern int64_t jrt_time_after(int64_t w);

void jade_time_sleep(jade_value_t w) {
    if (jrt_time_sleep_word((int64_t)w) != 0) jrt_throw_runtime("type error: time.sleep");
}

jade_value_t jade_time_after(jade_value_t w) {
    int64_t r = jrt_time_after((int64_t)w);
    if (r == -1) jrt_throw_runtime("type error: time.after");
    return (jade_value_t)r;
}

void jade_future_cancel(jade_value_t w) {
    if (jrt_future_cancel((int64_t)w) < 0) {
        jrt_require_kind((int64_t)w, JRT_WANT_FUTURE, "cancel");
    }
}

/* `wait(futures)` — block until one of them is settled, and answer which.
 *
 * The index, and it consumes nothing: the caller then awaits the one that is
 * ready, which costs nothing because it is. A deadline is an ordinary member of
 * the list, which is why `wait` needs no timeout of its own.
 *
 * The messages are the interpreter's, word for word, because the parity gate
 * diffs stdout and a caught `e.message` is stdout. */
/* `join(a, b, settle = true)` — every outcome, rather than the first failure.
 *
 * A fan-out with one failure should hand back the ones that worked, and plain
 * `join` throws them away. One dict per task, so the shape needs no new type:
 * {"ok": true, "value": v} or {"ok": false, "error": e}.
 *
 * A member that is not a future, or one already awaited, still raises. `settle`
 * covers what the tasks did; it does not turn calling `join` wrongly into data. */
jade_value_t jade_join_settle(const jade_value_t* ws, int n) {
    if (n < 0) n = 0;
    if (n > 256) jrt_throw_runtime("join: too many futures");
    /* VLAs rather than malloc: the raises below are longjmps that would skip a
     * free. `n == 0` would be a zero-length VLA, so give them a floor. */
    int64_t vals[n > 0 ? n : 1];
    int oks[n > 0 ? n : 1];

    int r = jrt_join_settle((const int64_t*)ws, n, vals, oks);
    if (r == -1) jrt_throw_runtime("'await' applied to a non-Future value");
    if (r == -3) jrt_throw_runtime("cannot await the same Future more than once");
    if (r < 0)  jrt_throw_runtime("join: could not await");

    void* out = jrt_karr_new();
    for (int i = 0; i < n; i++) {
        void* d = jrt_kdict_new();
        jrt_kdict_set(d, jrt_box_str(jrt_str_dup("ok", JRT_TRUSTED)), jrt_box_bool(oks[i]));
        jrt_kdict_set(d,
                      jrt_box_str(jrt_str_dup(oks[i] ? "value" : "error", JRT_TRUSTED)),
                      vals[i]);
        jrt_karr_push(out, jrt_box_ptr(d));
    }
    return jrt_box_ptr(out);
}

jade_value_t jade_wait(jade_value_t arr) {
    if (!jrt_is_ptr(arr) || jrt_kind_of(jrt_unbox_ptr(arr)) != JK_ARRAY) {
        jrt_throw_runtime("wait: expects an array of futures");
    }
    void* a = jrt_unbox_ptr(arr);
    int64_t n = jrt_coll_array_len(a);
    if (n <= 0) jrt_throw_runtime("wait: no futures to wait for");
    if (n > 256) jrt_throw_runtime("wait: too many futures");

    /* A VLA rather than malloc: `jrt_wait_any` can raise nothing itself, but the
     * throws above and below are longjmps that would skip a free. */
    int64_t buf[n];
    for (int64_t i = 0; i < n; i++) buf[i] = (int64_t)jrt_coll_array_get(a, i);

    int r = jrt_wait_any(buf, (int)n);
    /* -1 is a member that is not a future, -2 an empty list. The second cannot
     * happen here, since the count was checked, and is handled anyway because
     * this is the boundary. */
    if (r == -1) jrt_throw_runtime("'await' applied to a non-Future value");
    if (r < 0) jrt_throw_runtime("wait: no futures to wait for");
    return jrt_box_int(r);
}

jade_value_t jade_future_ready(jade_value_t w) {
    int r = jrt_future_ready((int64_t)w);
    if (r < 0) jrt_require_kind((int64_t)w, JRT_WANT_FUTURE, "ready");
    return jrt_box_bool(r);
}

jade_value_t jade_await_word(jade_value_t w) {
    int failed = 0; jade_value_t err = 0; const char* ty = NULL;
    jade_value_t r = jrt_await_word(w, &failed, &err, &ty);
    jade_task_rethrow(failed, err, ty);
    return r;
}

/* A join hands its results to the caller, whose collect-into-an-array loop takes
 * a reference for the array and gives this one back. On a failure that loop
 * never runs — the rethrow below is a longjmp — so the results already gathered
 * had no owner at all. A `try { join(ok(), bad()) } catch` in a loop leaked
 * every successful sibling's value. Slots for tasks that failed hold 0, which is
 * an int word and no-ops. */
static void jade_join_release(jade_value_t* results, int n) {
    for (int i = 0; i < n; i++) jrt_decref(results[i]);
}

void jade_join_words(const jade_value_t* ws, int n, jade_value_t* results_out) {
    int failed = 0; jade_value_t err = 0; const char* ty = NULL;
    jrt_join_words(ws, n, results_out, &failed, &err, &ty);
    if (failed) jade_join_release(results_out, n);
    jade_task_rethrow(failed, err, ty);
}

void jade_join(jade_future_t* futures, int n, jade_value_t* results_out) {
    int failed = 0; jade_value_t err = 0; const char* ty = NULL;
    jrt_join_impl((void* const*)futures, n, results_out, &failed, &err, &ty);
    if (failed) jade_join_release(results_out, n);
    jade_task_rethrow(failed, err, ty);
}

#endif /* !__JADE_KERNEL__ */

/* ── The program's own stack ──────────────────────────────────────────────
 *
 * `jade run` executes on a thread the CLI gives 256 MB of stack (see
 * `vm::chunk::VM_STACK_SIZE`), because interpreting a deeply recursive program
 * needs far more native stack than the 8 MB a process gets by default. A
 * compiled binary ran on that default, so the same program that printed fine
 * interpreted segfaulted compiled — a 2,000-deep nested array was enough, since
 * rendering one walks it recursively.
 *
 * So a binary runs its body on a thread of its own, sized to match. Every piece
 * of per-execution state the runtime keeps is already thread-local (the handler
 * stack, the recursion counter, the generator's yield stack), because async
 * tasks have always run on other threads, so there is nothing to move.
 *
 * Reserving address space costs nothing until it is touched: a 64-bit process
 * does not commit stack pages it never writes.
 *
 * If the thread cannot be created the body still runs, here on the main stack —
 * a shallower limit is much better than refusing to start. */
#define JRT_MAIN_STACK_SIZE ((size_t)256 * 1024 * 1024)

static void* jrt_main_trampoline(void* arg) {
    jade_body_fn body = (jade_body_fn)arg;
    body();
    return NULL;
}

void jrt_run_main(jade_body_fn body) {
    pthread_attr_t attr;
    pthread_t t;
    if (pthread_attr_init(&attr) != 0) { body(); return; }
    if (pthread_attr_setstacksize(&attr, JRT_MAIN_STACK_SIZE) != 0
        || pthread_create(&t, &attr, jrt_main_trampoline, (void*)body) != 0) {
        pthread_attr_destroy(&attr);
        body();
        return;
    }
    pthread_attr_destroy(&attr);
    pthread_join(t, NULL);
}
