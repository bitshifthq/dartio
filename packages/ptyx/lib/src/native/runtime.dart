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
  } on PtyException {
    try {
      runtimeShutdown(runtime);
      runtimeRelease(runtime);
    } on PtyException {
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
            when adapter == PTYD_INVALID_ADAPTER:
          unarmedLease.cancel();
          try {
            adapter = _createRuntime(port);
            reply.send([_guardianCreated, adapter]);
          } on PtyException catch (failure) {
            reply.send([
              _guardianCreateFailed,
              failure.category.index,
              failure.nativeCode ?? 0,
              failure.operation,
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
    if (adapter != PTYD_INVALID_ADAPTER) {
      try {
        runtimeDetach(adapter);
      } on PtyException {
        // The native cleanup worker retains failed ownership for retry.
      }
    }
  }
}

typedef _PendingSpawn = ({
  _NativeSession Function(int handle) onReady,
  void Function(PtyException error) onFailure,
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

  PtyCapabilities get capabilities {
    final cached = _capabilities;
    if (cached != null) return cached;
    return _capabilities = runtimeCapabilities(_adapter);
  }

  PtyException? abort() {
    PtyException? failure;
    try {
      runtimeAbort(_adapter);
    } on PtyException catch (error) {
      failure = error;
    }
    _updateLiveness();
    return failure;
  }

  void acknowledge(int token) {
    eventAcknowledge(_adapter, token);
  }

  PtyException? releaseSession(int handle) {
    PtyException? failure;
    try {
      sessionRelease(_adapter, handle);
    } on PtyException catch (error) {
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
    SpawnRequest request, {
    required _NativeSession Function(int handle) onReady,
    required void Function(PtyException error) onFailure,
  }) {
    try {
      final handle = spawnStart(_adapter, request);
      _pendingSpawns[handle] = (onReady: onReady, onFailure: onFailure);
    } finally {
      _updateLiveness();
    }
  }

  void _abortProtocol() {
    final failure = abort() ?? syntheticError(operation: 'controller');
    _handleInfrastructureFailure(failure);
  }

  void _ackOrRelease(int session, int token) {
    try {
      acknowledge(token);
    } on PtyException {
      final failure = releaseSession(session);
      if (failure != null) _handleInfrastructureFailure(failure);
    }
  }

  void _dispatch(NativeEvent event) {
    if (event.isGlobalInfrastructureFailure) {
      _handleInfrastructureFailure(
        event.error ?? syntheticError(operation: 'controller'),
      );
      return;
    }
    if (event.kind == .spawnReady) {
      _handleSpawnReady(event.session);
      return;
    }
    if (event.kind == .spawnFailed) {
      _handleSpawnFailure(event.session, event.error);
      return;
    }
    final target = _sessions[event.session]?.target;
    if (target == null) {
      if (event.token != PTYX_INVALID_EVENT_TOKEN) {
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

  void _handleInfrastructureFailure(PtyException error) {
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
      spawn.onFailure(error);
    }
    for (final session in sessions) {
      session._failInfrastructure(error);
    }
    _updateLiveness();
  }

  void _handleSpawnFailure(int handle, PtyException? error) {
    final pending = _pendingSpawns.remove(handle);
    if (pending == null) {
      _updateLiveness();
      return;
    }
    _retainTerminalDeliveryTurn(pending);
    pending.onFailure(error ?? syntheticError(operation: 'spawn'));
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

  void _onMessage(Object? message) {
    final event = decodeEvent(message);
    if (event == null) {
      _abortProtocol();
      return;
    }
    _dispatch(event);
  }

  void _retainTerminalDeliveryTurn(Object target) {
    _terminalDeliveryTargets.add(target);
    Timer.run(() {
      _terminalDeliveryTargets.remove(target);
      _updateLiveness();
    });
  }

  void _updateLiveness() {
    _port.keepIsolateAlive =
        _pendingSpawns.isNotEmpty ||
        _sessions.isNotEmpty ||
        _terminalDeliveryTargets.isNotEmpty;
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
          final int category,
          final int nativeCode,
          final String operation,
          final String message,
        ]:
          throw exceptionFromCategory(
            category: PtyErrorCategory.values[category],
            nativeCode: nativeCode == 0 ? null : nativeCode,
            operation: operation,
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
