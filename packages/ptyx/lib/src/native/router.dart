part of 'native.dart';

final class _NativeEvent {
  final int kind;
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
    kind: kind,
    session: session,
    token: token,
    flags: flags,
    value: value,
    data: data as Uint8List?,
    failure: errorKind == ptyx_error_kind.PTYX_ERROR_NONE
        ? null
        : _NativeRuntime._failureFromValues(
            domain: errorDomain,
            kind: errorKind,
            operation: errorOperation,
            nativeCode: errorNativeCode,
            flags: errorFlags,
          ),
  );

  bool get isGlobalInfrastructureFailure =>
      session == PTYX_INVALID_SESSION &&
      kind == ptyx_event_kind.PTYX_EVENT_INFRASTRUCTURE_FAILED;
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
        return;
      }
    }
    final failure = _runtime.abort() ?? _protocolFailure();
    _handleInfrastructureFailure(failure);
  }

  void _dispatch(_NativeEvent event) {
    if (event.isGlobalInfrastructureFailure) {
      _handleInfrastructureFailure(
        event.failure ??
            _NativeRuntime._failureFromValues(
              domain: ptyx_error_domain.PTYX_ERROR_DOMAIN_RUNTIME,
              kind: ptyx_error_kind.PTYX_ERROR_INFRASTRUCTURE_LOST,
              operation: ptyx_operation.PTYX_OPERATION_RUNTIME_SHUTDOWN,
              nativeCode: 0,
              flags: 0,
            ),
      );
      return;
    }
    if (event.kind == ptyx_event_kind.PTYX_EVENT_SPAWN_READY) {
      _handleSpawnReady(event.session);
      return;
    }
    if (event.kind == ptyx_event_kind.PTYX_EVENT_SPAWN_FAILED) {
      _handleSpawnFailure(event.session, event.failure);
      return;
    }

    final target = _sessions[event.session]?.target;
    if (target == null) {
      if (event.token != PTYX_INVALID_EVENT_TOKEN) {
        _ackOrRelease(event.session, event.token);
      }
      final failure = _runtime.releaseSession(event.session);
      if (failure != null) {
        _handleInfrastructureFailure(failure);
      }
      return;
    }
    _dispatchSessionEvent(target, event);
  }

  _NativeFailure _protocolFailure() => _NativeRuntime._failureFromValues(
    domain: ptyx_error_domain.PTYX_ERROR_DOMAIN_RUNTIME,
    kind: ptyx_error_kind.PTYX_ERROR_INFRASTRUCTURE_LOST,
    operation: ptyx_operation.PTYX_OPERATION_OUTPUT,
    nativeCode: 0,
    flags: 0,
  );

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
      if (failure != null) {
        _handleInfrastructureFailure(failure);
      }
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
    pending.onFailure(
      failure ??
          _NativeRuntime._failureFromValues(
            domain: ptyx_error_domain.PTYX_ERROR_DOMAIN_RUNTIME,
            kind: ptyx_error_kind.PTYX_ERROR_NATIVE_FAILURE,
            operation: ptyx_operation.PTYX_OPERATION_SPAWN,
            nativeCode: 0,
            flags: 0,
          ),
    );
    updateLiveness();
  }

  void _dispatchSessionEvent(_NativeSession target, _NativeEvent event) {
    switch (event.kind) {
      case ptyx_event_kind.PTYX_EVENT_OUTPUT:
        if (event.data case final Uint8List bytes) {
          target._nativeOutput(bytes, event.token);
        } else {
          _ackOrRelease(event.session, event.token);
        }
      case ptyx_event_kind.PTYX_EVENT_INPUT_FAILED:
        target._nativeInputFailed(event.failure!);
      case ptyx_event_kind.PTYX_EVENT_OUTPUT_FAILED:
        _retainTerminalDeliveryTurn(target);
        target._nativeOutputFailed(event.failure!);
      case ptyx_event_kind.PTYX_EVENT_INFRASTRUCTURE_FAILED:
        _retainTerminalDeliveryTurn(target);
        target._nativeInfrastructureFailed(event.failure!);
      case ptyx_event_kind.PTYX_EVENT_OUTPUT_DONE:
        _retainTerminalDeliveryTurn(target);
        target._nativeOutputDone();
      case ptyx_event_kind.PTYX_EVENT_EXIT:
        _retainTerminalDeliveryTurn(target);
        target._nativeExit(event.value);
      case ptyx_event_kind.PTYX_EVENT_EXIT_FAILED:
        _retainTerminalDeliveryTurn(target);
        target._nativeExitFailed(event.failure!);
      case ptyx_event_kind.PTYX_EVENT_CLOSE_COMPLETE:
        _retainTerminalDeliveryTurn(target);
        _sessions.remove(event.session);
        target._nativeCloseComplete(event.failure);
        updateLiveness();
      case ptyx_event_kind.PTYX_EVENT_MODE_CHANGED:
        target._nativeModeChanged(event.value);
      case ptyx_event_kind.PTYX_EVENT_MODE_FAILED:
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
