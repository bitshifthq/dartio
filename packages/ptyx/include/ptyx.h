/**
 * Internal Dart/native ABI for ptyx.
 *
 * This header is authoritative for the native symbols used by
 * lib/src/ffi/controller.dart. It is versioned as one package-private ABI and
 * is not a general-purpose C library API.
 */
#ifndef PTYX_H
#define PTYX_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32)
#define PTYX_EXPORT __declspec(dllexport)
#else
#define PTYX_EXPORT __attribute__((visibility("default")))
#endif

#define PTYX_ABI_VERSION 7u

PTYX_EXPORT uint32_t ptyi_abi_version(void);
/**
 * Capability bits: signals=1, process groups=2, modes=4, ConPTY=8,
 * terminal name=16.
 */
PTYX_EXPORT uint32_t ptyi_capabilities(void);
/** Returns the calling thread's last synchronous native error, or zero. */
PTYX_EXPORT int32_t ptyi_last_error_code(void);
PTYX_EXPORT bool ptyi_init(void* dart_initialize_api_dl_data);
/** Dart finalizer callback. The token is the generation-tagged handle. */
PTYX_EXPORT void ptyi_finalize(void* token);
/** Abandons a handle whose owning Dart isolate has exited. */
PTYX_EXPORT bool ptyi_abandon(uint64_t handle);

PTYX_EXPORT uint64_t ptyi_spawn(
    const char* executable,
    const char* const* arguments,
    size_t argument_count,
    const char* const* environment,
    size_t environment_count,
    bool inherit_environment,
    const char* cwd,
    uint32_t rows,
    uint32_t columns,
    uint32_t pixel_width,
    uint32_t pixel_height,
    size_t input_capacity,
    size_t output_capacity);

/** Publishes a staged session after Dart has installed its routing state. */
PTYX_EXPORT bool ptyi_activate(
    uint64_t handle,
    int64_t output_port,
    int64_t event_port);

/**
 * Returns one after admission, zero for temporary backpressure, -1 when the
 * session can never accept more input, or -2 when the native runtime owner is
 * unavailable.
 */
PTYX_EXPORT int64_t ptyi_write(
    uint64_t handle,
    const uint8_t* bytes,
    size_t length);

PTYX_EXPORT bool ptyi_credit_async(uint64_t handle, size_t bytes);
PTYX_EXPORT bool ptyi_pause(uint64_t handle, bool paused);

PTYX_EXPORT bool ptyi_exit_status(uint64_t handle, int64_t* status);
PTYX_EXPORT int64_t ptyi_pid(uint64_t handle);
PTYX_EXPORT bool ptyi_size(uint64_t handle, uint32_t values[4]);
PTYX_EXPORT bool ptyi_resize(
    uint64_t handle,
    uint32_t rows,
    uint32_t columns,
    uint32_t pixel_width,
    uint32_t pixel_height);
/** Returns 1 when delivered, 0 after exit, and -1 on native failure. */
PTYX_EXPORT int32_t ptyi_signal(uint64_t handle, int32_t signal);

/** Returns -1 when unavailable, otherwise canonical/echo/signals bits. */
PTYX_EXPORT int32_t ptyi_mode(uint64_t handle);

/**
 * Returns -1 when unavailable. Otherwise returns the required byte count when
 * target is NULL or too small, or the written byte count on success.
 */
PTYX_EXPORT intptr_t ptyi_tty_name(
    uint64_t handle,
    uint8_t* target,
    size_t capacity);

PTYX_EXPORT bool ptyi_close(uint64_t handle);
PTYX_EXPORT bool ptyi_destroy(uint64_t handle);

#endif
