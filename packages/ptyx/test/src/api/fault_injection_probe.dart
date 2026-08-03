import 'dart:async';
import 'dart:io';
import 'dart:typed_data';

import 'package:ptyx/ptyx.dart';

import '../ffi/ptyx_test.g.dart';

Future<void> main(List<String> arguments) {
  switch (arguments.length == 1 ? arguments.single : null) {
    case 'failed-post':
      return _exerciseFailedPost();
    case 'broker-loss':
      return _exerciseBrokerLoss();
    case 'reentrant-write':
      return _exerciseReentrantWriteFailure();
    case 'exit-observation':
      return _exerciseExitObservationFailure();
    default:
      throw ArgumentError.value(arguments, 'arguments', 'unknown probe');
  }
}

Future<void> _exerciseExitObservationFailure() async {
  const windowsScript = '''
Start-Sleep -Milliseconds 200
[Console]::Write("trailing")
Start-Sleep -Seconds 10
''';
  final session = await PtySession.spawn(
    PtySpawnOptions(
      executable: Platform.isWindows
          ? r'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe'
          : '/bin/sh',
      arguments: Platform.isWindows
          ? const ['-NoProfile', '-NonInteractive', '-Command', windowsScript]
          : const ['-c', 'sleep 0.2; printf trailing; sleep 10'],
      initialSize: const PtySize(rows: 24, columns: 80),
    ),
  );
  final trailingOutput = Completer<void>();
  final output = StringBuffer();
  late final StreamSubscription<Uint8List> outputSubscription;
  outputSubscription = session.output.listen((chunk) {
    output.write(String.fromCharCodes(chunk));
    if (!trailingOutput.isCompleted && output.toString().contains('trailing')) {
      trailingOutput.complete();
    }
  }, onError: trailingOutput.completeError);
  if (ptyd_test_fail_exit_observation() == 0) {
    throw StateError('exit-observation injection requires one active session');
  }
  try {
    await session.exitCode;
    throw StateError('exitCode did not report the injected failure');
  } on PtyException catch (error) {
    if (error.nativeCode != 87) rethrow;
  } on Object {
    rethrow;
  }
  await trailingOutput.future.timeout(const Duration(seconds: 5));
  await outputSubscription.cancel();
  await session.close();
}

Future<void> _exerciseReentrantWriteFailure() async {
  final session = await PtySession.spawn(
    PtySpawnOptions(
      executable: Platform.isWindows
          ? r'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe'
          : '/bin/sh',
      arguments: Platform.isWindows
          ? const [
              '-NoProfile',
              '-NonInteractive',
              '-Command',
              '[Console]::Write("ready"); Start-Sleep -Seconds 10',
            ]
          : const ['-c', 'printf ready; sleep 10'],
      initialSize: const PtySize(rows: 24, columns: 80),
    ),
  );
  final result = Completer<void>();
  late final StreamSubscription<Uint8List> subscription;
  subscription = session.output.listen(
    (_) {
      ptyd_test_fail_next_write();
      try {
        session.write(Uint8List.fromList(const [1]));
      } on PtyInfraException {
        result.complete();
      } on Object catch (error, stackTrace) {
        result.completeError(error, stackTrace);
      }
    },
    onError: (Object error, StackTrace stackTrace) {
      if (!result.isCompleted) {
        result.completeError(error, stackTrace);
      }
    },
  );

  await result.future.timeout(const Duration(seconds: 5));
  await subscription.cancel();
  await _expectInfrastructure(session.close());
}

Future<void> _expectInfrastructure<T>(Future<T> operation) async {
  try {
    await operation;
  } on PtyInfraException {
    return;
  }
  throw StateError('operation did not fail with PtyInfraException');
}

Future<void> _exerciseFailedPost() async {
  final session = await PtySession.spawn(
    PtySpawnOptions(
      executable: Platform.isWindows
          ? r'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe'
          : '/bin/sh',
      arguments: Platform.isWindows
          ? const [
              '-NoProfile',
              '-NonInteractive',
              '-Command',
              'Start-Sleep -Milliseconds 200; [Console]::Write("fault")',
            ]
          : const ['-c', 'sleep 0.2; printf fault'],
      initialSize: const PtySize(rows: 24, columns: 80),
    ),
  );
  final outputFailure = _expectInfrastructure(session.output.drain<Object?>());

  ptyd_test_fail_next_post();

  await outputFailure.timeout(const Duration(seconds: 5));
  await _expectInfrastructure(session.exitCode);
  try {
    await session.close();
  } on PtyException catch (error) {
    if (error.category == PtyErrorCategory.infrastructure ||
        error.category == PtyErrorCategory.cleanup) {
      return;
    }
    rethrow;
  }
  throw StateError('close did not report infrastructure cleanup failure');
}

Future<void> _exerciseBrokerLoss() async {
  final session = await PtySession.spawn(
    const PtySpawnOptions(
      executable: '/bin/sh',
      arguments: ['-c', "trap '' HUP TERM; while :; do sleep 1; done"],
      initialSize: PtySize(rows: 24, columns: 80),
    ),
  );
  final pid = session.pid!;
  final outputFailure = _expectInfrastructure(session.output.drain<Object?>());

  if (ptyd_test_kill_broker() == 0) {
    throw StateError('broker-loss injection requires one active adapter');
  }

  await outputFailure.timeout(const Duration(seconds: 5));
  await _expectInfrastructure(session.exitCode);
  await _expectInfrastructure(session.close());
  final deadline = DateTime.now().add(const Duration(seconds: 5));
  while (await _processRunning(pid) && DateTime.now().isBefore(deadline)) {
    await Future<void>.delayed(const Duration(milliseconds: 20));
  }
  if (await _processRunning(pid)) {
    throw StateError('broker-loss child process $pid remained alive');
  }
}

Future<bool> _processRunning(int pid) async {
  if ((await Process.run('/bin/kill', ['-0', '$pid'])).exitCode != 0) {
    return false;
  }
  final state = await Process.run('/bin/ps', ['-o', 'state=', '-p', '$pid']);
  return state.exitCode == 0 &&
      !(state.stdout as String).trimLeft().startsWith('Z');
}
