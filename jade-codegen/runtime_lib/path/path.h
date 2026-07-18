/* path/path.h — std::path runtime module ABI. All return TRUSTED-or-input
 * tagged strings (trust propagated from the input path). */
#ifndef JADE_RT_PATH_H
#define JADE_RT_PATH_H

#include <stdint.h>

/* path.basename(p) — final component (filename+ext), "" if none. */
char*   jrt_path_basename(const char* p);
/* path.ext(p) — extension incl. leading dot (".rs"), or NULL (nil) if none. */
char*   jrt_path_ext(const char* p);
/* path.join(a, b) — join two segments (absolute `b` replaces `a`). */
char*   jrt_path_join(const char* a, const char* b);
/* path.dirname(p) — parent dir; "." for a bare name, "/" for a root child. */
char*   jrt_path_dirname(const char* p);
/* path.stem(p) — filename without its final extension. */
char*   jrt_path_stem(const char* p);
/* path.abs(p) — absolute form (no symlink/existence resolution). */
char*   jrt_path_abs(const char* p);
/* path.is_abs(p) — 1 if the path is absolute, else 0. */
int32_t jrt_path_is_abs(const char* p);

#endif /* JADE_RT_PATH_H */
