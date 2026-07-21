/* infer.h — inference request builders and high-level prompt API.
 *
 * All LLM-facing entry points live here. They construct InferenceRequest
 * JSON payloads (matching jade-protocol) and dispatch them through
 * ipc.h, which owns the persistent socket. */

#pragma once

#include <stddef.h>
#include <stdint.h>

/* JRT_TRUSTED / JRT_TAINTED are declared in runtime.h. */
