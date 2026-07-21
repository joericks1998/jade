/* ipc.h — persistent connection to the jade-tree inference daemon.
 *
 * One Unix-domain socket fd is opened lazily on the first request and held
 * open for the program's lifetime. Requests are serialized by an internal
 * mutex; concurrent calls from spawned tasks are safe. Any transport-level
 * failure (connect, write, read, daemon ERROR frame) calls exit(1) with a
 * stderr message — the runtime never returns partial data.
 *
 * These entry points are IMPLEMENTED IN RUST, in jade-runtime's `infer`
 * module (linked via libjade_runtime.a), not in a sibling ipc.c. The C copy
 * that used to live here reimplemented the same wire protocol as the VM's
 * client, and the two had drifted — see that module's header for what
 * differed. This header remains as the C-side declaration of the ABI. */

#pragma once

#include <stddef.h>
#include <stdint.h>

/* Wire frame types, matching ovata-infer-protocol's response::tag. The
 * Rust implementation of these entry points takes them from that crate;
 * these copies exist only so C callers can name them. */
#define JRT_FRAME_TOKEN 0x01
#define JRT_FRAME_DONE  0x02
#define JRT_FRAME_ERROR 0x03
#define JRT_FRAME_META  0x04
#define JRT_FRAME_JSON  0x05  /* structured (non-token) result chunk, e.g. health */

/* Per-token callback for streaming requests.
 *   bytes:   token payload (NOT NUL-terminated)
 *   len:     payload length in bytes
 *   user:    opaque caller pointer */
typedef void (*jrt_token_cb)(const char* bytes, size_t len, void* user);

/* The model the daemon reported in its most recent META frame ("" before any
 * request). Backs `__model__`. */
const char* jrt_reported_model(void);

/* Send a framed request and accumulate TOKEN frames until DONE.
 *   req_json:        already-encoded JSON body (length-prefix is added internally)
 *   req_len:         body length in bytes
 *   resp_out:        receives a malloc'd NUL-terminated response string
 *   resp_len_out:    receives the response byte length (may be NULL)
 *   tokens_used_out: receives the 8-byte LE count from the DONE payload
 *                    (0 if absent or shorter than 8 bytes; may be NULL) */
void jrt_ipc_request(const void* req_json, size_t req_len,
                     char** resp_out, size_t* resp_len_out,
                     uint64_t* tokens_used_out);

/* Like jrt_ipc_request but invokes on_token per TOKEN frame in addition to
 * accumulating the full response. */
void jrt_ipc_request_streaming(const void* req_json, size_t req_len,
                               jrt_token_cb on_token, void* user,
                               char** resp_out, size_t* resp_len_out,
                               uint64_t* tokens_used_out);

/* Like jrt_ipc_request but accumulates 0x05 JSON frames (instead of TOKEN
 * frames) into resp_out, ignoring any TOKEN frames. Used by structured ops
 * such as llm.health that read a JSON object rather than a token stream. */
void jrt_ipc_request_json(const void* req_json, size_t req_len,
                          char** resp_out, size_t* resp_len_out);

/* Close the connection. Registered via atexit by the first request; safe
 * to call from program code as well. */
void jrt_ipc_shutdown(void);
