#include "dart_api_dl.h"

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32)
#define PTYX_HIDDEN
#else
#define PTYX_HIDDEN __attribute__((visibility("hidden")))
#endif

PTYX_HIDDEN bool ptyx_dart_post_event(
    Dart_Port_DL port, uint32_t kind, uint64_t session, uint64_t token,
    uint32_t flags, int64_t value, uint32_t error_domain, uint32_t error_kind,
    int32_t native_code, const uint8_t *bytes, intptr_t length) {
  Dart_CObject fields[8];
  int64_t integers[8] = {
      (int64_t)kind,
      (int64_t)session,
      (int64_t)token,
      (int64_t)flags,
      value,
      (int64_t)error_domain,
      (int64_t)error_kind,
      (int64_t)native_code,
  };
  Dart_CObject *values[9];
  for (size_t index = 0; index < 8; index++) {
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
  values[8] = &data;

  Dart_CObject message = {
      .type = Dart_CObject_kArray,
  };
  message.value.as_array.length = 9;
  message.value.as_array.values = values;
  Dart_PostCObject_Type post = Dart_PostCObject_DL;
  return post != NULL && post(port, &message);
}
