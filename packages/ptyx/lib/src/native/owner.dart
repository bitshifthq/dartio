part of 'native.dart';

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
  var adapter = PTYD_INVALID_ADAPTER;
  try {
    await for (final message in commands) {
      switch (message) {
        case [_guardianCreate, final int port, final SendPort reply]
            when adapter == PTYD_INVALID_ADAPTER:
          unarmedLease.cancel();
          try {
            final created = _NativeRuntime._createNative(port);
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
    if (adapter != PTYD_INVALID_ADAPTER) {
      final remaining = using((arena) {
        final handle = arena<ptyd_adapter_t>()..value = adapter;
        ptyd_runtime_detach(handle, nullptr);
        return handle.value;
      });
      if (remaining != PTYD_INVALID_ADAPTER) {
        ptyd_runtime_finalize(Pointer<Void>.fromAddress(remaining));
      }
    }
  }
}
