/* infer.h — the high-level prompt API.
 *
 * All LLM-facing entry points live here. Each builds a request dict and calls
 * the installed provider package in process, through native.c. The JSON request
 * payloads and the ipc.h socket they used to travel on are gone with the
 * inference daemon. */

#pragma once

#include <stddef.h>
#include <stdint.h>

/* JRT_TRUSTED / JRT_TAINTED are declared in runtime.h. */
