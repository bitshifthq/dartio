part of 'native.dart';

final class _NativeEventRouter {
  final _NativeRuntime _runtime;
  final Map<int, _PendingSpawn> _pendingSpawns = {};
  final Map<int, WeakReference<_NativeSession>> _sessions = {};
  final List<Object> _terminalDeliveryTargets = [];

  _NativeEventRouter(this._runtime);

  void addPendingSpawn(
    int handle, {
    required _NativeSession Function(int handle) onReady,
    required void Function(NativeFailure failure) onFailure,
  }) {
    _pendingSpawns[handle] = (onReady: onReady, onFailure: onFailure);
  }

  void remove(int handle) {
    _pendingSpawns.remove(handle);
    _sessions.remove(handle);
  }

  void onMessage(Object? message) {
    final event = decodeEvent(message);
    if (event == null) {
      _abortProtocol();
      return;
    }
    _dispatch(event);
  }

  void _abortProtocol() {
    final failure = _runtime.abort() ?? _protocolFailure();
    _handleInfrastructureFailure(failure);
  }

  void _dispatch(NativeEvent event) {
    if (event.isGlobalInfrastructureFailure) {
      _handleInfrastructureFailure(
        event.failure ??
            syntheticFailure(
              operation: ptyx_operation.PTYX_OPERATION_RUNTIME_SHUTDOWN,
              kind: ptyx_error_kind.PTYX_ERROR_INFRASTRUCTURE_LOST,
            ),
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

  NativeFailure _protocolFailure() => syntheticFailure(
    operation: ptyx_operation.PTYX_OPERATION_OUTPUT,
    kind: ptyx_error_kind.PTYX_ERROR_INFRASTRUCTURE_LOST,
  );

  void _handleInfrastructureFailure(NativeFailure failure) {
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

  void _handleSpawnFailure(int handle, NativeFailure? failure) {
    final pending = _pendingSpawns.remove(handle);
    if (pending == null) {
      updateLiveness();
      return;
    }
    _retainTerminalDeliveryTurn(pending);
    pending.onFailure(
      failure ??
          syntheticFailure(
            operation: ptyx_operation.PTYX_OPERATION_SPAWN,
            kind: ptyx_error_kind.PTYX_ERROR_NATIVE_FAILURE,
          ),
    );
    updateLiveness();
  }

  void _dispatchSessionEvent(_NativeSession target, NativeEvent event) {
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
    } on NativeFailure {
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
    Timer.run(() {
      _terminalDeliveryTargets.remove(target);
      updateLiveness();
    });
  }
}
