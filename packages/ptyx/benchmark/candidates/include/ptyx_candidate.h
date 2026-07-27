#ifndef PTYX_BENCHMARK_CANDIDATE_H_
#define PTYX_BENCHMARK_CANDIDATE_H_

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32)
#define PTYX_CANDIDATE_EXPORT __declspec(dllexport)
#define PTYX_CANDIDATE_CALL __cdecl
#else
#define PTYX_CANDIDATE_EXPORT __attribute__((visibility("default")))
#define PTYX_CANDIDATE_CALL
#endif

#ifdef __cplusplus
extern "C" {
#endif

typedef uint64_t ptyx_candidate_handle_t;

enum {
  PTYX_CANDIDATE_OK = 0,
  PTYX_CANDIDATE_INVALID_ARGUMENT = 1,
  PTYX_CANDIDATE_OS_ERROR = 2,
  PTYX_CANDIDATE_STALE_HANDLE = 3,
};

PTYX_CANDIDATE_EXPORT uint32_t PTYX_CANDIDATE_CALL
ptyx_candidate_abi(void);

PTYX_CANDIDATE_EXPORT int32_t PTYX_CANDIDATE_CALL
ptyx_candidate_spawn(
    const uint8_t* script,
    size_t script_length,
    ptyx_candidate_handle_t* out_handle);

PTYX_CANDIDATE_EXPORT int64_t PTYX_CANDIDATE_CALL
ptyx_candidate_read(
    ptyx_candidate_handle_t handle,
    uint8_t* bytes,
    size_t capacity);

PTYX_CANDIDATE_EXPORT int64_t PTYX_CANDIDATE_CALL
ptyx_candidate_write(
    ptyx_candidate_handle_t handle,
    const uint8_t* bytes,
    size_t length);

PTYX_CANDIDATE_EXPORT int32_t PTYX_CANDIDATE_CALL
ptyx_candidate_wait(
    ptyx_candidate_handle_t handle,
    int32_t* out_exit_code);

PTYX_CANDIDATE_EXPORT int32_t PTYX_CANDIDATE_CALL
ptyx_candidate_close(ptyx_candidate_handle_t handle);

PTYX_CANDIDATE_EXPORT int32_t PTYX_CANDIDATE_CALL
ptyx_candidate_last_os_error(void);

#ifdef __cplusplus
}
#endif

#endif
