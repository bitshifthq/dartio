part of 'native.dart';

const _guardianCreate = 0;
const _guardianCreated = 1;
const _guardianCreateFailed = 2;
const _guardianOwnerExit = 3;
const _guardianStartupTimeout = Duration(seconds: 30);
const _guardianUnarmedLease = Duration(seconds: 5);

int _createRuntime(int port) {
  final runtime = runtimeCreate();
  try {
    return runtimeAttach(runtime, port);
  } on NativeFailure {
    try {
      runtimeShutdown(runtime);
      runtimeRelease(runtime);
    } on NativeFailure {
      // The original attach failure is the actionable result. Native cleanup
      // is retried by the adapter when shutdown or release cannot converge.
    }
    rethrow;
  }
}

Future<void> _guardNativeOwner(SendPort ready) async {
  final commands = ReceivePort();
  final unarmedLease = Timer(_guardianUnarmedLease, commands.close);
  ready.send(commands.sendPort);
  var adapter = 0;
  try {
    await for (final message in commands) {
      switch (message) {
        case [_guardianCreate, final int port, final SendPort reply]
            when isInvalidAdapter(adapter):
          unarmedLease.cancel();
          try {
            adapter = _createRuntime(port);
            reply.send([_guardianCreated, adapter]);
          } on NativeFailure catch (failure) {
            reply.send([
              _guardianCreateFailed,
              failure.status,
              failure.domain,
              failure.kind,
              failure.operation,
              failure.nativeCode,
              failure.flags,
              failure.message,
            ]);
            commands.close();
          }
        case [_guardianOwnerExit]:
          commands.close();
      }
    }
  } finally {
    unarmedLease.cancel();
    if (!isInvalidAdapter(adapter)) {
      try {
        runtimeDetach(adapter);
      } on NativeFailure {
        // The native cleanup worker retains failed ownership for retry.
      }
    }
  }
}

typedef _NativeSpawnRequest = PtyxSpawnRequest;
typedef _PendingSpawn = ({
  _NativeSession Function(int handle) onReady,
  void Function(NativeFailure failure) onFailure,
});

final class _NativeRuntime implements PtyxFinalizable {
  static final _finalizer = NativeFinalizer(
    Native.addressOf<NativeFinalizerFunction>(ptyd_runtime_finalize),
  );
  static final Future<_NativeRuntime> instance = _create();

  final RawReceivePort _port;
  final int _adapter;
  PtyCapabilities? _capabilities;
  final Map<int, _PendingSpawn> _pendingSpawns = {};
  final Map<int, WeakReference<_NativeSession>> _sessions = {};
  final List<Object> _terminalDeliveryTargets = [];

  _NativeRuntime._(this._port, int adapter) : _adapter = adapter {
    _finalizer.attach(this, Pointer<Void>.fromAddress(adapter), detach: this);
  }

  NativeFailure? abort() {
    NativeFailure? failure;
    try {
      runtimeAbort(_adapter);
    } on NativeFailure catch (error) {
      failure = error;
    }
    _updateLiveness();
    return failure;
  }

  void acknowledge(int token) {
    eventAcknowledge(_adapter, token);
  }

  NativeFailure? releaseSession(int handle) {
    NativeFailure? failure;
    try {
      sessionRelease(_adapter, handle);
    } on NativeFailure catch (error) {
      failure = error;
    }
    if (failure == null) {
      _pendingSpawns.remove(handle);
      _sessions.remove(handle);
    }
    _updateLiveness();
    return failure;
  }

  void startSpawn(
    _NativeSpawnRequest request, {
    required _NativeSession Function(int handle) onReady,
    required void Function(NativeFailure failure) onFailure,
  }) {
    try {
      final handle = spawnStart(_adapter, request);
      _pendingSpawns[handle] = (onReady: onReady, onFailure: onFailure);
    } finally {
      _updateLiveness();
    }
  }

  void _onMessage(Object? message) {
    final event = decodeEvent(message);
    if (event == null) {
      _abortProtocol();
      return;
    }
    _dispatch(event);
  }

  void _updateLiveness() {
    _port.keepIsolateAlive =
        _pendingSpawns.isNotEmpty ||
        _sessions.isNotEmpty ||
        _terminalDeliveryTargets.isNotEmpty;
  }

