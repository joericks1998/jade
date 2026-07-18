/* path/ — std::path runtime module.
 *
 * Lexical path manipulation mirroring Rust's std::path (the VM's path_pkg.rs).
 * Trust is propagated from the input path. Uses only libc + the public jade ABI. */

#include "runtime.h"
#include "path/path.h"

#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/* Length of the final path component (filename+ext), ignoring trailing
 * slashes. Sets *start to its first byte. Returns 0 (component empty / "." /
 * "..") to signal "no file name", matching Rust Path::file_name -> None. */
static size_t path_filename(const char* p, const char** start) {
    size_t len = strlen(p);
    while (len > 0 && p[len-1] == '/') len--;          /* strip trailing '/' */
    size_t end = len;
    while (len > 0 && p[len-1] != '/') len--;          /* back to last '/' */
    const char* s = p + len;
    size_t n = end - len;
    if (n == 0) { *start = s; return 0; }
    if ((n == 1 && s[0] == '.') ||
        (n == 2 && s[0] == '.' && s[1] == '.')) { *start = s; return 0; }
    *start = s;
    return n;
}

char* jrt_path_basename(const char* p) {
    uint8_t t = jrt_trust_of(p);
    if (!p) return jrt_str_dup("", t);
    const char* s;
    size_t n = path_filename(p, &s);
    char* out = jrt_str_new(n, t);
    if (n > 0) memcpy(out, s, n);
    return out;
}

char* jrt_path_ext(const char* p) {
    if (!p) return NULL;
    const char* s;
    size_t n = path_filename(p, &s);
    if (n == 0) return NULL;
    /* last '.' within the file name, not at index 0 (a leading dot is a
     * hidden-file marker, not an extension — matches Rust Path::extension). */
    size_t dot = n;
    for (size_t i = n; i > 0; i--) { if (s[i-1] == '.') { dot = i-1; break; } }
    if (dot == n || dot == 0) return NULL;
    uint8_t t = jrt_trust_of(p);
    size_t elen = n - dot;            /* includes the dot */
    char* out = jrt_str_new(elen, t);
    memcpy(out, s + dot, elen);
    return out;
}

char* jrt_path_join(const char* a, const char* b) {
    uint8_t t = jrt_trust_of(a) | jrt_trust_of(b);
    if (!a) a = "";
    if (!b) b = "";
    if (b[0] == '/') return jrt_str_dup(b, t);   /* absolute b replaces a */
    if (a[0] == '\0') return jrt_str_dup(b, t);
    size_t alen = strlen(a), blen = strlen(b);
    int sep = (a[alen-1] != '/') ? 1 : 0;        /* avoid doubling '/' */
    char* out = jrt_str_new(alen + (size_t)sep + blen, t);
    memcpy(out, a, alen);
    if (sep) out[alen] = '/';
    memcpy(out + alen + (size_t)sep, b, blen);
    return out;
}

/* path.dirname(p) — parent directory; "." for a bare filename, "/" for a root
 * child. Mirrors Rust Path::parent(): trailing slashes are stripped before the
 * last separator is located. */
char* jrt_path_dirname(const char* p) {
    uint8_t t = jrt_trust_of(p);
    if (!p || !*p) return jrt_str_dup(".", t);
    size_t n = strlen(p);
    while (n > 1 && p[n-1] == '/') n--;          /* strip trailing slashes */
    size_t slash = n;
    for (size_t i = n; i > 0; i--) { if (p[i-1] == '/') { slash = i-1; break; } }
    if (slash == n) return jrt_str_dup(".", t);  /* no separator → "." */
    if (slash == 0) return jrt_str_dup("/", t);  /* parent is the root */
    size_t end = slash;
    while (end > 1 && p[end-1] == '/') end--;     /* collapse repeated seps */
    char* out = jrt_str_new(end, t);
    memcpy(out, p, end);
    return out;
}

/* path.stem(p) — filename without its final extension. Mirrors Rust
 * Path::file_stem(): a leading dot is a hidden-file marker, not an extension. */
char* jrt_path_stem(const char* p) {
    uint8_t t = jrt_trust_of(p);
    if (!p) return jrt_str_dup("", t);
    const char* s;
    size_t n = path_filename(p, &s);
    size_t dot = n;
    for (size_t i = n; i > 0; i--) { if (s[i-1] == '.') { dot = i-1; break; } }
    size_t stemlen = (dot == n || dot == 0) ? n : dot;   /* no ext, or leading dot */
    char* out = jrt_str_new(stemlen, t);
    if (stemlen > 0) memcpy(out, s, stemlen);
    return out;
}

/* path.abs(p) — absolute form without resolving symlinks or requiring the path
 * to exist. Mirrors the VM's std::path::absolute for the common cases: an
 * already-absolute path is returned verbatim; a relative one is joined onto the
 * cwd. (`.`/`..` components are not lexically normalized — a minor, documented
 * divergence from Rust's normalizer.) */
char* jrt_path_abs(const char* p) {
    uint8_t t = jrt_trust_of(p);
    if (!p) p = "";
    if (p[0] == '/') return jrt_str_dup(p, t);
    char cwd[4096];
    if (!getcwd(cwd, sizeof(cwd))) return jrt_str_dup(p, t);
    size_t cl = strlen(cwd), pl = strlen(p);
    char* out = jrt_str_new(cl + 1 + pl, t);
    memcpy(out, cwd, cl);
    out[cl] = '/';
    memcpy(out + cl + 1, p, pl);
    return out;
}

/* path.is_abs(p) — true if the path begins at the filesystem root. */
int32_t jrt_path_is_abs(const char* p) {
    return (p && p[0] == '/') ? 1 : 0;
}
