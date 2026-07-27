#include "dart_api_dl.h"
#include <stdatomic.h>

static atomic_int fail_next_post = 0;

bool ptyx_dart_post_integer(Dart_Port_DL port, int64_t message) {
  if (atomic_exchange(&fail_next_post, 0) != 0) {
    return false;
  }
  return Dart_PostInteger_DL(port, message);
}

bool ptyx_dart_post_bytes(
    Dart_Port_DL port,
    int64_t handle,
    const uint8_t *bytes,
    intptr_t length
) {
  if (atomic_exchange(&fail_next_post, 0) != 0) {
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
  return Dart_PostCObject_DL(port, &message);
}

void ptyx_dart_fail_next_post(void) {
  atomic_store(&fail_next_post, 1);
}
