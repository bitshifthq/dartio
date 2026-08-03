import 'dart:typed_data';

import '../api/api.dart';
import '../ffi/ptyx.g.dart';
import 'errors.dart';

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

final class NativeEvent {
  final NativeEventKind kind;
  final int session;
  final int token;
  final int flags;
  final int value;
  final Uint8List? data;
  final PtyException? error;

  const NativeEvent({
    required this.kind,
    required this.session,
    required this.token,
    required this.flags,
    required this.value,
    required this.data,
    required this.error,
  });

  bool get isGlobalInfrastructureFailure =>
      session == PTYX_INVALID_SESSION && kind == .infrastructureFailed;
}

NativeEvent? decodeEvent(Object? message) {
  if (message case [
    final int rawKind,
    final int session,
    final int token,
    final int flags,
    final int value,
    final int errorDomain,
    final int errorKind,
    final int errorNativeCode,
    final Object? data,
  ]) {
    if (data != null && data is! Uint8List) return null;
    final kind = _tryDecodeKind(rawKind);
    if (kind == null) return null;
    final error = errorKind == ptyx_error_kind.PTYX_ERROR_NONE
        ? null
        : exceptionFromEvent(
            domain: errorDomain,
            kind: errorKind,
            nativeCode: errorNativeCode,
            operation: _operation(kind),
          );
    final event = NativeEvent(
      kind: kind,
      session: session,
      token: token,
      flags: flags,
      value: value,
      data: data as Uint8List?,
      error: error,
    );
    return event;
  }
  return null;
}

String _operation(NativeEventKind kind) => switch (kind) {
  .spawnFailed => 'spawn',
  .output => 'output',
  .inputFailed => 'write',
  .outputFailed => 'output',
  .infrastructureFailed => 'controller',
  .exitFailed => 'exit',
  .closeComplete => 'close',
  .modeFailed => 'modeChanges.observe',
  _ => 'controller',
};

NativeEventKind? _tryDecodeKind(int value) {
  try {
    return decodeEventKind(value);
  } on FormatException {
    return null;
  }
}
