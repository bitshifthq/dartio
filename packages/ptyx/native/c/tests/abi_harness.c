#if !defined(_WIN32) && !defined(_POSIX_C_SOURCE)
#define _POSIX_C_SOURCE 200809L
#endif

#include <ptyx/ptyx.h>

#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#if defined(_WIN32)
#include <windows.h>
#else
#include <sched.h>
#include <time.h>
#endif

_Static_assert(PTYX_ABI_VERSION == UINT32_C(4), "unexpected ABI version");
_Static_assert(sizeof(ptyx_runtime_t) == 8, "runtime handle width changed");
_Static_assert(sizeof(ptyx_session_t) == 8, "session handle width changed");
_Static_assert(sizeof(ptyx_status_t) == sizeof(int32_t),
               "status enum width changed");
_Static_assert(_Alignof(ptyx_status_t) == _Alignof(int32_t),
               "status enum alignment changed");
_Static_assert(sizeof(ptyx_error_domain_t) == sizeof(int32_t),
               "error domain enum width changed");
_Static_assert(sizeof(ptyx_error_kind_t) == sizeof(int32_t),
               "error kind enum width changed");
_Static_assert(sizeof(ptyx_operation_t) == sizeof(int32_t),
               "operation enum width changed");
_Static_assert(sizeof(ptyx_event_kind_t) == sizeof(int32_t),
               "event kind enum width changed");
_Static_assert(PTYX_STATUS_ENUM_FORCE_32_BIT == INT32_MAX,
               "status enum does not force 32-bit storage");
_Static_assert(PTYX_ERROR_DOMAIN_ENUM_FORCE_32_BIT == INT32_MAX,
               "error domain enum does not force 32-bit storage");
_Static_assert(PTYX_ERROR_KIND_ENUM_FORCE_32_BIT == INT32_MAX,
               "error kind enum does not force 32-bit storage");
_Static_assert(PTYX_OPERATION_ENUM_FORCE_32_BIT == INT32_MAX,
               "operation enum does not force 32-bit storage");
_Static_assert(PTYX_EVENT_KIND_ENUM_FORCE_32_BIT == INT32_MAX,
               "event kind enum does not force 32-bit storage");
_Static_assert(PTYX_EVENT_MODE_FAILED == 11, "mode failure event value changed");
_Static_assert(PTYX_EVENT_EXIT_FAILED == 12, "exit failure event value changed");
_Static_assert(PTYX_MODE_CANONICAL == UINT32_C(1),
               "canonical mode bit changed");
_Static_assert(PTYX_MODE_ECHO == UINT32_C(2), "echo mode bit changed");
_Static_assert(PTYX_MODE_SIGNALS == UINT32_C(4), "signal mode bit changed");
_Static_assert(sizeof(ptyx_size_t) == 16, "size layout changed");
_Static_assert(sizeof(ptyx_error_t) == 64, "error layout changed");
_Static_assert(sizeof(ptyx_runtime_options_t) == 56,
               "runtime options layout changed");
_Static_assert(sizeof(ptyx_spawn_options_t) == 144,
               "spawn options layout changed");
_Static_assert(sizeof(ptyx_event_t) == 136, "event layout changed");

static void require(int condition, const char *message) {
  if (!condition) {
    fprintf(stderr, "ptyx ABI harness failed: %s\n", message);
    exit(EXIT_FAILURE);
  }
}

static ptyx_error_t error_value(void) {
  ptyx_error_t error;
  memset(&error, 0, sizeof(error));
  error.struct_size = sizeof(error);
  return error;
}

static ptyx_bytes_view_t bytes_view(const char *value) {
  ptyx_bytes_view_t view;
  view.data = (const uint8_t *)value;
  view.length = strlen(value);
  return view;
}

static uint64_t monotonic_milliseconds(void) {
#if defined(_WIN32)
  return GetTickCount64();
#else
  struct timespec now;
  require(clock_gettime(CLOCK_MONOTONIC, &now) == 0,
          "monotonic clock failed");
  return (uint64_t)now.tv_sec * UINT64_C(1000) +
         (uint64_t)now.tv_nsec / UINT64_C(1000000);
#endif
}

