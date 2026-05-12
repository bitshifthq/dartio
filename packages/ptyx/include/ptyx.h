/**
 * @file ptyx.h
 * @brief C ABI for the ptyx native library.
 *
 * The ABI is stable within a matching major version. Callers should initialize
 * option structs with ptyx_session_options_init() and compare
 * ptyx_abi_version_major() with PTYX_ABI_VERSION_MAJOR before using a loaded
 * dynamic library.
 */

#ifndef PTYX_H
#define PTYX_H

#ifdef __cplusplus
extern "C" {
#endif

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32)
  #if defined(PTYX_BUILD_SHARED)
    #define PTYX_API __declspec(dllexport)
  #elif defined(PTYX_SHARED)
    #define PTYX_API __declspec(dllimport)
  #else
    #define PTYX_API
  #endif
#elif defined(__GNUC__) || defined(__clang__)
  #define PTYX_API __attribute__((visibility("default")))
#else
  #define PTYX_API
#endif

#if defined(_WIN32)
  #define PTYX_CALL __cdecl
#else
  #define PTYX_CALL
#endif

#define PTYX_ABI_VERSION_MAJOR 11u
#define PTYX_ABI_VERSION_MINOR 0u

/** Status code returned by fallible ABI functions. */
typedef uint32_t ptyx_status_t;

/** Bitset used for option flags and validity masks. */
typedef uint32_t ptyx_flags_t;

/** Environment handling mode for a spawned child process. */
typedef uint32_t ptyx_env_mode_t;

/** Operation completed successfully. */
#define PTYX_STATUS_OK                 0u
/** Unclassified error. Inspect ptyx_last_error_message() for details. */
#define PTYX_STATUS_ERROR              1u
/** One or more arguments were invalid. */
#define PTYX_STATUS_INVALID_ARGUMENT   2u
/** The operation is not supported on this platform. */
#define PTYX_STATUS_UNSUPPORTED        3u
/** Memory allocation failed. */
#define PTYX_STATUS_OUT_OF_MEMORY      4u
/** The session or resource is closed. */
#define PTYX_STATUS_CLOSED             6u
/** The operation would block. */
#define PTYX_STATUS_WOULD_BLOCK        7u
/** The supplied buffer is too small. */
#define PTYX_STATUS_BUFFER_TOO_SMALL   8u
/** Child process spawning failed. */
#define PTYX_STATUS_SPAWN_FAILED       9u
/** Native I/O failed. */
#define PTYX_STATUS_IO_FAILED          10u
/** Waiting for child exit failed. */
#define PTYX_STATUS_WAIT_FAILED        11u
/** The operation timed out. */
#define PTYX_STATUS_TIMEOUT            12u
/** End of file was reached. */
#define PTYX_STATUS_EOF                14u
/** A pipe or PTY endpoint was closed by the peer. */
#define PTYX_STATUS_BROKEN_PIPE        15u
/** The operating system denied permission. */
#define PTYX_STATUS_PERMISSION_DENIED  16u
/** Another operation is already active for the same resource. */
#define PTYX_STATUS_BUSY               18u
/** The platform API returned an error. */
#define PTYX_STATUS_NATIVE_ERROR       19u

/** Inherit the parent environment and ignore env_items. */
#define PTYX_ENV_INHERIT               0u
/** Inherit the parent environment and apply env_items. */
#define PTYX_ENV_OVERLAY               1u
/** Clear the parent environment and apply env_items. */
#define PTYX_ENV_REPLACE               2u
/** Clear the parent environment and ignore env_items. */
#define PTYX_ENV_CLEAR                 3u

/** ptyx_term_mode_t::canonical is valid. */
#define PTYX_TERM_MODE_CANONICAL_VALID     (UINT32_C(1) << 0)
/** ptyx_term_mode_t::echo is valid. */
#define PTYX_TERM_MODE_ECHO_VALID          (UINT32_C(1) << 1)
/** ptyx_term_mode_t::signals is valid. */
#define PTYX_TERM_MODE_SIGNALS_VALID       (UINT32_C(1) << 2)

