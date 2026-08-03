/**
 * @file ptyx_dart_test.h
 * @brief Test-only controls for the private Dart adapter.
 *
 * This header is not part of the published C ABI. It is included only by
 * diagnostic test builds and is intentionally separate from ptyx_dart.h.
 */
#ifndef PTYX_DART_TEST_H
#define PTYX_DART_TEST_H

#include <ptyx_dart.h>

#ifdef __cplusplus
extern "C" {
#endif

/** Forces the next Dart port post to fail. */
PTYX_EXPORT void PTYX_CALL ptyd_test_fail_next_post(void);
/** Terminates the Unix broker; returns one when a broker was terminated. */
PTYX_EXPORT uint32_t PTYX_CALL ptyd_test_kill_broker(void);
/** Delays the next spawn worker in diagnostic builds. */
PTYX_EXPORT void PTYX_CALL ptyd_test_delay_next_spawn(uint64_t milliseconds);
/** Returns one while the diagnostic spawn delay is active. */
PTYX_EXPORT uint32_t PTYX_CALL ptyd_test_spawn_delay_active(void);
/** Forces the next admitted write to fail. */
PTYX_EXPORT void PTYX_CALL ptyd_test_fail_next_write(void);
/** Injects a typed exit-observation failure. */
PTYX_EXPORT uint32_t PTYX_CALL ptyd_test_fail_exit_observation(void);
/** Delays the next runtime attachment in diagnostic builds. */
PTYX_EXPORT void PTYX_CALL ptyd_test_delay_next_attach(uint64_t milliseconds);
/** Returns one while attachment is delayed. */
PTYX_EXPORT uint32_t PTYX_CALL ptyd_test_attach_delay_active(void);
/** Returns the number of live Dart adapters. */
PTYX_EXPORT uint32_t PTYX_CALL ptyd_test_adapter_count(void);
/** Returns the number of sessions tracked by live Dart adapters. */
PTYX_EXPORT uint32_t PTYX_CALL ptyd_test_session_count(void);

#ifdef __cplusplus
}
#endif

#endif
