/**
 * @file ptyx_dart.h
 * @brief Private Dart isolate adapter for the ptyx C ABI.
 *
 * This interface is versioned with the Dart package, not the stable ptyx C
 * ABI. It depends on Dart API-DL at runtime but keeps Dart headers and ports
 * out of the reusable PTY engine and language-neutral ABI.
 */
#ifndef PTYX_DART_H
#define PTYX_DART_H

#include "ptyx.h"

#ifdef __cplusplus
extern "C" {
#endif

/** Generation-checked identity of one Dart event-pump adapter. */
typedef uint64_t ptyd_adapter_t;

/** Invalid or empty Dart adapter handle. */
#define PTYD_INVALID_ADAPTER UINT64_C(0)

/**
 * @brief Initializes Dart API-DL for the current library artifact.
 *
 * @param[in] api_data Dart NativeApi.initializeApiDLData.
 * @return PTYX_STATUS_OK or PTYX_STATUS_INVALID_ARGUMENT.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL ptyd_initialize(void *api_data);

/**
 * @brief Attaches the sole event pump and cleanup owner to a runtime.
 *
 * @param[in] runtime Live runtime transferred to the adapter on success.
 * @param[in] port Dart native port receiving event arrays.
 * @param[out] adapter Receives the adapter handle.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK or a typed failure.
 *
 * Each message is an eleven-element Dart array containing kind, session,
 * token, flags, value, error domain, error kind, error operation, native error
 * code, error flags, and nullable Uint8List data, in that order.
 * Output remains bounded by each session's configured native output capacity.
 * Withholding an acknowledgement applies backpressure only to that session;
 * the shared runtime continues fairly delivering other sessions' events.
 *
 * After success, detach or finalization owns runtime shutdown and release.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL ptyd_runtime_attach(ptyx_runtime_t runtime,
                                                        int64_t port,
                                                        ptyd_adapter_t *adapter,
                                                        ptyx_error_t *error);

/**
 * @brief Atomically admits and transfers a session spawn to the adapter.
 *
 * The pump cannot process SPAWN_READY or SPAWN_FAILED before it owns the
 * returned handle. The adapter releases the handle after SPAWN_FAILED or
 * CLOSE_COMPLETE, or during detach.
 *
 * @param[in] adapter Owning adapter.
 * @param[in] options Validated, borrowed spawn options.
 * @param[out] session Receives the tracked session handle.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK or a typed failure.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL ptyd_session_spawn_start(
    ptyd_adapter_t adapter, const ptyx_spawn_options_t *options,
    ptyx_session_t *session, ptyx_error_t *error);

/**
 * @brief Explicitly abandons and releases a tracked session.
 *
 * @param[in] adapter Owning adapter.
 * @param[in,out] session Tracked handle, cleared on success.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK or a typed failure.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL ptyd_session_release(
    ptyd_adapter_t adapter, ptyx_session_t *session, ptyx_error_t *error);

/**
 * @brief Acknowledges one output event after Dart consumes its copied bytes.
 *
 * Acknowledgement releases the C event and returns native output credit. Each
 * nonzero token must be acknowledged exactly once.
 *
 * @param[in] adapter Owning adapter.
 * @param[in] token Nonzero output-event token.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK or a typed failure.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL ptyd_event_ack(ptyd_adapter_t adapter,
                                                   ptyx_event_token_t token,
                                                   ptyx_error_t *error);

/**
 * @brief Stops the pump and releases every resource transferred to it.
 *
 * Shutdown wakes the blocked event read. The function joins the pump, releases
 * outstanding events and tracked sessions, shuts down and releases the
 * runtime, and clears adapter. Passing an already-zero handle succeeds.
 *
 * @param[in,out] adapter Adapter handle, cleared on success.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK or a typed failure.
 */
PTYX_EXPORT ptyx_status_t PTYX_CALL ptyd_runtime_detach(ptyd_adapter_t *adapter,
                                                        ptyx_error_t *error);

/**
 * @brief Native finalizer for an adapter handle encoded as a pointer address.
 *
 * Transfers ownership to a process-wide cleanup worker and returns without
 * waiting. The worker performs the same cleanup as detach and ignores
 * diagnostic output because no Dart owner remains.
 *
 * @param[in] token Adapter handle encoded as a pointer-sized integer.
 */
PTYX_EXPORT void PTYX_CALL ptyd_runtime_finalize(void *token);

#if defined(PTYX_TEST_CONTROLS)
/** Forces the next Dart port post to fail in diagnostic builds. */
PTYX_EXPORT void PTYX_CALL ptyd_test_fail_next_post(void);
/**
 * Terminates the Unix broker in diagnostic builds.
 *
 * @return One when a broker was terminated; otherwise zero.
 */
PTYX_EXPORT uint32_t PTYX_CALL ptyd_test_kill_broker(void);
/**
 * Delays the next spawn worker in diagnostic builds.
 *
 * @param[in] milliseconds Delay duration.
 */
PTYX_EXPORT void PTYX_CALL ptyd_test_delay_next_spawn(uint64_t milliseconds);
/**
 * Reports whether the diagnostic spawn delay is active.
 *
 * @return One while a delayed worker is active; otherwise zero.
 */
PTYX_EXPORT uint32_t PTYX_CALL ptyd_test_spawn_delay_active(void);
/** Forces the next admitted write to fail in diagnostic builds. */
PTYX_EXPORT void PTYX_CALL ptyd_test_fail_next_write(void);
#endif

#ifdef __cplusplus
}
#endif

#endif