/** Post output as Dart external typed data when supported. */
#define PTYX_SESSION_OUTPUT_EXTERNAL_TYPED_DATA (UINT32_C(1) << 0)
/** Emit terminal mode events on the event port. */
#define PTYX_SESSION_ENABLE_MODE_EVENTS         (UINT32_C(1) << 1)
/** Require ptyx_ack_output() calls for native output backpressure. */
#define PTYX_SESSION_REQUIRE_OUTPUT_ACKS        (UINT32_C(1) << 2)

/** Output-port message carrying bytes. */
#define PTYX_MESSAGE_OUTPUT             1u
/** Output-port message indicating native output EOF. */
#define PTYX_MESSAGE_CLOSED             2u

/** Event-port message carrying the child exit code. */
#define PTYX_EVENT_EXIT                 1u
/** Event-port message carrying an error source, status, and message. */
#define PTYX_EVENT_ERROR                2u
/** Event-port message carrying terminal mode state. */
#define PTYX_EVENT_TERM_MODE            3u

/** Error originated in the output reader. */
#define PTYX_ERROR_SOURCE_OUTPUT        1u
/** Error originated in the input writer. */
#define PTYX_ERROR_SOURCE_WRITE         2u
/** Error originated while waiting for child exit. */
#define PTYX_ERROR_SOURCE_WAIT          3u
/** Error originated while reading terminal mode. */
#define PTYX_ERROR_SOURCE_MODE          4u

/** Opaque PTY session handle. */
typedef struct ptyx_session ptyx_session_t;

/** Opaque caller-filled buffer used by ptyx_write_owned(). */
typedef struct ptyx_owned_buffer ptyx_owned_buffer_t;

/**
 * Borrowed byte string.
 *
 * Strings are UTF-8 where text is required. The buffer does not need to be
 * NUL-terminated and must remain valid for the duration of the call that
 * receives it.
 */
typedef struct ptyx_string {
  /** Pointer to the first byte, or NULL when len is zero. */
  const char* data;
  /** Number of bytes at data. */
  size_t len;
} ptyx_string_t;

/** PTY cell and pixel size. */
typedef struct ptyx_size {
  /** Number of terminal rows. */
  uint32_t rows;
  /** Number of terminal columns. */
  uint32_t columns;
  /** Width in pixels, or zero when unknown. */
  uint32_t pixel_width;
  /** Height in pixels, or zero when unknown. */
  uint32_t pixel_height;
} ptyx_size_t;

/** Snapshot of observable terminal input mode flags. */
typedef struct ptyx_term_mode {
  /** Bitset of PTYX_TERM_MODE_*_VALID flags. */
  ptyx_flags_t valid_fields;
  /** Canonical input processing state, valid when CANONICAL_VALID is set. */
  bool canonical;
  /** Input echo state, valid when ECHO_VALID is set. */
  bool echo;
  /** Terminal signal generation state, valid when SIGNALS_VALID is set. */
  bool signals;
} ptyx_term_mode_t;

