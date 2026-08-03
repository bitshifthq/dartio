#include <ptyx/ptyx.h>

#include <cstdint>
#include <type_traits>

static_assert(std::is_standard_layout_v<PtyxError>);
static_assert(std::is_standard_layout_v<PtyxEvent>);
static_assert(sizeof(PtyxRuntime) == sizeof(std::uint64_t));
static_assert(sizeof(PtyxSession) == sizeof(std::uint64_t));
static_assert(std::is_enum_v<PtyxStatus>);
static_assert(std::is_enum_v<PtyxErrorDomain>);
static_assert(std::is_enum_v<PtyxErrorKind>);
static_assert(std::is_enum_v<PtyxEventKind>);
static_assert(sizeof(PtyxStatus) == sizeof(std::int32_t));
static_assert(alignof(PtyxStatus) == alignof(std::int32_t));
static_assert(PTYX_STATUS_ENUM_FORCE_32_BIT == INT32_MAX);
static_assert(PTYX_ERROR_DOMAIN_ENUM_FORCE_32_BIT == INT32_MAX);
static_assert(PTYX_ERROR_KIND_ENUM_FORCE_32_BIT == INT32_MAX);
static_assert(PTYX_EVENT_KIND_ENUM_FORCE_32_BIT == INT32_MAX);
static_assert(PTYX_EVENT_MODE_FAILED == 11);
static_assert(PTYX_EVENT_EXIT_FAILED == 12);
static_assert(PTYX_MODE_CANONICAL == UINT32_C(1));
static_assert(PTYX_MODE_ECHO == UINT32_C(2));
static_assert(PTYX_MODE_SIGNALS == UINT32_C(4));
static_assert(sizeof(PtyxError) == 16);
static_assert(sizeof(PtyxEvent) == 88);
int main() {
  PtyxRuntime runtime = PTYX_INVALID_RUNTIME;
  PtyxSession session = PTYX_INVALID_SESSION;
  PtyxEvent event{};
  PtyxError error{};
  event.struct_size = sizeof(event);
  error.struct_size = sizeof(error);

  if (ptyx_abi_version() != PTYX_ABI_VERSION) {
    return 1;
  }
  if (ptyx_runtime_release(&runtime, &error) != PTYX_STATUS_OK) {
    return 2;
  }
  if (ptyx_session_release(&session, &error) != PTYX_STATUS_OK) {
    return 3;
  }
  if (ptyx_event_release(&event, &error) != PTYX_STATUS_OK) {
    return 4;
  }
  return 0;
}
