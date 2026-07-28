#include "ptyx.h"

#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>

_Static_assert(PTYX_ABI_VERSION == 7u, "unexpected compile-time ABI version");
_Static_assert(sizeof(uint32_t) == 4, "uint32_t layout is not supported");
_Static_assert(sizeof(uint64_t) == 8, "uint64_t layout is not supported");
_Static_assert(sizeof(int64_t) == 8, "int64_t layout is not supported");

static void require(int condition, const char *message) {
  if (!condition) {
    fprintf(stderr, "ptyx ABI harness failed: %s\n", message);
    exit(EXIT_FAILURE);
  }
}

int main(void) {
  const uint64_t invalid = UINT64_C(0xffffffffffffffff);
  const uint8_t byte = 0x5a;
  uint32_t size[4] = {0};
  int64_t status = 0;

  require(ptyi_abi_version() == PTYX_ABI_VERSION, "ABI version mismatch");
  require((ptyi_capabilities() & ~UINT32_C(0x1f)) == 0,
          "unknown capability bit");
  require(ptyi_last_error_code() == 0, "unexpected initial native error");
  require(!ptyi_init(NULL), "NULL Dart API initialization was accepted");
  ptyi_finalize(NULL);
  require(!ptyi_abandon(0), "zero handle was abandoned");
  require(ptyi_spawn(NULL, NULL, 0, NULL, 0, true, NULL, 24, 80, 0, 0,
                     65536, 65536) == 0,
          "NULL executable was accepted");
  require(ptyi_spawn("true", NULL, 0, NULL, 0, false, NULL, 24, 80, 0, 0,
                     65536, 65536) == 0,
          "uninitialized empty-array spawn unexpectedly succeeded");
  require(ptyi_spawn("true", (const char *const *)&byte, SIZE_MAX, NULL, 0,
                     true, NULL, 24, 80, 0, 0, 65536, 65536) == 0,
          "oversized argument count was accepted");
  require(!ptyi_activate(invalid, 1, 1), "invalid handle activated");
  require(ptyi_write(invalid, &byte, 1) == -2,
          "unavailable runtime was not reported");
  require(ptyi_write(invalid, NULL, 1) == -1, "NULL input was accepted");
  require(ptyi_write(invalid, &byte, SIZE_MAX) == -1,
          "oversized input length was accepted");
  require(!ptyi_credit_async(invalid, 1), "invalid handle accepted credit");
  require(!ptyi_pause(invalid, true), "invalid handle paused");
  require(!ptyi_exit_status(invalid, &status),
          "invalid handle returned exit status");
  require(!ptyi_exit_status(invalid, NULL), "NULL exit status was accepted");
  require(ptyi_pid(invalid) == -1, "invalid handle returned a process ID");
  require(!ptyi_size(invalid, size), "invalid handle returned a size");
  require(!ptyi_size(invalid, NULL), "NULL size target was accepted");
  require(!ptyi_resize(invalid, 24, 80, 0, 0),
          "invalid handle was resized");
  require(ptyi_signal(invalid, 15) == -1, "invalid handle was signaled");
  require(ptyi_mode(invalid) == -1, "invalid handle returned terminal mode");
  require(ptyi_tty_name(invalid, NULL, 0) == -1,
          "invalid handle returned a terminal name");
  require(!ptyi_close(invalid), "invalid handle closed");
  require(!ptyi_destroy(invalid), "invalid handle destroyed");

  printf("ptyx ABI %u: layout, version, NULL, and stale-handle checks passed\n",
         ptyi_abi_version());
  return EXIT_SUCCESS;
}
