#include <ptyx/ptyx.h>

#include <cstdint>
#include <type_traits>

static_assert(std::is_standard_layout_v<ptyx_error_t>);
static_assert(std::is_standard_layout_v<ptyx_event_t>);
static_assert(sizeof(ptyx_runtime_t) == sizeof(std::uint64_t));
static_assert(sizeof(ptyx_session_t) == sizeof(std::uint64_t));
static_assert(std::is_enum_v<ptyx_status_t>);
static_assert(std::is_enum_v<ptyx_error_domain_t>);
static_assert(std::is_enum_v<ptyx_error_kind_t>);
static_assert(std::is_enum_v<ptyx_operation_t>);
static_assert(std::is_enum_v<ptyx_event_kind_t>);
static_assert(sizeof(ptyx_status_t) == sizeof(std::int32_t));
static_assert(alignof(ptyx_status_t) == alignof(std::int32_t));
static_assert(PTYX_STATUS_ENUM_FORCE_32_BIT == INT32_MAX);
static_assert(PTYX_ERROR_DOMAIN_ENUM_FORCE_32_BIT == INT32_MAX);
static_assert(PTYX_ERROR_KIND_ENUM_FORCE_32_BIT == INT32_MAX);
static_assert(PTYX_OPERATION_ENUM_FORCE_32_BIT == INT32_MAX);
static_assert(PTYX_EVENT_KIND_ENUM_FORCE_32_BIT == INT32_MAX);
static_assert(PTYX_MODE_CANONICAL == UINT32_C(1));
static_assert(PTYX_MODE_ECHO == UINT32_C(2));
static_assert(PTYX_MODE_SIGNALS == UINT32_C(4));
static_assert(sizeof(ptyx_session_snapshot_t) == 96);
static_assert(sizeof(ptyx_event_t) == 136);
int main() {
  ptyx_runtime_t runtime = PTYX_INVALID_RUNTIME;
  ptyx_session_t session = PTYX_INVALID_SESSION;
  ptyx_event_t event{};
  ptyx_error_t error{};
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
