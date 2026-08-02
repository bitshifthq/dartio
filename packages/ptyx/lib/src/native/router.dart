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
      _dispatch(
        kind,
        session,
        token,
        flags,
        value,
        errorDomain,
        errorKind,
        errorOperation,
        errorNativeCode,
        errorFlags,
        data,
      );
      return;
    }
    _handleInfrastructureFailure(_protocolFailure());
  }

  void _dispatch(
    int kind,
    int session,
    int token,
    int flags,
    int value,
    int errorDomain,
    int errorKind,
    int errorOperation,
    int errorNativeCode,
    int errorFlags,
    Object? data,
  ) {
    final failure = errorKind == ptyx_error_kind.PTYX_ERROR_NONE
        ? null
        : _NativeRuntime._failureFromValues(
            domain: errorDomain,
            kind: errorKind,
            operation: errorOperation,
            nativeCode: errorNativeCode,
            flags: errorFlags,
          );
    if (!_validEvent(
      kind: kind,
      session: session,
      token: token,
      errorKind: errorKind,
      failure: failure,
      data: data,
    )) {
      _handleMalformedEvent(session, token);
      return;
    }
    if (_isInfrastructureFailure(kind, session)) {
      _handleInfrastructureFailure(
        failure ??
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
    if (kind == ptyx_event_kind.PTYX_EVENT_SPAWN_READY) {
      _handleSpawnReady(session);
      return;
    }
    if (kind == ptyx_event_kind.PTYX_EVENT_SPAWN_FAILED) {
      _handleSpawnFailure(session, failure);
      return;
    }

    final target = _sessions[session]?.target;
    if (target == null) {
      if (token != PTYX_INVALID_EVENT_TOKEN) {
        _ackOrRelease(session, token);
      }
      final failure = _runtime.releaseSession(session);
      if (failure != null) {
        _handleInfrastructureFailure(failure);
      }
      return;
    }
    _dispatchSessionEvent(
      target,
      kind,
      session,
      token,
      flags,
      value,
      data,
      failure,
    );
  }

  bool _validEvent({
    required int kind,
    required int session,
    required int token,
    required int errorKind,
    required _NativeFailure? failure,
    required Object? data,
  }) {
    final hasToken = token != PTYX_INVALID_EVENT_TOKEN;
    final hasFailure = failure != null;
    final errorMatches =
        hasFailure == (errorKind != ptyx_error_kind.PTYX_ERROR_NONE);
    return errorMatches &&
        switch (kind) {
          ptyx_event_kind.PTYX_EVENT_SPAWN_READY =>
            session != PTYX_INVALID_SESSION &&
                !hasToken &&
                !hasFailure &&
                data == null,
          ptyx_event_kind.PTYX_EVENT_SPAWN_FAILED =>
            session != PTYX_INVALID_SESSION &&
                !hasToken &&
                hasFailure &&
                data == null,
          ptyx_event_kind.PTYX_EVENT_OUTPUT =>
            session != PTYX_INVALID_SESSION &&
                hasToken &&
                !hasFailure &&
                data is Uint8List,
          ptyx_event_kind.PTYX_EVENT_INPUT_FAILED ||
          ptyx_event_kind.PTYX_EVENT_OUTPUT_FAILED ||
          ptyx_event_kind.PTYX_EVENT_EXIT_FAILED ||
          ptyx_event_kind.PTYX_EVENT_MODE_FAILED =>
            session != PTYX_INVALID_SESSION &&
                !hasToken &&
                hasFailure &&
                data == null,
          ptyx_event_kind.PTYX_EVENT_INFRASTRUCTURE_FAILED =>
            !hasToken && hasFailure && data == null,
          ptyx_event_kind.PTYX_EVENT_OUTPUT_DONE ||
          ptyx_event_kind.PTYX_EVENT_EXIT ||
          ptyx_event_kind.PTYX_EVENT_MODE_CHANGED =>
            session != PTYX_INVALID_SESSION &&
                !hasToken &&
                !hasFailure &&
                data == null,
          ptyx_event_kind.PTYX_EVENT_CLOSE_COMPLETE =>
            session != PTYX_INVALID_SESSION && !hasToken && data == null,
          _ => false,
        };
  }

  void _handleMalformedEvent(int session, int token) {
    if (token != PTYX_INVALID_EVENT_TOKEN) {
      _ackOrRelease(session, token);
    }
    final failure = _protocolFailure();
    final target = _sessions[session]?.target;
    if (target == null) {
      _handleInfrastructureFailure(failure);
      return;
    }
    _sessions.remove(session);
    _retainTerminalDeliveryTurn(target);
    target._nativeInfrastructureFailed(failure);
    updateLiveness();
  }

  _NativeFailure _protocolFailure() => _NativeRuntime._failureFromValues(
    domain: ptyx_error_domain.PTYX_ERROR_DOMAIN_RUNTIME,
    kind: ptyx_error_kind.PTYX_ERROR_INFRASTRUCTURE_LOST,
    operation: ptyx_operation.PTYX_OPERATION_OUTPUT,
    nativeCode: 0,
    flags: 0,
  );

  bool _isInfrastructureFailure(int kind, int session) =>
      session == PTYX_INVALID_SESSION &&
      kind == ptyx_event_kind.PTYX_EVENT_INFRASTRUCTURE_FAILED;

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

  void _dispatchSessionEvent(
    _NativeSession target,
    int kind,
    int session,
    int token,
    int flags,
    int value,
    Object? data,
    _NativeFailure? failure,
  ) {
    switch (kind) {
      case ptyx_event_kind.PTYX_EVENT_OUTPUT:
        if (data case final Uint8List bytes) {
          target._nativeOutput(bytes, token);
        } else {
          _ackOrRelease(session, token);
        }
      case ptyx_event_kind.PTYX_EVENT_INPUT_FAILED:
        target._nativeInputFailed(failure!);
      case ptyx_event_kind.PTYX_EVENT_OUTPUT_FAILED:
        _retainTerminalDeliveryTurn(target);
        target._nativeOutputFailed(failure!);
      case ptyx_event_kind.PTYX_EVENT_INFRASTRUCTURE_FAILED:
        _retainTerminalDeliveryTurn(target);
        target._nativeInfrastructureFailed(failure!);
      case ptyx_event_kind.PTYX_EVENT_OUTPUT_DONE:
        _retainTerminalDeliveryTurn(target);
        target._nativeOutputDone();
      case ptyx_event_kind.PTYX_EVENT_EXIT:
        _retainTerminalDeliveryTurn(target);
        target._nativeExit(value);
      case ptyx_event_kind.PTYX_EVENT_EXIT_FAILED:
        _retainTerminalDeliveryTurn(target);
        target._nativeExitFailed(failure!);
      case ptyx_event_kind.PTYX_EVENT_CLOSE_COMPLETE:
        _retainTerminalDeliveryTurn(target);
        _sessions.remove(session);
        target._nativeCloseComplete(failure);
        updateLiveness();
      case ptyx_event_kind.PTYX_EVENT_MODE_CHANGED:
        target._nativeModeChanged(value);
      case ptyx_event_kind.PTYX_EVENT_MODE_FAILED:
        target._nativeModeFailed(failure!);
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