/** Options for ptyx_spawn(). */
typedef struct ptyx_session_options {
  /** Bitset of PTYX_SESSION_* flags. */
  ptyx_flags_t flags;
  /** Executable path or command name. Required. */
  ptyx_string_t executable;
  /** Argument array, not including executable. */
  const ptyx_string_t* argv;
  /** Number of entries in argv. */
  size_t argc;
  /**
   * Environment entries as KEY=VALUE strings.
   *
   * KEY must not be empty or contain NUL. VALUE must not contain NUL and may
   * contain '='.
   */
  const ptyx_string_t* env_items;
  /** Number of entries in env_items. */
  size_t env_count;
  /** Environment handling mode. */
  ptyx_env_mode_t env_mode;
  /** Working directory, or empty to inherit the parent working directory. */
  ptyx_string_t cwd;
  /** Initial PTY size. */
  ptyx_size_t initial_size;
  /** Dart native port that receives PTYX_MESSAGE_* output messages. */
  int64_t output_port;
  /** Dart native port that receives PTYX_EVENT_* messages. */
  int64_t event_port;
  /** Native read buffer size in bytes. Zero selects the default. */
  uint32_t read_buffer_size;
  /** Maximum bytes per posted output batch. Zero selects the default. */
  uint32_t output_batch_max_bytes;
  /** Maximum batching delay in microseconds. Zero selects the default. */
  uint32_t output_batch_max_delay_us;
  /** Terminal mode polling interval in milliseconds. Zero selects the default. */
  uint32_t mode_poll_interval_ms;
  /** Maximum unacknowledged output bytes. Zero selects the default. */
  uint64_t max_inflight_bytes;
  /** Maximum outstanding external typed-data bytes. Zero selects the default. */
  uint64_t max_external_output_bytes;
  /** Maximum queued input bytes. Zero selects the default. */
  uint64_t write_queue_max_bytes;
} ptyx_session_options_t;

/** Returns the ABI major version implemented by the loaded library. */
PTYX_API uint32_t PTYX_CALL ptyx_abi_version_major(void);

/** Returns the ABI minor version implemented by the loaded library. */
PTYX_API uint32_t PTYX_CALL ptyx_abi_version_minor(void);

/**
 * Returns a static string for a status code.
 *
 * @param status Status code to describe.
 * @return Static NUL-terminated string. The caller must not free it.
 */
PTYX_API const char* PTYX_CALL ptyx_status_string(ptyx_status_t status);

/**
 * Initializes session options with defaults.
 *
 * @param options Options struct to initialize. NULL is ignored.
 */
PTYX_API void PTYX_CALL ptyx_session_options_init(ptyx_session_options_t* options);

/**
 * Initializes Dart native API access for the library.
 *
 * @param dart_initialize_api_dl_data Pointer provided by Dart_InitializeApiDL.
 * @return PTYX_STATUS_OK or an error status.
 */
PTYX_API ptyx_status_t PTYX_CALL ptyx_init(void* dart_initialize_api_dl_data);

/**
 * Spawns a child process attached to a new PTY.
 *
 * @param options Session options. The pointed-to data only needs to remain
 * valid for the duration of this call.
 * @param out_session Receives the new session handle on success.
 * @return PTYX_STATUS_OK or an error status.
 */
PTYX_API ptyx_status_t PTYX_CALL ptyx_spawn(
  const ptyx_session_options_t* options,
  ptyx_session_t** out_session
);

/**
 * Queues bytes for the child process input.
 *
 * @param session Session handle.
 * @param data Bytes to write. May be NULL only when length is zero.
 * @param length Number of bytes to write.
 * @return PTYX_STATUS_OK or an error status.
 */
PTYX_API ptyx_status_t PTYX_CALL ptyx_write(
  ptyx_session_t* session,
  const uint8_t* data,
  size_t length
);

/**
 * Allocates a native buffer for ptyx_write_owned().
 *
 * @param capacity Buffer capacity in bytes.
 * @param out_buffer Receives the allocated buffer.
 * @return PTYX_STATUS_OK or an error status.
 */
PTYX_API ptyx_status_t PTYX_CALL ptyx_buffer_alloc(
  size_t capacity,
  ptyx_owned_buffer_t** out_buffer
);

/**
 * Returns the writable data pointer for an owned buffer.
 *
 * @param buffer Buffer handle.
 * @return Pointer to capacity bytes, or NULL for an invalid buffer.
 */
PTYX_API uint8_t* PTYX_CALL ptyx_buffer_data(ptyx_owned_buffer_t* buffer);

/**
 * Frees an owned buffer.
 *
 * @param buffer Buffer handle. NULL is allowed.
 */
PTYX_API void PTYX_CALL ptyx_buffer_free(ptyx_owned_buffer_t* buffer);

