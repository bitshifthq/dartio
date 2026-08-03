/**
 * @file ptyx.h
 * @brief Stable, language-neutral C interface to the ptyx PTY engine.
 *
 * The ABI never invokes caller callbacks. Except for
 * ptyx_runtime_next_event(), functions do not wait for PTY I/O or queue
 * capacity. All caller-owned input is copied before a function returns.
 */
#ifndef PTYX_H
#define PTYX_H

#include <stdint.h>

#if UINTPTR_MAX != UINT64_MAX
#error "ptyx requires a 64-bit target"
#endif

#if defined(_WIN32)
#if defined(PTYX_STATIC)
#define PTYX_EXPORT
#elif defined(PTYX_BUILDING_LIBRARY)
#define PTYX_EXPORT __declspec(dllexport)
#else
#define PTYX_EXPORT __declspec(dllimport)
#endif
#define PTYX_CALL __cdecl
#else
#define PTYX_EXPORT __attribute__((visibility("default")))
#define PTYX_CALL
#endif

#ifdef __cplusplus
extern "C" {
#endif

/** @defgroup ptyx_version Version and capabilities */
/** @{ */

/** ABI major version. */
#define PTYX_ABI_VERSION_MAJOR UINT32_C(0)
/** ABI minor version. */
#define PTYX_ABI_VERSION_MINOR UINT32_C(4)
/** Packed ABI version returned by ptyx_abi_version(). */
#define PTYX_ABI_VERSION                                                       \
  ((PTYX_ABI_VERSION_MAJOR << UINT32_C(16)) | PTYX_ABI_VERSION_MINOR)

/**
 * While the ABI major is zero, callers must require the exact packed version;
 * development releases may replace layouts without a compatibility shim.
 * Beginning with ABI 1.0, major changes are incompatible and minor releases
 * may add symbols, constants, event kinds, or trailing structure fields.
 * Callers must ignore unknown event kinds after releasing the event and must
 * initialize caller-sized structures to zero so a newer library can inspect
 * struct_size safely.
 *
 * ABI 0 requires the complete structure layout for the exact packed version.
 * Prefix-compatible fieldwise access and old-caller/new-library tests are
 * required before declaring ABI 1.0.
 */
/** Generation-checked runtime identity. */
typedef uint64_t ptyx_runtime_t;
/** Generation-checked session identity. */
typedef uint64_t ptyx_session_t;
/** Generation-checked owning event identity. */
typedef uint64_t ptyx_event_token_t;

/** Invalid or empty runtime handle. */
#define PTYX_INVALID_RUNTIME UINT64_C(0)
/** Invalid or empty session handle. */
#define PTYX_INVALID_SESSION UINT64_C(0)
/** Event value that owns no output lease. */
#define PTYX_INVALID_EVENT_TOKEN UINT64_C(0)

/** Stable result of a ptyx operation. */
typedef enum ptyx_status {
  /** Operation completed successfully. */
  PTYX_STATUS_OK = 0,
  /** An argument or caller-sized structure is invalid. */
  PTYX_STATUS_INVALID_ARGUMENT = 1,
  /** A generation-checked handle or token is stale. */
  PTYX_STATUS_STALE_HANDLE = 2,
  /** The operation is invalid in the current lifecycle state. */
  PTYX_STATUS_WRONG_STATE = 3,
  /** Bounded input storage cannot accept the complete write now. */
  PTYX_STATUS_BACKPRESSURE = 4,
  /** The requested capability is unavailable. */
  PTYX_STATUS_UNSUPPORTED = 5,
  /** The requested direction or session is closed. */
  PTYX_STATUS_CLOSED = 6,
  /** The blocking event source reached its terminal state. */
  PTYX_STATUS_END_OF_STREAM = 7,
  /** Ownership is valid but temporarily prevents the operation. */
  PTYX_STATUS_BUSY = 8,
  /** An operating-system operation failed. */
  PTYX_STATUS_OS_ERROR = 9,
  /** Runtime infrastructure or an internal invariant failed. */
  PTYX_STATUS_INTERNAL = 10,
  /** Caller storage is smaller than the reported required length. */
  PTYX_STATUS_BUFFER_TOO_SMALL = 11,
  /** Reserved value that fixes the public enum representation at 32 bits. */
  PTYX_STATUS_ENUM_FORCE_32_BIT = INT32_MAX
} ptyx_status_t;

/** Unix signal delivery is available. */
#define PTYX_CAPABILITY_SIGNALS UINT32_C(1)
/** Unix process-group ownership is available. */
#define PTYX_CAPABILITY_PROCESS_GROUPS UINT32_C(2)
/** Terminal-mode queries and observation are available. */
#define PTYX_CAPABILITY_TERMINAL_MODES UINT32_C(4)
/** The Windows ConPTY backend is active. */
#define PTYX_CAPABILITY_CONPTY UINT32_C(8)
/** A stable terminal device name is available. */
#define PTYX_CAPABILITY_TERMINAL_NAME UINT32_C(16)

/**
 * @brief Returns the packed ABI major and minor version.
 *
 * @return `(major << 16) | minor`.
 *
 * @par Thread safety
 * Safe to call concurrently.
 */
PTYX_EXPORT uint32_t PTYX_CALL ptyx_abi_version(void);

/** @} */

/** @defgroup ptyx_errors Errors */
/** @{ */

/** Stable subsystem that reported an error. */
typedef enum ptyx_error_domain {
  /** No error domain. */
  PTYX_ERROR_DOMAIN_NONE = 0,
  /** Caller input validation failure. */
  PTYX_ERROR_DOMAIN_ARGUMENT = 1,
  /** Lifecycle or handle-state failure. */
  PTYX_ERROR_DOMAIN_STATE = 2,
  /** Terminal input direction failure. */
  PTYX_ERROR_DOMAIN_INPUT = 3,
  /** Terminal output direction failure. */
  PTYX_ERROR_DOMAIN_OUTPUT = 4,
  /** Child process operation failure. */
  PTYX_ERROR_DOMAIN_PROCESS = 5,
  /** Shared runtime infrastructure failure. */
  PTYX_ERROR_DOMAIN_RUNTIME = 6,
  /** Operating-system boundary failure. */
  PTYX_ERROR_DOMAIN_OS = 7,
  /** Reserved value that fixes the public enum representation at 32 bits. */
  PTYX_ERROR_DOMAIN_ENUM_FORCE_32_BIT = INT32_MAX
} ptyx_error_domain_t;

/** Stable category of an error. */
typedef enum ptyx_error_kind {
  /** No error kind. */
  PTYX_ERROR_NONE = 0,
  /** Invalid caller value or layout. */
  PTYX_ERROR_INVALID_ARGUMENT = 1,
  /** Retired or unknown generation-checked identity. */
  PTYX_ERROR_STALE_HANDLE = 2,
  /** Operation rejected by the current lifecycle state. */
  PTYX_ERROR_WRONG_STATE = 3,
  /** Bounded queue cannot accept the complete operation. */
  PTYX_ERROR_QUEUE_FULL = 4,
  /** Capability is unavailable on this backend. */
  PTYX_ERROR_UNSUPPORTED = 5,
  /** Session or direction is terminal. */
  PTYX_ERROR_CLOSED = 6,
  /** Native or operating-system operation failed. */
  PTYX_ERROR_NATIVE_FAILURE = 7,
  /** Runtime ownership or event infrastructure was lost. */
  PTYX_ERROR_INFRASTRUCTURE_LOST = 8,
  /** Reserved value that fixes the public enum representation at 32 bits. */
  PTYX_ERROR_KIND_ENUM_FORCE_32_BIT = INT32_MAX
} ptyx_error_kind_t;

/** Stable operation associated with an error. */
typedef enum ptyx_operation {
  /** No associated operation. */
  PTYX_OPERATION_NONE = 0,
  /** Runtime construction. */
  PTYX_OPERATION_RUNTIME_CREATE = 1,
  /** Runtime shutdown or release. */
  PTYX_OPERATION_RUNTIME_SHUTDOWN = 2,
  /** Child and terminal spawn. */
  PTYX_OPERATION_SPAWN = 3,
  /** Terminal input write. */
  PTYX_OPERATION_WRITE = 4,
  /** Terminal output delivery or cancellation. */
  PTYX_OPERATION_OUTPUT = 5,
  /** Terminal resize. */
  PTYX_OPERATION_RESIZE = 6,
  /** Child termination. */
  PTYX_OPERATION_TERMINATE = 7,
  /** Terminal size query. */
  PTYX_OPERATION_SIZE = 8,
  /** Direct-child process identifier query. */
  PTYX_OPERATION_PROCESS_ID = 9,
  /** Terminal mode query or observation. */
  PTYX_OPERATION_TERMINAL_MODE = 10,
  /** Controller terminal-name query. */
  PTYX_OPERATION_TERMINAL_NAME = 11,
  /** Session cleanup. */
  PTYX_OPERATION_CLOSE = 12,
  /** Direct-child exit-status observation. */
  PTYX_OPERATION_EXIT = 13,
  /** Reserved value that fixes the public enum representation at 32 bits. */
  PTYX_OPERATION_ENUM_FORCE_32_BIT = INT32_MAX
} ptyx_operation_t;

/**
 * @brief Stable value describing one failure.
 *
 * This structure owns no pointers. Unknown trailing fields are reserved for
 * compatible ABI growth. Initialize the complete structure to zero and set
 * struct_size before passing it to ptyx.
 */
typedef struct ptyx_error {
  uint32_t struct_size;       /**< Caller-visible structure size. */
  ptyx_error_domain_t domain; /**< PTYX_ERROR_DOMAIN_* value. */
  ptyx_error_kind_t kind;     /**< PTYX_ERROR_* value. */
  ptyx_operation_t operation; /**< PTYX_OPERATION_* value. */
  int32_t native_code;        /**< Optional errno or Win32 status, or zero. */
  uint32_t flags;             /**< Reserved error-detail bits. */
  uint64_t context[2];        /**< Bounded non-secret numeric context. */
  uint64_t reserved[3];       /**< Must be zero. */
} ptyx_error_t;

/**
 * @brief Formats an error into caller-owned UTF-8 storage.
 *
 * @param[in] error Error value to format.
 * @param[out] target Destination bytes, or NULL to query the required size.
 * @param[in] capacity Writable bytes at target.
 * @param[out] required Receives the required byte count excluding a trailing
 * NUL. May be NULL.
 * @return PTYX_STATUS_OK when the complete message was written,
 * PTYX_STATUS_BUFFER_TOO_SMALL when capacity is insufficient, or
 * PTYX_STATUS_INVALID_ARGUMENT for an invalid structure or pointer pair.
 *
 * A successful write appends a NUL when capacity is greater than required.
 * No message contains spawn arguments, environment values, or input bytes.
 *
 * @par Thread safety
 * Safe to call concurrently.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL ptyx_error_format(const ptyx_error_t *error,
                                                      uint8_t *target,
                                                      uint64_t capacity,
                                                      uint64_t *required);

/** @} */

/** @defgroup ptyx_runtime Runtime */
/** @{ */

/** @brief Immutable borrowed byte sequence. */
typedef struct ptyx_bytes_view {
  const uint8_t *data; /**< Borrowed bytes, or NULL when length is zero. */
  uint64_t length;     /**< Number of readable bytes at data. */
} ptyx_bytes_view_t;

/**
 * @brief Runtime creation options.
 *
 * On Unix, an empty broker_path selects the integrity-checked broker bundled
 * with this library artifact. A nonempty path must name an executable broker
 * compatible with the library. Windows ignores broker_path. Set flags and
 * reserved fields to zero.
 */
typedef struct ptyx_runtime_options {
  uint32_t struct_size;          /**< Caller-visible structure size. */
  uint32_t flags;                /**< Must be zero. */
  ptyx_bytes_view_t broker_path; /**< Optional absolute Unix broker path. */
  uint64_t reserved[4];          /**< Must be zero. */
} ptyx_runtime_options_t;

/**
 * @brief Creates an isolated PTY runtime.
 *
 * @param[in] options Runtime options, or NULL for defaults.
 * @param[out] runtime Receives a nonzero generation handle on success.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK or a typed failure status.
 *
 * The operation is transactional. On failure, runtime receives
 * PTYX_INVALID_RUNTIME and no native worker remains owned by the caller.
 *
 * @par Blocking
 * May start native worker threads and a Unix broker.
 * @par Thread safety
 * Safe to call concurrently.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL
ptyx_runtime_create(const ptyx_runtime_options_t *options,
                    ptyx_runtime_t *runtime, ptyx_error_t *error);

/**
 * @brief Returns capabilities implemented by a runtime.
 *
 * @param[in] runtime Live runtime handle.
 * @param[out] capabilities Receives PTYX_CAPABILITY_* bits.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK or PTYX_STATUS_STALE_HANDLE.
 *
 * @par Thread safety
 * Safe to call concurrently.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL ptyx_runtime_capabilities(
    ptyx_runtime_t runtime, uint32_t *capabilities, ptyx_error_t *error);

struct ptyx_event;

/**
 * @brief Waits for and transfers ownership of the next fair runtime event.
 *
 * @param[in] runtime Live runtime handle.
 * @param[in,out] event Zero-initialized event with struct_size set.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK, PTYX_STATUS_END_OF_STREAM after shutdown, or a
 * typed failure status.
 *
 * Events are selected round-robin from per-session queues. Exactly one
 * logical consumer may call this function for a runtime. Runtime shutdown
 * wakes a blocked consumer. Passing an event that still owns a token returns
 * PTYX_STATUS_WRONG_STATE without changing the event.
 *
 * @par Blocking
 * Blocks until an event or runtime shutdown is available.
 * @par Thread safety
 * Other runtime and session functions may run concurrently.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL ptyx_runtime_next_event(
    ptyx_runtime_t runtime, struct ptyx_event *event, ptyx_error_t *error);

/**
 * @brief Starts deterministic shutdown and wakes the event consumer.
 *
 * @param[in] runtime Live runtime handle.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK when shutdown converged. Repeated calls succeed.
 *
 * @par Blocking
 * May wait for native owners and the Unix broker to terminate.
 * @par Thread safety
 * Do not race this function with another shutdown or release.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL
ptyx_runtime_shutdown(ptyx_runtime_t runtime, ptyx_error_t *error);

/**
 * @brief Releases a shut-down runtime handle.
 *
 * @param[in,out] runtime Live handle to release. Receives
 * PTYX_INVALID_RUNTIME on success.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK, PTYX_STATUS_BUSY when shutdown has not converged,
 * or a typed failure status. Passing an already-zero handle succeeds.
 *
 * All sessions and transferred events must be released first.
 *
 * @par Thread safety
 * Do not race release with any operation using this runtime.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL
ptyx_runtime_release(ptyx_runtime_t *runtime, ptyx_error_t *error);

/** @} */

/** @defgroup ptyx_sessions Sessions */
/** @{ */

/** Inherit the parent environment; environment_count must be zero. */
#define PTYX_SPAWN_INHERIT_ENVIRONMENT UINT32_C(1)
/** Terminal input uses canonical line buffering. */
#define PTYX_MODE_CANONICAL UINT32_C(1)
/** Terminal input is echoed. */
#define PTYX_MODE_ECHO UINT32_C(2)
/** Terminal signal-character processing is enabled. */
#define PTYX_MODE_SIGNALS UINT32_C(4)

/** @brief PTY size in cells and optional pixels. */
typedef struct ptyx_size {
  uint32_t rows;         /**< Terminal rows in the range 1..=32767. */
  uint32_t columns;      /**< Terminal columns in the range 1..=32767. */
  uint32_t pixel_width;  /**< Optional pixel width in 0..=65535. */
  uint32_t pixel_height; /**< Optional pixel height in 0..=65535. */
} ptyx_size_t;

/**
 * @brief Spawn configuration.
 *
 * Arguments and environment entries are byte strings without embedded NUL.
 * Unix preserves their native bytes. Windows requires UTF-8. Environment
 * entries use the platform-neutral `NAME=VALUE` form. Set reserved fields to
 * zero. PTYX_SPAWN_INHERIT_ENVIRONMENT requires environment_count to be zero;
 * without it, environment is the complete child environment.
 * At most 256 arguments and 4096 environment entries are accepted. The
 * complete native-encoded spawn payload must not exceed 64 KiB.
 * graceful_close_timeout_us controls Unix graceful-close escalation and must
 * not exceed 60000000. Windows accepts the value but begins Job Object
 * termination immediately because it has no portable graceful request.
 */
typedef struct ptyx_spawn_options {
  uint32_t struct_size;                 /**< Caller-visible structure size. */
  uint32_t flags;                       /**< PTYX_SPAWN_* bits. */
  ptyx_bytes_view_t executable;         /**< Executable path or lookup name. */
  const ptyx_bytes_view_t *arguments;   /**< Borrowed argument array. */
  uint64_t argument_count;              /**< Number of argument views. */
  const ptyx_bytes_view_t *environment; /**< Borrowed NAME=VALUE array. */
  uint64_t environment_count;           /**< Number of environment views. */
  ptyx_bytes_view_t working_directory;  /**< Directory, or empty for current. */
  ptyx_size_t size;                     /**< Initial terminal size. */
  uint64_t input_capacity;              /**< Bounded accepted input bytes. */
  uint64_t output_capacity;             /**< Bounded retained output bytes. */
  uint64_t graceful_close_timeout_us;   /**< Unix TERM-to-force interval. */
  uint64_t reserved[4];                 /**< Must be zero. */
} ptyx_spawn_options_t;

/**
 * @brief Starts asynchronous PTY session creation.
 *
 * @param[in] runtime Live runtime handle.
 * @param[in] options Complete spawn options.
 * @param[out] session Receives a nonzero generation handle on success.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK after native ownership of the request is established,
 * or a typed failure status.
 *
 * Every caller-owned byte is validated and copied before return. Exactly one
 * PTYX_EVENT_SPAWN_READY or PTYX_EVENT_SPAWN_FAILED event follows successful
 * request admission. Releasing the returned handle before readiness cancels
 * or abandons the transaction without leaking a child.
 *
 * @par Blocking
 * Does not wait for process creation, broker I/O, or queue capacity.
 * @par Thread safety
 * Safe to call concurrently.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL ptyx_session_spawn_start(
    ptyx_runtime_t runtime, const ptyx_spawn_options_t *options,
    ptyx_session_t *session, ptyx_error_t *error);

/**
 * @brief Accepts a complete input buffer or none of it.
 *
 * @param[in] session Live session handle.
 * @param[in] bytes Input bytes. May be NULL only when length is zero.
 * @param[in] length Number of input bytes.
 * @param[in,out] error Optional initialized error destination. Receives a
 * value on failure and remains unchanged on success so the input hot path
 * performs no error-structure stores.
 * @return PTYX_STATUS_OK after bounded native admission,
 * PTYX_STATUS_BACKPRESSURE when bounded admission is temporarily unavailable
 * because of capacity, entry, channel, or concurrent-admission contention, or
 * a terminal failure status.
 *
 * Returning success permits immediate reuse or release of caller storage.
 * Success does not mean the child consumed the bytes.
 *
 * @par Thread safety
 * Safe to call concurrently. Accepted writes are FIFO by successful
 * admission linearization order.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL ptyx_session_write(ptyx_session_t session,
                                                       const uint8_t *bytes,
                                                       uint64_t length,
                                                       ptyx_error_t *error);

/**
 * @brief Commits drain-and-discard for buffered and future output.
 *
 * @param[in] session Live session handle.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK or a typed failure status.
 *
 * This operation does not release output events already transferred to the
 * caller. Repeating a successful cancellation for the same live session
 * succeeds without changing state.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL
ptyx_session_cancel_output(ptyx_session_t session, ptyx_error_t *error);

/**
 * @brief Resizes a live PTY.
 *
 * @param[in] session Live session handle.
 * @param[in] size Non-NULL validated size.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK or a typed failure status.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL ptyx_session_resize(ptyx_session_t session,
                                                        const ptyx_size_t *size,
                                                        ptyx_error_t *error);

/**
 * @brief Requests termination of the owned terminal job.
 *
 * @param[in] session Live session handle.
 * @param[in] signal Unix signal number. Windows ignores this value and
 * terminates the owned Job Object.
 * @param[out] delivered Receives one when termination was accepted and zero
 * when the direct child had already exited.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK, PTYX_STATUS_UNSUPPORTED, or a typed failure.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL
ptyx_session_terminate(ptyx_session_t session, int32_t signal,
                       uint32_t *delivered, ptyx_error_t *error);

/**
 * @brief Returns the current terminal dimensions.
 *
 * @param[in] session Live session handle.
 * @param[out] size Receives cell and pixel dimensions.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK or a typed query failure.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL ptyx_session_get_size(
    ptyx_session_t session, ptyx_size_t *size, ptyx_error_t *error);

/**
 * @brief Returns the direct-child process identifier.
 *
 * @param[in] session Live session handle.
 * @param[out] pid Receives the identifier, or -1 when the backend has no
 * direct-child identifier.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK or a typed query failure.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL ptyx_session_get_child_pid(
    ptyx_session_t session, int64_t *pid, ptyx_error_t *error);

/**
 * @brief Returns terminal mode bits.
 *
 * @param[in] session Live session handle.
 * @param[out] mode Receives PTYX_MODE_* bits.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK, PTYX_STATUS_UNSUPPORTED when terminal modes are not
 * available, or a typed query failure.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL ptyx_session_get_term_mode(
    ptyx_session_t session, uint32_t *mode, ptyx_error_t *error);

/**
 * @brief Returns the controller terminal name in caller-owned storage.
 *
 * @param[in] session Live session handle.
 * @param[out] name Destination bytes, or NULL when capacity is zero.
 * @param[in] capacity Writable bytes at name.
 * @param[out] required Receives the byte count excluding a NUL terminator.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK, PTYX_STATUS_BUFFER_TOO_SMALL after writing required,
 * PTYX_STATUS_UNSUPPORTED when no stable terminal name exists, or a typed
 * query failure.
 *
 * Passing a null name with zero capacity performs a size query. The returned
 * bytes are not NUL terminated.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL ptyx_session_get_tty_name(
    ptyx_session_t session, uint8_t *name, uint64_t capacity,
    uint64_t *required, ptyx_error_t *error);

/**
 * @brief Enables or disables terminal mode-change observation.
 *
 * @param[in] session Live session handle.
 * @param[in] enabled One to observe changes or zero to stop observation.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK, PTYX_STATUS_UNSUPPORTED, or a typed failure.
 *
 * Observation is inactive by default. Enabling it produces distinct
 * PTYX_EVENT_MODE_CHANGED events. Disabling it stops native polling and does
 * not remove an event already transferred to the caller.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL ptyx_session_observe_mode(
    ptyx_session_t session, uint32_t enabled, ptyx_error_t *error);

/**
 * @brief Starts or joins session close.
 *
 * @param[in] session Live session handle.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK when close was accepted, or a typed failure.
 *
 * Exactly one PTYX_EVENT_CLOSE_COMPLETE event reports whether every accepted
 * input byte and native owner reached a known terminal state.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL ptyx_session_close(ptyx_session_t session,
                                                       ptyx_error_t *error);

/**
 * @brief Releases or abandons a session handle.
 *
 * @param[in,out] session Live handle to release. Receives
 * PTYX_INVALID_SESSION on success.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK or a typed failure. Passing an already-zero handle
 * succeeds.
 *
 * Release is nonblocking. If deterministic close has not converged, native
 * ownership continues cleanup independently.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL
ptyx_session_release(ptyx_session_t *session, ptyx_error_t *error);

/** @} */

/** @defgroup ptyx_events Events */
/** @{ */

/** Stable kind of an event transferred from a runtime. */
typedef enum ptyx_event_kind {
  /** Spawn committed and the session is ready for operations. */
  PTYX_EVENT_SPAWN_READY = 1,
  /** Spawn failed and its attempted ownership was reclaimed. */
  PTYX_EVENT_SPAWN_FAILED = 2,
  /** Ordered terminal output bytes with an owning event token. */
  PTYX_EVENT_OUTPUT = 3,
  /** Previously accepted terminal input could not be delivered. */
  PTYX_EVENT_INPUT_FAILED = 4,
  /** Terminal output ended with a native failure. */
  PTYX_EVENT_OUTPUT_FAILED = 5,
  /** Runtime or process ownership infrastructure was lost. */
  PTYX_EVENT_INFRASTRUCTURE_FAILED = 6,
  /** Every safely readable output byte was delivered. */
  PTYX_EVENT_OUTPUT_DONE = 7,
  /** The direct child exited. */
  PTYX_EVENT_EXIT = 8,
  /** Explicit close reached its terminal cleanup result. */
  PTYX_EVENT_CLOSE_COMPLETE = 9,
  /** An observed terminal input mode changed. */
  PTYX_EVENT_MODE_CHANGED = 10,
  /** Native terminal-mode observation failed and stopped. */
  PTYX_EVENT_MODE_FAILED = 11,
  /** Direct-child exit-status observation failed. */
  PTYX_EVENT_EXIT_FAILED = 12,
  /** Reserved value that fixes the public enum representation at 32 bits. */
  PTYX_EVENT_KIND_ENUM_FORCE_32_BIT = INT32_MAX
} ptyx_event_kind_t;

/** Close lost accepted input. */
#define PTYX_EVENT_CLOSE_INPUT_FAILED UINT32_C(1)
/** Close observed terminal output failure. */
#define PTYX_EVENT_CLOSE_OUTPUT_FAILED UINT32_C(2)
/** Close could not establish complete native cleanup. */
#define PTYX_EVENT_CLOSE_CLEANUP_FAILED UINT32_C(4)

/**
 * @brief One event transferred from a runtime.
 *
 * The kind determines which fields carry values:
 *
 * - PTYX_EVENT_SPAWN_READY has no additional value.
 * - PTYX_EVENT_SPAWN_FAILED carries error.
 * - PTYX_EVENT_OUTPUT carries immutable data, data_length, and an owning
 *   token.
 * - PTYX_EVENT_INPUT_FAILED, PTYX_EVENT_OUTPUT_FAILED,
 *   PTYX_EVENT_EXIT_FAILED, and PTYX_EVENT_INFRASTRUCTURE_FAILED carry error.
 * - PTYX_EVENT_OUTPUT_DONE has no additional value.
 * - PTYX_EVENT_EXIT carries the signed platform exit status in value.
 * - PTYX_EVENT_CLOSE_COMPLETE carries PTYX_EVENT_CLOSE_* bits in flags and
 *   the highest-priority retained failure in error.
 * - PTYX_EVENT_MODE_CHANGED carries PTYX_MODE_* bits in value.
 * - PTYX_EVENT_MODE_FAILED carries error.
 *
 * Every event identifies its session. Only PTYX_EVENT_OUTPUT has a nonzero
 * token. Its data remains valid until release.
 * Every successful next_event result must be released exactly once, including
 * events with an invalid token. Copying and releasing an owning event twice is
 * invalid.
 */
typedef struct ptyx_event {
  uint32_t struct_size;     /**< Caller-visible structure size. */
  ptyx_event_kind_t kind;   /**< PTYX_EVENT_* value. */
  uint32_t flags;           /**< Kind-specific PTYX_EVENT_* bits. */
  uint32_t reserved0;       /**< Must be zero. */
  ptyx_session_t session;   /**< Originating session or spawn request. */
  ptyx_event_token_t token; /**< Owning output token, or zero. */
  const uint8_t *data;      /**< Immutable output bytes until release. */
  uint64_t data_length;     /**< Number of readable bytes at data. */
  int64_t value;            /**< Kind-specific signed value. */
  ptyx_error_t error;       /**< Kind-specific value error. */
  uint64_t reserved[2];     /**< Must be zero. */
} ptyx_event_t;

/**
 * @brief Releases one transferred event and its output credit.
 *
 * @param[in,out] event Event returned by ptyx_runtime_next_event(). Every field
 * except the caller-provided struct_size is reset to zero on success so the
 * structure can be reused.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK, PTYX_STATUS_STALE_HANDLE for an invalid owning
 * token, or PTYX_STATUS_INVALID_ARGUMENT. A zero-initialized event succeeds.
 *
 * @par Thread safety
 * Safe to call from a different thread than event receipt.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL ptyx_event_release(ptyx_event_t *event,
                                                       ptyx_error_t *error);

/** @} */

#ifdef __cplusplus
}
#endif

#endif
