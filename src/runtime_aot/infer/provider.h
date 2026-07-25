/* provider.h — in-process inference via a loaded provider package.
 *
 * The daemon-free inference path. When an active provider `.so` is installed
 * (in $HOME/.jade/provider/active/), a compiled Jade binary drives it directly
 * instead of connecting to the jade-tree daemon. These entry points mirror
 * ipc.h's jrt_ipc_* in signature — so infer.c routes to them with a one-line
 * branch — and are IMPLEMENTED IN RUST, in jade-runtime's `provider` module
 * (linked via libjade_runtime.a), reusing the same frame decoder as the daemon
 * transport. */

#pragma once

#include <stddef.h>
#include <stdint.h>

#include "ipc.h" /* jrt_token_cb */

/* Nonzero if an active provider is installed — the cue to drive it in-process
 * rather than talk to the daemon. Cheap: a directory check, no dlopen. */
int jrt_provider_available(void);

/* Mirror of jrt_ipc_request: drive the active provider for one request,
 * accumulating TOKEN frames until DONE. Same malloc'd-buffer contract; prints
 * and exit(1)s on any failure (a compiled binary has no interpreter to unwind
 * into), so a returned buffer is always complete. */
void jrt_provider_request(const void* req_json, size_t req_len,
                          char** resp_out, size_t* resp_len_out,
                          uint64_t* tokens_used_out);

/* Mirror of jrt_ipc_request_streaming: as above, plus on_token per TOKEN frame. */
void jrt_provider_request_streaming(const void* req_json, size_t req_len,
                                    jrt_token_cb on_token, void* user,
                                    char** resp_out, size_t* resp_len_out,
                                    uint64_t* tokens_used_out);
