#include "dart_api_dl.h"

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32)
#define PTYX_HIDDEN
#else
#define PTYX_HIDDEN __attribute__((visibility("hidden")))
#endif

#ifdef _MSC_VER
#include <windows.h>
static volatile LONG fail_next_post = 0;

static bool take_failed_post(void) {
  return InterlockedExchange(&fail_next_post, 0) != 0;
}

PTYX_HIDDEN void ptyx_dart_test_fail_next_post(void) {
  InterlockedExchange(&fail_next_post, 1);
}
#else
#include <stdatomic.h>
static atomic_int fail_next_post = 0;

static bool take_failed_post(void) {
  return atomic_exchange(&fail_next_post, 0) != 0;
}

PTYX_HIDDEN void ptyx_dart_test_fail_next_post(void) {
  atomic_store(&fail_next_post, 1);
}
#endif

PTYX_HIDDEN bool ptyx_dart_post_event(
    Dart_Port_DL port, uint32_t kind, uint64_t session, uint64_t token,
    uint32_t flags, int64_t value, uint32_t error_domain, uint32_t error_kind,
    uint32_t error_operation, int32_t native_code, uint32_t error_flags,
    const uint8_t *bytes, intptr_t length) {
  if (take_failed_post()) {
    return false;
  }

  Dart_CObject fields[10];
  int64_t integers[10] = {
      (int64_t)kind,
      (int64_t)session,
      (int64_t)token,
      (int64_t)flags,
      value,
      (int64_t)error_domain,
      (int64_t)error_kind,
      (int64_t)error_operation,
      (int64_t)native_code,
      (int64_t)error_flags,
  };
  Dart_CObject *values[11];
  for (size_t index = 0; index < 10; index++) {
    fields[index].type = Dart_CObject_kInt64;
    fields[index].value.as_int64 = integers[index];
    values[index] = &fields[index];
  }

  Dart_CObject data = {
      .type = Dart_CObject_kNull,
  };
  if (bytes != NULL) {
    data.type = Dart_CObject_kTypedData;
    data.value.as_typed_data.type = Dart_TypedData_kUint8;
    data.value.as_typed_data.length = length;
    data.value.as_typed_data.values = (uint8_t *)bytes;
  }
  values[10] = &data;

  Dart_CObject message = {
      .type = Dart_CObject_kArray,
  };
  message.value.as_array.length = 11;
  message.value.as_array.values = values;
  Dart_PostCObject_Type post = Dart_PostCObject_DL;
  return post != NULL && post(port, &message);
}
