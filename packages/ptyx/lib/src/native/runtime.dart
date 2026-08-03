part of 'native.dart';

typedef _NativeFailure = PtyxFailure;
typedef _NativeSpawnRequest = PtyxSpawnRequest;
typedef _NativeSnapshot = PtyxSnapshot;

final class _NativeRuntime implements PtyxFinalizable {
  static final _finalizer = NativeFinalizer(
    Native.addressOf<NativeFinalizerFunction>(ptyd_runtime_finalize),
  );
  static final Future<_NativeRuntime> instance = _create();

  final RawReceivePort _port;
  final int _adapter;
  final int capabilityBits;
  late final _NativeEventRouter _router;

  _NativeRuntime._(this._port, int adapter, this.capabilityBits)
    : _adapter = adapter {
    _router = _NativeEventRouter(this);
    _finalizer.attach(this, Pointer<Void>.fromAddress(adapter), detach: this);
  }

  static Future<_NativeRuntime> _create() async {
    final version = abiVersion();
    if (version != PTYX_ABI_VERSION) {
      throw StateError(
        'ptyx C ABI mismatch: expected $PTYX_ABI_VERSION, found $version',
      );
    }
    if (initializeApi(NativeApi.initializeApiDLData) !=
        ptyx_status.PTYX_STATUS_OK) {
      throw StateError('ptyx Dart adapter initialization failed');
    }

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
      final handle = spawnStart(_adapter, request);
      _router.addPendingSpawn(handle, onReady: onReady, onFailure: onFailure);
    } finally {
      _updateLiveness();
    }
  }

  _NativeFailure? abort() {
    final status = runtimeAbort(_adapter, nativeError());
    final failure = status == ptyx_status.PTYX_STATUS_OK
        ? null
        : ptyxFailureFromNative(status, nativeError());
    _updateLiveness();
    return failure;
  }

  void acknowledge(int token) {
    final status = eventAcknowledge(_adapter, token, nativeError());
    if (status != ptyx_status.PTYX_STATUS_OK) {
      throw ptyxAcknowledgementFailure(status);
    }
  }

  _NativeFailure? releaseSession(int handle) {
    final failure = using((arena) {
      final session = arena<ptyx_session_t>()..value = handle;
      final status = sessionRelease(_adapter, session, nativeError());
      if (status == ptyx_status.PTYX_STATUS_OK ||
          status == ptyx_status.PTYX_STATUS_STALE_HANDLE) {
        return null;
      }
      return ptyxFailureFromNative(status, nativeError());
    });
    if (failure == null) {
      _router.remove(handle);
    }
    _updateLiveness();
    return failure;
  }

  void _onMessage(Object? message) => _router.onMessage(message);

  void _updateLiveness() => _router.updateLiveness();
}

({int adapter, int capabilities}) _createRuntime(int port) {
  return using((arena) {
    final runtime = arena<ptyx_runtime_t>();
    final error = newError(arena);
    final status = runtimeCreate(nullptr, runtime, error);
    if (status != ptyx_status.PTYX_STATUS_OK) {
      throw ptyxFailureFromNative(status, error);
    }
    final capabilities = arena<Uint32>();
    final capabilityStatus = runtimeCapabilities(
      runtime.value,
      capabilities,
      error,
    );
    if (capabilityStatus != ptyx_status.PTYX_STATUS_OK) {
      runtimeShutdown(runtime.value, error);
      runtimeRelease(runtime, error);
      throw ptyxFailureFromNative(capabilityStatus, error);
    }
    final adapter = arena<ptyd_adapter_t>();
    final attachStatus = runtimeAttach(runtime.value, port, adapter, error);
    if (attachStatus != ptyx_status.PTYX_STATUS_OK) {
      runtimeShutdown(runtime.value, error);
      runtimeRelease(runtime, error);
      throw ptyxFailureFromNative(attachStatus, error);
    }
    return (adapter: adapter.value, capabilities: capabilities.value);
  });
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
  var adapter = 0;
  try {
    await for (final message in commands) {
      switch (message) {
        case [_guardianCreate, final int port, final SendPort reply]
            when isInvalidAdapter(adapter):
          unarmedLease.cancel();
          try {
            final created = _createRuntime(port);
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
    if (!isInvalidAdapter(adapter)) {
      final remaining = using((arena) {
        final handle = arena<ptyd_adapter_t>()..value = adapter;
        runtimeDetach(handle, nullptr);
        return handle.value;
      });
      if (!isInvalidAdapter(remaining)) {
        runtimeFinalize(Pointer<Void>.fromAddress(remaining));
      }
    }
  }
}
