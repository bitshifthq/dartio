import 'dart:typed_data';

import '../api/api.dart';
import '../ffi/ptyx.g.dart';
import 'errors.dart';

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
    final error = errorKind == PtyxErrorKind.PTYX_ERROR_NONE
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

NativeEventKind decodeEventKind(int value) => switch (value) {
  PtyxEventKind.PTYX_EVENT_SPAWN_READY => .spawnReady,
  PtyxEventKind.PTYX_EVENT_SPAWN_FAILED => .spawnFailed,
  PtyxEventKind.PTYX_EVENT_OUTPUT => .output,
  PtyxEventKind.PTYX_EVENT_INPUT_FAILED => .inputFailed,
  PtyxEventKind.PTYX_EVENT_OUTPUT_FAILED => .outputFailed,
  PtyxEventKind.PTYX_EVENT_INFRASTRUCTURE_FAILED => .infrastructureFailed,
  PtyxEventKind.PTYX_EVENT_OUTPUT_DONE => .outputDone,
  PtyxEventKind.PTYX_EVENT_EXIT => .exit,
  PtyxEventKind.PTYX_EVENT_EXIT_FAILED => .exitFailed,
  PtyxEventKind.PTYX_EVENT_CLOSE_COMPLETE => .closeComplete,
  PtyxEventKind.PTYX_EVENT_MODE_CHANGED => .modeChanged,
  PtyxEventKind.PTYX_EVENT_MODE_FAILED => .modeFailed,
  _ => throw FormatException('unknown native event kind: $value'),
};

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
