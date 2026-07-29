#include "ptyx.h"
#include "ptyx_dart.h"

#include <cstdint>
#include <type_traits>

static_assert(std::is_standard_layout_v<ptyx_error_t>);
static_assert(std::is_standard_layout_v<ptyx_event_t>);
static_assert(sizeof(ptyx_runtime_t) == sizeof(std::uint64_t));
static_assert(sizeof(ptyx_session_t) == sizeof(std::uint64_t));
static_assert(sizeof(ptyx_session_snapshot_t) == 96);
static_assert(sizeof(ptyx_event_t) == 136);
static_assert(sizeof(ptyd_adapter_t) == sizeof(std::uint64_t));

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
