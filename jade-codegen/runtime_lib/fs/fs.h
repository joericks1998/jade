/* fs/fs.h — std::fs runtime module ABI. */
#ifndef JADE_RT_FS_H
#define JADE_RT_FS_H

#include <stdint.h>

/* fs.read(path, trust) — file contents as a string ("" on error). By default
 * (trust==0) the content is TAINTED and a tainted path is refused (it would be a
 * read sink for an untrusted location). With trust!=0 the caller asserts the
 * file is trusted: the tainted-path refusal is skipped and the content is
 * returned TRUSTED. (Temporary operator override; the durable model is kernel
 * file-trust.) */
char*   jrt_fs_read(const char* path, int32_t trust);
/* fs.write(path, content) — truncating write. Returns Nil; errors swallowed. */
void    jrt_fs_write(const char* path, const char* content);
/* fs.append(path, content) — append (create if absent). Returns Nil. */
void    jrt_fs_append(const char* path, const char* content);
/* fs.exists(path) — 1 if the path exists, else 0. */
int32_t jrt_fs_exists(const char* path);
/* fs.delete(path) — remove a file. Returns Nil; errors swallowed. */
void    jrt_fs_delete(const char* path);
/* fs.list_dir(path) — entry names (no "."/"..", incl. hidden), TAINTED. Raises
 * a catchable runtime exception on read failure. Returns jade_array*. */
void*   jrt_fs_list_dir(const char* path);
/* fs.mkdir(path) — create path and missing parents (create_dir_all). Returns Nil. */
void    jrt_fs_mkdir(const char* path);

#endif /* JADE_RT_FS_H */
