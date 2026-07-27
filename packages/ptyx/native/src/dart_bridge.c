#include "dart_api_dl.h"

#include <stddef.h>

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

static void arm_failed_post(void) {
  InterlockedExchange(&fail_next_post, 1);
}
#else
#include <stdatomic.h>
static atomic_int fail_next_post = 0;

static bool take_failed_post(void) {
  return atomic_exchange(&fail_next_post, 0) != 0;
}

static void arm_failed_post(void) {
  atomic_store(&fail_next_post, 1);
}
#endif

PTYX_HIDDEN bool ptyx_dart_post_integer(Dart_Port_DL port, int64_t message) {
  if (take_failed_post()) {
    return false;
  }
  Dart_PostInteger_Type post = Dart_PostInteger_DL;
  return post != NULL && post(port, message);
}

PTYX_HIDDEN bool ptyx_dart_post_bytes(
    Dart_Port_DL port,
    int64_t handle,
    const uint8_t *bytes,
    intptr_t length
) {
  if (take_failed_post()) {
    return false;
  }
  Dart_CObject handle_object = {
      .type = Dart_CObject_kInt64,
  };
  handle_object.value.as_int64 = handle;
  Dart_CObject bytes_object = {
      .type = Dart_CObject_kTypedData,
  };
  bytes_object.value.as_typed_data.type = Dart_TypedData_kUint8;
  bytes_object.value.as_typed_data.length = length;
  bytes_object.value.as_typed_data.values = (uint8_t *)bytes;
  Dart_CObject *values[] = {&handle_object, &bytes_object};
  Dart_CObject message = {
      .type = Dart_CObject_kArray,
  };
  message.value.as_array.length = 2;
  message.value.as_array.values = values;
  Dart_PostCObject_Type post = Dart_PostCObject_DL;
  return post != NULL && post(port, &message);
}

PTYX_HIDDEN void ptyx_dart_fail_next_post(void) {
  arm_failed_post();
}