  PtyCapabilities get capabilities {
    final cached = _capabilities;
    if (cached != null) return cached;
    return _capabilities = runtimeCapabilities(_adapter);
  }

  void _abortProtocol() {
    final failure =
        abort() ??
        syntheticFailure(
          operation: ptyx_operation.PTYX_OPERATION_OUTPUT,
          kind: ptyx_error_kind.PTYX_ERROR_INFRASTRUCTURE_LOST,
        );
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
      final failure = releaseSession(event.session);
      if (failure != null) _handleInfrastructureFailure(failure);
      return;
    }
    if (event.kind == .output && event.data == null) {
      _ackOrRelease(event.session, event.token);
      return;
    }
    if (event.kind
        case .outputFailed ||
            .infrastructureFailed ||
            .outputDone ||
            .exit ||
            .exitFailed ||
            .closeComplete) {
      _retainTerminalDeliveryTurn(target);
    }
    target._onNativeEvent(event);
    if (event.kind == .closeComplete) {
      _sessions.remove(event.session);
      _updateLiveness();
    }
  }

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
    for (final session in sessions) {
      session._nativeInfrastructureFailed(failure);
    }
    _updateLiveness();
  }

  void _handleSpawnReady(int handle) {
    final pending = _pendingSpawns.remove(handle);
    if (pending == null) {
      final failure = releaseSession(handle);
      if (failure != null) _handleInfrastructureFailure(failure);
      return;
    }
    final session = pending.onReady(handle);
    _sessions[handle] = WeakReference(session);
    _updateLiveness();
  }

  void _handleSpawnFailure(int handle, NativeFailure? failure) {
    final pending = _pendingSpawns.remove(handle);
    if (pending == null) {
      _updateLiveness();
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
    _updateLiveness();
  }

  void _ackOrRelease(int session, int token) {
    try {
      acknowledge(token);
    } on NativeFailure {
      final failure = releaseSession(session);
      if (failure != null) _handleInfrastructureFailure(failure);
    }
  }

  void _retainTerminalDeliveryTurn(Object target) {
    _terminalDeliveryTargets.add(target);
    Timer.run(() {
      _terminalDeliveryTargets.remove(target);
      _updateLiveness();
    });
  }

  static Future<_NativeRuntime> _create() async {
    final version = abiVersion();
    if (version != PTYX_ABI_VERSION) {
      throw StateError(
        'ptyx C ABI mismatch: expected $PTYX_ABI_VERSION, found $version',
      );
    }
    initializeApi(NativeApi.initializeApiDLData.address);

    late final _NativeRuntime controller;
    final port = RawReceivePort();
    final ready = ReceivePort();
    final created = ReceivePort();
    SendPort? guardian;
    try {
      await Isolate.spawn(_guardNativeOwner, ready.sendPort);
      guardian = await ready.first.timeout(_guardianStartupTimeout) as SendPort;
      Isolate.current.addOnExitListener(
        guardian,
        response: const [_guardianOwnerExit],
      );
      guardian.send([
        _guardianCreate,
        port.sendPort.nativePort,
        created.sendPort,
      ]);
      final response =
          await created.first.timeout(_guardianStartupTimeout) as List<Object?>;
      switch (response) {
        case [_guardianCreated, final int adapter]:
          controller = _NativeRuntime._(port, adapter);
        case [
          _guardianCreateFailed,
          final int status,
          final int domain,
          final int kind,
          final int operation,
          final int nativeCode,
          final int flags,
          final String message,
        ]:
          throw NativeFailure(
            status: status,
            domain: domain,
            kind: kind,
            operation: operation,
            nativeCode: nativeCode,
            flags: flags,
            message: message,
          );
        default:
          throw StateError('ptyx owner guardian returned an invalid response');
      }
      port.handler = controller._onMessage;
      port.keepIsolateAlive = false;
      return controller;
    } on Object {
      if (guardian case final owner?) {
        Isolate.current.removeOnExitListener(owner);
        owner.send(const [_guardianOwnerExit]);
      }
      port.close();
      rethrow;
    } finally {
      ready.close();
      created.close();
    }
  }
}