static void yield_thread(void) {
#if defined(_WIN32)
  Sleep(0);
#else
  sched_yield();
#endif
}

static void write_when_admitted(ptyx_session_t session, const char *input,
                                ptyx_error_t *error) {
  const uint64_t deadline = monotonic_milliseconds() + UINT64_C(5000);
  for (;;) {
    const ptyx_status_t status =
        ptyx_session_write(session, (const uint8_t *)input, strlen(input),
                           error);
    if (status == PTYX_STATUS_OK) {
      return;
    }
    require(status == PTYX_STATUS_BACKPRESSURE,
            "session input failed instead of applying backpressure");
    require(monotonic_milliseconds() < deadline,
            "session input remained backpressured");
    yield_thread();
  }
}

static void exercise_session_lifecycle(void) {
#if defined(_WIN32)
  const char *executable = "cmd.exe";
  const char *argument_values[] = {
      "/d", "/c",
      "set /p line= & echo ptyx-c-abi & ping -n 30 "
      "127.0.0.1 >nul"};
  const char *input = "x\r\n";
#else
  const char *executable = "/bin/sh";
  const char *argument_values[] = {"-c",
                                   "read line; echo ptyx-c-abi; sleep 30"};
  const char *input = "x\n";
#endif
  ptyx_bytes_view_t
      arguments[sizeof(argument_values) / sizeof(argument_values[0])];
  ptyx_error_t error = error_value();
  ptyx_event_t event;
  ptyx_runtime_t runtime = PTYX_INVALID_RUNTIME;
  ptyx_session_t session = PTYX_INVALID_SESSION;
  ptyx_size_t size;
  int64_t pid = 0;
  uint64_t tty_required = 0;
  ptyx_spawn_options_t options;
  uint8_t tty_name[1024];
  size_t index;
  int close_started = 0;
  int close_completed = 0;
  int saw_output = 0;

  memset(&event, 0, sizeof(event));
  event.struct_size = sizeof(event);
  memset(&options, 0, sizeof(options));
  options.struct_size = sizeof(options);
  options.flags = PTYX_SPAWN_INHERIT_ENVIRONMENT;
  options.executable = bytes_view(executable);
  for (index = 0; index < sizeof(arguments) / sizeof(arguments[0]); index++) {
    arguments[index] = bytes_view(argument_values[index]);
  }
  options.arguments = arguments;
  options.argument_count = sizeof(arguments) / sizeof(arguments[0]);
  options.size.rows = 24;
  options.size.columns = 80;
  options.input_capacity = UINT64_C(65536);
  options.output_capacity = UINT64_C(65536);
  options.graceful_close_timeout_us = UINT64_C(250000);
  memset(&size, 0, sizeof(size));

  require(ptyx_runtime_create(NULL, &runtime, &error) == PTYX_STATUS_OK,
          "session runtime was not created");
  require(ptyx_session_spawn_start(runtime, &options, &session, &error) ==
              PTYX_STATUS_OK,
          "session spawn request was not admitted");

  while (!close_completed) {
    ptyx_status_t status = ptyx_runtime_next_event(runtime, &event, &error);
    if (status != PTYX_STATUS_OK) {
      fprintf(stderr,
              "next event status=%" PRIu32 " domain=%" PRIu32 " kind=%" PRIu32
              "\n",
              status, error.domain, error.kind);
    }
    require(status == PTYX_STATUS_OK, "session event was not delivered");
    require(event.session == session, "event carried the wrong session");
    require(event.kind != PTYX_EVENT_SPAWN_FAILED, "session spawn failed");
    if (event.kind == PTYX_EVENT_SPAWN_READY) {
      require(!close_started, "spawn readiness was delivered twice");
      require(ptyx_session_get_child_pid(session, &pid, &error) ==
                  PTYX_STATUS_OK,
              "session pid query failed");
      require(pid > 0, "session pid query returned an invalid pid");
      require(ptyx_session_get_size(session, &size, &error) == PTYX_STATUS_OK,
              "session size query failed");
      require(size.rows == 24 && size.columns == 80,
              "session size query returned the wrong size");
      require(ptyx_session_get_tty_name(session, NULL, 0, &tty_required,
                                        &error) == PTYX_STATUS_BUFFER_TOO_SMALL,
              "session terminal-name size query failed");
      require(tty_required > 0, "session terminal-name size was empty");
      require(ptyx_session_get_tty_name(session, tty_name, sizeof(tty_name),
                                        &tty_required, &error) == PTYX_STATUS_OK,
              "session terminal-name query failed");
      write_when_admitted(session, input, &error);
    }
    if (event.kind == PTYX_EVENT_CLOSE_COMPLETE) {
      require(event.flags == 0, "session close reported data loss");
      close_completed = 1;
    }
    if (event.kind == PTYX_EVENT_OUTPUT) {
      require(event.token != PTYX_INVALID_EVENT_TOKEN,
              "output event did not own a token");
      require(event.data != NULL && event.data_length != 0,
              "output event did not carry bytes");
      saw_output = 1;
    }
    const int cancel_output = event.kind == PTYX_EVENT_OUTPUT;
    const int output_done = event.kind == PTYX_EVENT_OUTPUT_DONE;
    require(ptyx_event_release(&event, &error) == PTYX_STATUS_OK,
            "session event was not released");
    if (cancel_output) {
      require(ptyx_session_cancel_output(session, &error) == PTYX_STATUS_OK,
              "session output was not canceled");
      require(ptyx_session_close(session, &error) == PTYX_STATUS_OK,
              "session close was not accepted");
      for (size_t repeated_close = 0; repeated_close < 4096; repeated_close++) {
        require(ptyx_session_close(session, &error) == PTYX_STATUS_OK,
                "repeated session close was not idempotent");
      }
      close_started = 1;
    }
    if (output_done) {
      require(ptyx_session_cancel_output(session, &error) == PTYX_STATUS_OK,
              "terminal output cancellation was not idempotent");
    }
  }

  require(close_started, "session never became ready");
  require(saw_output, "session never delivered output");
  require(ptyx_session_close(session, &error) == PTYX_STATUS_OK,
          "post-terminal session close was not idempotent");
  require(ptyx_session_release(&session, &error) == PTYX_STATUS_OK,
          "closed session was not released");
  require(ptyx_runtime_shutdown(runtime, &error) == PTYX_STATUS_OK,
          "session runtime did not shut down");
  require(ptyx_runtime_release(&runtime, &error) == PTYX_STATUS_OK,
          "session runtime was not released");
}

