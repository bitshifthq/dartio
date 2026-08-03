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

#include <ptyx/ptyx.h>

#ifdef __cplusplus
extern "C" {
#endif

/** Generation-checked identity of one Dart event-pump adapter. */
typedef uint64_t PtydAdapter;

/** Invalid or empty Dart adapter handle. */
#define PTYD_INVALID_ADAPTER UINT64_C(0)

/**
 * @brief Initializes Dart API-DL for the current library artifact.
 *
 * @param[in] api_data Dart NativeApi.initializeApiDLData.
 * @return PTYX_STATUS_OK or PTYX_STATUS_INVALID_ARGUMENT.
 */
PTYX_EXPORT PtyxStatus PTYX_CALL ptyd_initialize(void *api_data);

/**
 * @brief Attaches the sole event pump and cleanup owner to a runtime.
 *
 * @param[in] runtime Live runtime transferred to the adapter on success.
 * @param[in] port Dart native port receiving event arrays.
 * @param[out] adapter Receives the adapter handle.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK or a typed failure.
 *
 * Each message is a nine-element Dart array containing kind, session, token,
 * flags, value, error domain, error kind, native error code, and nullable
 * Uint8List data, in that order.
 * The adapter owns output-token registration and release. The Dart side keeps
 * only a fixed message-shape guard for ABI safety; unknown or malformed port
 * messages converge through the adapter abort operation.
 * Output remains bounded by each session's configured native output capacity.
 * Withholding an acknowledgement applies backpressure only to that session;
 * the shared runtime continues fairly delivering other sessions' events.
 *
 * After success, detach or finalization owns runtime shutdown and release.
 */
PTYX_EXPORT PtyxStatus PTYX_CALL ptyd_runtime_attach(PtyxRuntime runtime,
                                                     int64_t port,
                                                     PtydAdapter *adapter,
                                                     PtyxError *error);

/**
 * @brief Returns capabilities for an attached Dart runtime adapter.
 *
 * @param[in] adapter Attached runtime adapter.
 * @param[out] capabilities Receives PTYX_CAPABILITY_* bits.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK, PTYX_STATUS_STALE_HANDLE, or a typed failure.
 */
PTYX_EXPORT PtyxStatus PTYX_CALL ptyd_runtime_capabilities(
    PtydAdapter adapter, uint32_t *capabilities, PtyxError *error);

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
PTYX_EXPORT PtyxStatus PTYX_CALL ptyd_session_spawn_start(
    PtydAdapter adapter, const PtyxSpawnOptions *options,
    PtyxSession *session, PtyxError *error);

/**
 * @brief Explicitly abandons and releases a tracked session.
 *
 * @param[in] adapter Owning adapter.
 * @param[in,out] session Tracked handle, cleared on successful or stale
 * release.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK or a typed failure. A failure leaves the session
 * tracked and its ownership available for a later release attempt.
 *
 * A busy or internal result leaves ownership tracked so the native cleanup
 * worker can retry it.
 */
PTYX_EXPORT PtyxStatus PTYX_CALL ptyd_session_release(
    PtydAdapter adapter, PtyxSession *session, PtyxError *error);

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
PTYX_EXPORT PtyxStatus PTYX_CALL ptyd_event_ack(PtydAdapter adapter,
                                                PtyxEventToken token,
                                                PtyxError *error);

/**
 * @brief Stops the pump and releases every resource transferred to it.
 *
 * Shutdown wakes the blocked event read. The function joins the pump, releases
 * outstanding events and tracked sessions, shuts down and releases the
 * runtime, and clears adapter. Passing an already-zero handle succeeds.
 *
 * @param[in,out] adapter Adapter handle, cleared on success.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK or a typed failure. A failure leaves the adapter
 * registered so the caller can retry cleanup.
 */
PTYX_EXPORT PtyxStatus PTYX_CALL ptyd_runtime_detach(PtydAdapter *adapter,
                                                     PtyxError *error);

/**
 * @brief Aborts a Dart adapter after a protocol or infrastructure failure.
 *
 * Abort uses the same idempotent ownership path as detach. Outstanding event
 * leases, tracked sessions, and the C runtime remain registered when cleanup
 * reports a busy or internal result. The native cleanup worker retries those
 * resources without requiring a Dart isolate.
 *
 * @param[in,out] adapter Adapter handle, cleared only after complete cleanup.
 * @param[out] error Optional initialized error destination.
 * @return PTYX_STATUS_OK or a typed failure.
 */
PTYX_EXPORT PtyxStatus PTYX_CALL ptyd_runtime_abort(PtydAdapter *adapter,
                                                    PtyxError *error);

/**
 * @brief Native finalizer for an adapter handle encoded as a pointer address.
 *
 * Transfers ownership to a process-wide cleanup worker and returns without
 * waiting. The worker performs the same cleanup as detach and ignores
 * diagnostic output because no Dart owner remains.
 *
 * @param[in] token Adapter handle encoded as a pointer-sized integer.
 * @return Nothing.
 */
PTYX_EXPORT void PTYX_CALL ptyd_runtime_finalize(void *token);

/**
 * @brief Native finalizer for a session handle encoded as a pointer address.
 *
 * Transfers ownership to the process-wide cleanup worker and returns without
 * waiting. Cleanup is idempotent and generation checked, so it is safe when
 * explicit close or adapter teardown races finalization.
 *
 * @param[in] token Session handle encoded as a pointer-sized integer.
 * @return Nothing.
 */
PTYX_EXPORT void PTYX_CALL ptyd_session_finalize(void *token);

#ifdef __cplusplus
}
#endif

#endif
