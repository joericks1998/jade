/* env/env.h — std::env runtime module ABI. */
#ifndef JADE_RT_ENV_H
#define JADE_RT_ENV_H

#include <stdint.h>

/* env.cwd() — current working directory as a TRUSTED heap string. */
char*   jrt_env_cwd(void);
/* env.get(name) — variable value as a TAINTED heap string, or NULL (nil) if
 * unset. Environment is external input → tainted, refused at execution sinks. */
char*   jrt_env_get(const char* name);
/* env.set(name, value) — set a process environment variable. Returns Nil. */
void    jrt_env_set(const char* name, const char* value);
/* env.args() — process arguments as a jade_array* of TRUSTED char* (argv[0]
 * first). Empty until jrt_set_args has run. */
void*   jrt_env_args(void);
/* jrt_set_args — record process argv for jrt_env_args (called once by main). */
void    jrt_set_args(int argc, char** argv);

#endif /* JADE_RT_ENV_H */
