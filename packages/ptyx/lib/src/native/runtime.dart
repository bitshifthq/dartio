part of 'native.dart';

typedef _NativeFailure = PtyxFailure;
typedef _NativeSpawnRequest = PtyxSpawnRequest;
typedef _NativeSnapshot = PtyxSnapshot;

final class _NativeRuntime implements PtyxFinalizable {
  static final Future<_NativeRuntime> instance = _create();

  final RawReceivePort _port;
  final int _adapter;
  final int capabilityBits;
  late final _NativeEventRouter _router;

  _NativeRuntime._(this._port, int adapter, this.capabilityBits)
    : _adapter = adapter {
    _router = _NativeEventRouter(this);
    ptyxAttachRuntimeFinalizer(this, adapter);
  }

  static Future<_NativeRuntime> _create() async {
    ptyxInitialize();

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
        ptyxPort(port.sendPort),
        created.sendPort,
      ]);
      final response =
          await created.first.timeout(_guardianStartupTimeout) as List<Object?>;
      switch (response) {
        case [_guardianCreated, final int adapter, final int capabilities]:
          controller = _NativeRuntime._(port, adapter, capabilities);
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
          throw _NativeFailure(
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

  void startSpawn(
    _NativeSpawnRequest request, {
    required _NativeSession Function(int handle) onReady,
    required void Function(_NativeFailure failure) onFailure,
  }) {
    try {
      final handle = ptyxStartSpawn(_adapter, request);
      _router.addPendingSpawn(handle, onReady: onReady, onFailure: onFailure);
    } finally {
      _updateLiveness();
    }
  }

  void attachFinalizer(_NativeSession target, int handle) {
    ptyxAttachSessionFinalizer(target, handle);
  }

  void detachFinalizer(_NativeSession target) {
    ptyxDetachSessionFinalizer(target);
  }

  _NativeFailure? abort() {
    final failure = ptyxAbort(_adapter);
    _updateLiveness();
    return failure;
  }

  void acknowledge(int token) => ptyxAcknowledge(_adapter, token);

  _NativeFailure? releaseSession(int handle) {
    final failure = ptyxReleaseSession(_adapter, handle);
    if (failure == null) {
      _router.remove(handle);
    }
    _updateLiveness();
    return failure;
  }

  void _onMessage(Object? message) => _router.onMessage(message);

  void _updateLiveness() => _router.updateLiveness();
}

typedef _PendingSpawn = ({
  _NativeSession Function(int handle) onReady,
  void Function(_NativeFailure failure) onFailure,
});

const _guardianStartupTimeout = Duration(seconds: 30);
const _guardianUnarmedLease = Duration(seconds: 5);
const _guardianCreate = 0;
const _guardianCreated = 1;
const _guardianCreateFailed = 2;
const _guardianOwnerExit = 3;

Future<void> _guardNativeOwner(SendPort ready) async {
  final commands = ReceivePort();
  final unarmedLease = Timer(_guardianUnarmedLease, commands.close);
  ready.send(commands.sendPort);
  var adapter = ptyxInvalidAdapter;
  try {
    await for (final message in commands) {
      switch (message) {
        case [_guardianCreate, final int port, final SendPort reply]
            when adapter == ptyxInvalidAdapter:
          unarmedLease.cancel();
          try {
            final created = ptyxCreate(port);
            adapter = created.adapter;
            reply.send([_guardianCreated, adapter, created.capabilities]);
          } on _NativeFailure catch (failure) {
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
    ptyxOwnerCleanup(adapter);
  }
}