int main(void) {
  const ptyx_runtime_t stale_runtime = UINT64_C(0xffffffffffffffff);
  const ptyx_session_t stale_session = UINT64_C(0xffffffffffffffff);
  ptyx_error_t error = error_value();
  ptyx_event_t event;
  ptyx_size_t size;
  int64_t pid = 0;
  uint32_t mode = 0;
  uint64_t tty_required = 0;
  ptyx_runtime_t runtime = PTYX_INVALID_RUNTIME;
  ptyx_session_t session = PTYX_INVALID_SESSION;
  uint64_t value = 0;
  uint32_t word = 0;

  memset(&event, 0, sizeof(event));
  event.struct_size = sizeof(event);
  memset(&size, 0, sizeof(size));

  require(ptyx_abi_version() == PTYX_ABI_VERSION, "ABI version mismatch");
  require(ptyx_runtime_create(NULL, NULL, &error) ==
              PTYX_STATUS_INVALID_ARGUMENT,
          "NULL runtime output was accepted");
  require(ptyx_runtime_capabilities(stale_runtime, &word, &error) ==
              PTYX_STATUS_STALE_HANDLE,
          "stale runtime returned capabilities");
  require(ptyx_runtime_next_event(stale_runtime, &event, &error) ==
              PTYX_STATUS_STALE_HANDLE,
          "stale runtime returned an event");
  event.token = UINT64_C(1);
  require(ptyx_runtime_next_event(stale_runtime, &event, &error) ==
              PTYX_STATUS_WRONG_STATE,
          "an owning event was overwritten");
  event.token = PTYX_INVALID_EVENT_TOKEN;
  require(ptyx_runtime_shutdown(stale_runtime, &error) ==
              PTYX_STATUS_STALE_HANDLE,
          "stale runtime shut down");
  require(ptyx_runtime_release(NULL, &error) == PTYX_STATUS_INVALID_ARGUMENT,
          "NULL runtime release target was accepted");
  require(ptyx_runtime_release(&runtime, &error) == PTYX_STATUS_OK,
          "zero runtime release was not idempotent");
  require(ptyx_runtime_create(NULL, &runtime, &error) == PTYX_STATUS_OK,
          "default runtime was not created");
  require(runtime != PTYX_INVALID_RUNTIME,
          "runtime creation returned an invalid handle");
  require(ptyx_runtime_capabilities(runtime, &word, &error) == PTYX_STATUS_OK,
          "live runtime did not return capabilities");
  require(ptyx_runtime_shutdown(runtime, &error) == PTYX_STATUS_OK,
          "runtime did not shut down");
  require(ptyx_runtime_shutdown(runtime, &error) == PTYX_STATUS_OK,
          "repeated runtime shutdown was not idempotent");
  require(ptyx_runtime_release(&runtime, &error) == PTYX_STATUS_OK,
          "shut-down runtime was not released");
  require(runtime == PTYX_INVALID_RUNTIME,
          "runtime release did not clear the handle");

  require(ptyx_session_spawn_start(stale_runtime, NULL, &session, &error) ==
              PTYX_STATUS_INVALID_ARGUMENT,
          "NULL spawn options were accepted");
  require(ptyx_session_write(stale_session, NULL, 1, &error) ==
              PTYX_STATUS_INVALID_ARGUMENT,
          "NULL input was accepted");
  require(ptyx_session_cancel_output(stale_session, &error) ==
              PTYX_STATUS_STALE_HANDLE,
          "stale session canceled output");
  require(ptyx_session_resize(stale_session, NULL, &error) ==
              PTYX_STATUS_INVALID_ARGUMENT,
          "NULL size was accepted");
  require(ptyx_session_terminate(stale_session, 15, &word, &error) ==
              PTYX_STATUS_STALE_HANDLE,
          "stale session was terminated");
  require(ptyx_session_get_size(stale_session, &size, &error) ==
              PTYX_STATUS_STALE_HANDLE,
          "stale session returned a size");
  require(ptyx_session_get_child_pid(stale_session, &pid, &error) ==
              PTYX_STATUS_STALE_HANDLE,
          "stale session returned a pid");
  require(ptyx_session_get_term_mode(stale_session, &mode, &error) ==
              PTYX_STATUS_STALE_HANDLE,
          "stale session returned a terminal mode");
  require(ptyx_session_get_tty_name(stale_session, NULL, 0, &tty_required,
                                    &error) == PTYX_STATUS_STALE_HANDLE,
          "stale session returned a terminal name");
  require(ptyx_session_observe_mode(stale_session, 1, &error) ==
              PTYX_STATUS_STALE_HANDLE,
          "stale session observed modes");
  require(ptyx_session_close(stale_session, &error) == PTYX_STATUS_STALE_HANDLE,
          "stale session closed");
  require(ptyx_session_release(NULL, &error) == PTYX_STATUS_INVALID_ARGUMENT,
          "NULL session release target was accepted");
  require(ptyx_session_release(&session, &error) == PTYX_STATUS_OK,
          "zero session release was not idempotent");

  require(ptyx_event_release(NULL, &error) == PTYX_STATUS_INVALID_ARGUMENT,
          "NULL event release target was accepted");
  require(ptyx_event_release(&event, &error) == PTYX_STATUS_OK,
          "zero event release was not idempotent");
  require(ptyx_error_format(NULL, NULL, 0, &value) ==
              PTYX_STATUS_INVALID_ARGUMENT,
          "NULL error was formatted");

  exercise_session_lifecycle();

  printf("ptyx ABI %" PRIu32
         ": layout, NULL, stale-handle, and idempotence checks passed\n",
         ptyx_abi_version());
  return EXIT_SUCCESS;
}