/**
 * Queues an owned buffer for the child process input.
 *
 * On success, ownership of buffer moves to the session. On failure, ownership
 * remains with the caller and the buffer must be freed with ptyx_buffer_free().
 *
 * @param session Session handle.
 * @param buffer Buffer previously allocated by ptyx_buffer_alloc().
 * @param length Number of initialized bytes in buffer.
 * @return PTYX_STATUS_OK or an error status.
 */
PTYX_API ptyx_status_t PTYX_CALL ptyx_write_owned(
  ptyx_session_t* session,
  ptyx_owned_buffer_t* buffer,
  size_t length
);

/**
 * Acknowledges output bytes delivered to the caller.
 *
 * This is required when PTYX_SESSION_REQUIRE_OUTPUT_ACKS is set. Acknowledged
 * bytes release native output backpressure.
 *
 * @param session Session handle.
 * @param byte_count Number of output bytes accepted or discarded by the caller.
 * @return PTYX_STATUS_OK or an error status.
 */
PTYX_API ptyx_status_t PTYX_CALL ptyx_ack_output(ptyx_session_t* session, uint64_t byte_count);

/**
 * Resizes the PTY.
 *
 * @param session Session handle.
 * @param size New PTY size.
 * @return PTYX_STATUS_OK or an error status.
 */
PTYX_API ptyx_status_t PTYX_CALL ptyx_resize(ptyx_session_t* session, ptyx_size_t size);

/**
 * Reads the current PTY size.
 *
 * @param session Session handle.
 * @param out_size Receives the PTY size.
 * @return PTYX_STATUS_OK or an error status.
 */
PTYX_API ptyx_status_t PTYX_CALL ptyx_get_size(ptyx_session_t* session, ptyx_size_t* out_size);

/**
 * Reads the current observable terminal mode.
 *
 * @param session Session handle.
 * @param out_mode Receives the terminal mode snapshot.
 * @return PTYX_STATUS_OK or an error status.
 */
PTYX_API ptyx_status_t PTYX_CALL ptyx_get_term_mode(
  ptyx_session_t* session,
  ptyx_term_mode_t* out_mode
);

/**
 * Reads the child process ID.
 *
 * @param session Session handle.
 * @param out_pid Receives the process ID.
 * @return PTYX_STATUS_OK, PTYX_STATUS_UNSUPPORTED, or an error status.
 */
PTYX_API ptyx_status_t PTYX_CALL ptyx_get_child_pid(
  ptyx_session_t* session,
  uint64_t* out_pid
);

/**
 * Reads the PTY device name.
 *
 * When buffer is NULL or too small, inout_len receives the required byte count,
 * including the trailing NUL, and PTYX_STATUS_BUFFER_TOO_SMALL is returned.
 *
 * @param session Session handle.
 * @param buffer Destination buffer, or NULL to query the required size.
 * @param inout_len On input, buffer capacity. On output, required or written
 * byte count including the trailing NUL.
 * @return PTYX_STATUS_OK, PTYX_STATUS_UNSUPPORTED,
 * PTYX_STATUS_BUFFER_TOO_SMALL, or an error status.
 */
PTYX_API ptyx_status_t PTYX_CALL ptyx_get_tty_name(
  ptyx_session_t* session,
  char* buffer,
  size_t* inout_len
);

/**
 * Sends a signal to the child process.
 *
 * @param session Session handle.
 * @param signal Platform signal number. Ignored on platforms without signals.
 * @return true when a live child was signaled, false otherwise.
 */
PTYX_API bool PTYX_CALL ptyx_kill(ptyx_session_t* session, int32_t signal);

/**
 * Closes and frees a session.
 *
 * The session pointer is invalid after this call. NULL is allowed.
 *
 * @param session Session handle.
 */
PTYX_API void PTYX_CALL ptyx_close(ptyx_session_t* session);

/**
 * Returns the last error message for the current thread.
 *
 * @return Thread-local NUL-terminated string. The pointer remains valid until
 * the next fallible ptyx call on the same thread.
 */
PTYX_API const char* PTYX_CALL ptyx_last_error_message(void);

#ifdef __cplusplus
}
#endif

#endif
