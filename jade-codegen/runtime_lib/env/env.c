/* env/ — std::env runtime module.
 *
 * cwd/get/set/args. Uses only libc + the public jade ABI. The VM's env_pkg.rs
 * is the source of truth.
 *
 * Trust model (AOT-only): env.get is external, attacker-influenceable input →
 * TAINTED (refused at execution sinks); cwd and args are the program's own
 * invocation → TRUSTED. */

#include "runtime.h"
#include "env/env.h"

#include <stdlib.h>
#include <unistd.h>

char* jrt_env_cwd(void) {
    char buf[4096];
    if (getcwd(buf, sizeof(buf))) return jrt_str_dup(buf, JRT_TRUSTED);
    return jrt_str_dup("", JRT_TRUSTED);
}

char* jrt_env_get(const char* name) {
    if (!name) return NULL;
    const char* v = getenv(name);
    return v ? jrt_str_dup(v, JRT_TAINTED) : NULL;
}

void jrt_env_set(const char* name, const char* value) {
    if (name) setenv(name, value ? value : "", 1);
}

/* Process argv, captured by main() before user code runs (see codegen's entry
 * emission). g_argv points at the OS-owned argv array, valid for the whole run. */
static int    g_argc = 0;
static char** g_argv = NULL;
void jrt_set_args(int argc, char** argv) { g_argc = argc; g_argv = argv; }

/* env.args() — process arguments as a tagged ObjHeader array of TRUSTED string
 * words (argv[0] first). Chunk-path native: the returned word is already boxed
 * (jrt_box_ptr), so codegen consumes it directly. */
jade_value_t jrt_env_args(void) {
    void* arr = jrt_karr_new();
    for (int i = 0; i < g_argc; i++) {
        const char* a = g_argv && g_argv[i] ? g_argv[i] : "";
        jrt_karr_push(arr, jrt_box_str(jrt_str_dup(a, JRT_TRUSTED)));
    }
    return jrt_box_ptr(arr);
}
