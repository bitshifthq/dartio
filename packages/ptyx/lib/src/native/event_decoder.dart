import '../ffi/ptyx.g.dart';

enum NativeEventKind {
  spawnReady,
  spawnFailed,
  output,
  inputFailed,
  outputFailed,
  infrastructureFailed,
  outputDone,
  exit,
  exitFailed,
  closeComplete,
  modeChanged,
  modeFailed,
}

NativeEventKind decodeEventKind(int value) => switch (value) {
  ptyx_event_kind.PTYX_EVENT_SPAWN_READY => .spawnReady,
  ptyx_event_kind.PTYX_EVENT_SPAWN_FAILED => .spawnFailed,
  ptyx_event_kind.PTYX_EVENT_OUTPUT => .output,
  ptyx_event_kind.PTYX_EVENT_INPUT_FAILED => .inputFailed,
  ptyx_event_kind.PTYX_EVENT_OUTPUT_FAILED => .outputFailed,
  ptyx_event_kind.PTYX_EVENT_INFRASTRUCTURE_FAILED => .infrastructureFailed,
  ptyx_event_kind.PTYX_EVENT_OUTPUT_DONE => .outputDone,
  ptyx_event_kind.PTYX_EVENT_EXIT => .exit,
  ptyx_event_kind.PTYX_EVENT_EXIT_FAILED => .exitFailed,
  ptyx_event_kind.PTYX_EVENT_CLOSE_COMPLETE => .closeComplete,
  ptyx_event_kind.PTYX_EVENT_MODE_CHANGED => .modeChanged,
  ptyx_event_kind.PTYX_EVENT_MODE_FAILED => .modeFailed,
  _ => throw FormatException('unknown native event kind: $value'),
};

bool isInvalidSession(int value) => value == PTYX_INVALID_SESSION;

bool isInvalidToken(int value) => value == PTYX_INVALID_EVENT_TOKEN;

bool isInvalidAdapter(int value) => value == PTYD_INVALID_ADAPTER;
