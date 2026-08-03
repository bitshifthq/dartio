part of 'native.dart';

final class _NativeEvent {
  final NativeEventKind kind;
  final int session;
  final int token;
  final int flags;
  final int value;
  final Uint8List? data;
  final _NativeFailure? failure;

  const _NativeEvent({
    required this.kind,
    required this.session,
    required this.token,
    required this.flags,
    required this.value,
    required this.data,
    required this.failure,
  });

  factory _NativeEvent.fromMessage({
    required int kind,
    required int session,
    required int token,
    required int flags,
    required int value,
    required int errorDomain,
    required int errorKind,
    required int errorOperation,
    required int errorNativeCode,
    required int errorFlags,
    required Object? data,
  }) => _NativeEvent(
    kind: decodeEventKind(kind),
    session: session,
    token: token,
    flags: flags,
    value: value,
    data: data as Uint8List?,
    failure: errorKind != ptyx_error_kind.PTYX_ERROR_NONE
        ? ptyxFailureFromEvent(
            domain: errorDomain,
            kind: errorKind,
            operation: errorOperation,
            nativeCode: errorNativeCode,
            flags: errorFlags,
          )
        : null,
  );

  bool get isGlobalInfrastructureFailure =>
      isInvalidSession(session) && kind == .infrastructureFailed;
}

final class _NativeEventRouter {
  final _NativeRuntime _runtime;
  final Map<int, _PendingSpawn> _pendingSpawns = {};
  final Map<int, WeakReference<_NativeSession>> _sessions = {};
  final List<Object> _terminalDeliveryTargets = [];

  _NativeEventRouter(this._runtime);

  void addPendingSpawn(
    int handle, {
    required _NativeSession Function(int handle) onReady,
    required void Function(_NativeFailure failure) onFailure,
  }) {
    _pendingSpawns[handle] = (onReady: onReady, onFailure: onFailure);
  }

  void remove(int handle) {
    _pendingSpawns.remove(handle);
    _sessions.remove(handle);
  }

  void onMessage(Object? message) {
    if (message case [
      final int kind,
      final int session,
      final int token,
      final int flags,
      final int value,
      final int errorDomain,
      final int errorKind,
      final int errorOperation,
      final int errorNativeCode,
      final int errorFlags,
      final Object? data,
    ]) {
      if (data == null || data is Uint8List) {
        try {
          _dispatch(
            _NativeEvent.fromMessage(
              kind: kind,
              session: session,
              token: token,
              flags: flags,
              value: value,
              errorDomain: errorDomain,
              errorKind: errorKind,
              errorOperation: errorOperation,
              errorNativeCode: errorNativeCode,
              errorFlags: errorFlags,
              data: data,
            ),
          );
        } on Object {
          _abortProtocol();
        }
        return;
      }
    }
    _abortProtocol();
  }

  void _abortProtocol() {
    final failure = _runtime.abort() ?? _protocolFailure();
    _handleInfrastructureFailure(failure);
  }

  void _dispatch(_NativeEvent event) {
    if (!_isValid(event)) {
      _abortProtocol();
      return;
    }
    if (event.isGlobalInfrastructureFailure) {
      _handleInfrastructureFailure(
        event.failure ?? ptyxRuntimeShutdownFailure(),
      );
      return;
    }
    if (event.kind == .spawnReady) {
      _handleSpawnReady(event.session);
      return;
    }
    if (event.kind == .spawnFailed) {
      _handleSpawnFailure(event.session, event.failure);
      return;
    }

    final target = _sessions[event.session]?.target;
    if (target == null) {
      if (!isInvalidToken(event.token)) {
        _ackOrRelease(event.session, event.token);
      }
      final failure = _runtime.releaseSession(event.session);
      if (failure != null) _handleInfrastructureFailure(failure);
      return;
    }
    _dispatchSessionEvent(target, event);
  }

  bool _isValid(_NativeEvent event) {
    final sessionValid = !isInvalidSession(event.session);
    final tokenValid = isInvalidToken(event.token);
    return switch (event.kind) {
      .spawnReady => sessionValid && tokenValid && event.data == null,
      .spawnFailed =>
        sessionValid &&
            tokenValid &&
            event.data == null &&
            event.failure != null,
      .output =>
        sessionValid &&
            !tokenValid &&
            event.data != null &&
            event.failure == null,
      .inputFailed =>
        sessionValid &&
            tokenValid &&
            event.data == null &&
            event.failure != null,
      .outputFailed || .exitFailed || .modeFailed =>
        sessionValid &&
            tokenValid &&
            event.data == null &&
            event.failure != null,
      .infrastructureFailed =>
        tokenValid &&
            event.data == null &&
            (event.isGlobalInfrastructureFailure || event.failure != null),
      .outputDone || .exit || .modeChanged =>
        sessionValid &&
            tokenValid &&
            event.data == null &&
            event.failure == null,
      .closeComplete => sessionValid && tokenValid && event.data == null,
    };
  }

