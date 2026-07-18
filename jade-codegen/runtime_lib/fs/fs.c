/* fs/ — std::fs runtime module.
 *
 * read/write/append/exists/delete/list_dir/mkdir. Uses only libc and the public
 * jade ABI (runtime.h). The VM (builtins/fs_pkg.rs) is the source of truth.
 *
 * Trust model (AOT-only, the VM has no taint): file contents are external input
 * → TAINTED; fs.read refuses a tainted PATH (read sink). The writers (write,
 * append, delete, mkdir) are not code-execution sinks, so they accept tainted
 * input and swallow errors, matching the VM which performs no trust check. */

#include "runtime.h"
#include "fs/fs.h"

#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <unistd.h>
#include <errno.h>
#include <dirent.h>
#include <sys/stat.h>

/* Raise a Jade error for a failed fs op, mirroring the VM's io_err shape
 * ("<op> '<path>': <strerror> (os error <n>)") so the message is explicit about
 * what failed and why. Caught by try/catch; uncaught it aborts with the message. */
static void fs_raise(const char* op, const char* path, int err) {
    char buf[4352];
    snprintf(buf, sizeof buf, "%s '%s': %s (os error %d)",
             op, path ? path : "(null)", strerror(err), err);
    jade_exc_throw_typed(jrt_box_str(jrt_str_dup(buf, JRT_TRUSTED)), NULL);
}

int32_t jrt_fs_exists(const char* path) {
    if (!path) return 0;
    return access(path, F_OK) == 0 ? 1 : 0;
}

/* trust!=0: operator asserts this file is trusted — skip the tainted-path
 * refusal and return TRUSTED content (so an LLM/program can read a file and use
 * it at sinks). trust==0: default safe behavior (refuse tainted path, content
 * TAINTED). Temporary override; kernel file-trust is the permanent design. */
char* jrt_fs_read(const char* path, int32_t trust) {
    uint8_t tag = trust ? JRT_TRUSTED : JRT_TAINTED;
    if (!trust) jrt_refuse_if_tainted(path, "fs.read(path)");
    if (!path) { fs_raise("read", path, EINVAL); return NULL; }
    FILE* f = fopen(path, "rb");
    if (!f) { fs_raise("read", path, errno); return NULL; }
    if (fseek(f, 0, SEEK_END) != 0) { int e = errno; fclose(f); fs_raise("read", path, e); return NULL; }
    long sz = ftell(f);
    if (sz < 0) { int e = errno; fclose(f); fs_raise("read", path, e); return NULL; }
    fseek(f, 0, SEEK_SET);
    char* out = jrt_str_new((size_t)sz, tag);
    size_t nr = fread(out, 1, (size_t)sz, f);
    fclose(f);
    /* Clamp the visible length on a short read (malloc'd buffer isn't zeroed). */
    if (nr < (size_t)sz) {
        char* clamped = jrt_str_new(nr, tag);
        if (nr > 0) memcpy(clamped, out, nr);
        jrt_str_free(out);
        return clamped;
    }
    return out;
}

/* Write `content` to `path`, truncating. Mirrors the VM's fs.write (returns
 * Nil). Not a code-execution sink → accepts tainted input; errors swallowed. */
void jrt_fs_write(const char* path, const char* content) {
    if (!path) { fs_raise("write", path, EINVAL); return; }
    FILE* f = fopen(path, "wb");
    if (!f) { fs_raise("write", path, errno); return; }
    if (content) {
        size_t len = strlen(content);
        if (len > 0 && fwrite(content, 1, len, f) != len) {
            int e = errno; fclose(f); fs_raise("write", path, e ? e : EIO); return;
        }
    }
    fclose(f);
}

/* Append `content` to `path` (create if absent). Mirrors the VM's fs.append. */
void jrt_fs_append(const char* path, const char* content) {
    if (!path) { fs_raise("append", path, EINVAL); return; }
    FILE* f = fopen(path, "ab");
    if (!f) { fs_raise("append", path, errno); return; }
    if (content) {
        size_t len = strlen(content);
        if (len > 0 && fwrite(content, 1, len, f) != len) {
            int e = errno; fclose(f); fs_raise("append", path, e ? e : EIO); return;
        }
    }
    fclose(f);
}

/* Remove a file. Mirrors the VM's fs.delete (raises on failure, e.g. the file
 * doesn't exist or the directory isn't writable). */
void jrt_fs_delete(const char* path) {
    if (!path) { fs_raise("delete", path, EINVAL); return; }
    if (remove(path) != 0) fs_raise("delete", path, errno);
}

/* Recursively create `path` and any missing parents (VM uses create_dir_all,
 * returns Nil). Each intermediate mkdir's EEXIST is ignored. */
void jrt_fs_mkdir(const char* path) {
    if (!path || !*path) { fs_raise("mkdir", path, EINVAL); return; }
    size_t n = strlen(path);
    char* tmp = malloc(n + 1);
    if (!tmp) { fs_raise("mkdir", path, ENOMEM); return; }
    memcpy(tmp, path, n + 1);
    for (size_t i = 1; i < n; i++) {
        if (tmp[i] == '/') {
            tmp[i] = '\0';
            mkdir(tmp, 0777);   /* ignore intermediate errors (e.g. EEXIST) */
            tmp[i] = '/';
        }
    }
    /* An already-existing target is fine (create_dir_all is idempotent); any
     * other failure (permission, a non-dir component) is raised. */
    if (mkdir(tmp, 0777) != 0 && errno != EEXIST) {
        int e = errno; free(tmp); fs_raise("mkdir", path, e); return;
    }
    free(tmp);
}
