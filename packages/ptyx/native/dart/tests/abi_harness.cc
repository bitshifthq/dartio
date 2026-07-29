#include <ptyx/ptyx.h>
#include "ptyx_dart.h"

#include <cstdint>

static_assert(sizeof(ptyd_adapter_t) == sizeof(std::uint64_t));
static_assert(PTYD_INVALID_ADAPTER == 0);

int main() { return 0; }