  _NativeFailure _protocolFailure() => ptyxOutputInfrastructureFailure();

  void _handleInfrastructureFailure(_NativeFailure failure) {
    final pending = _pendingSpawns.values.toList(growable: false);
    final sessions = [
      for (final reference in _sessions.values)
        if (reference.target case final _NativeSession target) target,
    ];
    if (pending.isNotEmpty || sessions.isNotEmpty) {
      _retainTerminalDeliveryTurn((pending, sessions));
    }
    _pendingSpawns.clear();
    _sessions.clear();
    for (final spawn in pending) {
      spawn.onFailure(failure);
    }
    for (final target in sessions) {
      target._nativeInfrastructureFailed(failure);
    }
    updateLiveness();
  }

  void _handleSpawnReady(int handle) {
    final pending = _pendingSpawns.remove(handle);
    if (pending == null) {
      final failure = _runtime.releaseSession(handle);
      if (failure != null) _handleInfrastructureFailure(failure);
      return;
    }
    final target = pending.onReady(handle);
    _sessions[handle] = WeakReference(target);
    updateLiveness();
  }

  void _handleSpawnFailure(int handle, _NativeFailure? failure) {
    final pending = _pendingSpawns.remove(handle);
    if (pending == null) {
      updateLiveness();
      return;
    }
    _retainTerminalDeliveryTurn(pending);
    pending.onFailure(failure ?? ptyxSpawnFailure());
    updateLiveness();
  }

  void _dispatchSessionEvent(_NativeSession target, _NativeEvent event) {
    switch (event.kind) {
      case .spawnReady:
        _handleSpawnReady(event.session);
      case .spawnFailed:
        _handleSpawnFailure(event.session, event.failure);
      case .output:
        if (event.data case final Uint8List bytes) {
          target._nativeOutput(bytes, event.token);
        } else {
          _ackOrRelease(event.session, event.token);
        }
      case .inputFailed:
        target._nativeInputFailed(event.failure!);
      case .outputFailed:
        _retainTerminalDeliveryTurn(target);
        target._nativeOutputFailed(event.failure!);
      case .infrastructureFailed:
        _retainTerminalDeliveryTurn(target);
        target._nativeInfrastructureFailed(event.failure!);
      case .outputDone:
        _retainTerminalDeliveryTurn(target);
        target._nativeOutputDone();
      case .exit:
        _retainTerminalDeliveryTurn(target);
        target._nativeExit(event.value);
      case .exitFailed:
        _retainTerminalDeliveryTurn(target);
        target._nativeExitFailed(event.failure!);
      case .closeComplete:
        _retainTerminalDeliveryTurn(target);
        _sessions.remove(event.session);
        target._nativeCloseComplete(event.failure);
        updateLiveness();
      case .modeChanged:
        target._nativeModeChanged(event.value);
      case .modeFailed:
        target._nativeModeFailed(event.failure!);
    }
  }

  void _ackOrRelease(int session, int token) {
    try {
      _runtime.acknowledge(token);
    } on _NativeFailure {
      final failure = _runtime.releaseSession(session);
      if (failure != null) {
        _handleInfrastructureFailure(failure);
      }
    }
  }

  void updateLiveness() {
    _runtime._port.keepIsolateAlive =
        _pendingSpawns.isNotEmpty ||
        _sessions.isNotEmpty ||
        _terminalDeliveryTargets.isNotEmpty;
  }

  void _retainTerminalDeliveryTurn(Object target) {
    // A terminal native message can synchronously queue final output and
    // complete several Dart futures. Keep the port alive through the current
    // microtasks and the next event turn so work queued by those completions
    // remains observable before the isolate becomes idle.
    _terminalDeliveryTargets.add(target);
    Timer.run(() => _releaseTerminalDeliveryTurn(target));
  }

  void _releaseTerminalDeliveryTurn(Object target) {
    Timer.run(() {
      _terminalDeliveryTargets.remove(target);
      updateLiveness();
    });
  }
}
