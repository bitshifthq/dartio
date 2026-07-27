@DefaultAsset('package:ptyx/ptyx.dart')
library;

import 'dart:ffi';
import 'dart:io';

import 'package:ptyx/ptyx.dart';

@Native<Void Function()>(symbol: 'ptyi_test_fail_next_post')
external void failNextNativePost();

@Native<Void Function()>(symbol: 'ptyi_test_kill_broker')
external void killNativeBroker();

Future<void> main(List<String> arguments) {
  switch (arguments.length == 1 ? arguments.single : null) {
    case 'failed-post':
      return _exerciseFailedPost();
    case 'broker-loss':
      return _exerciseBrokerLoss();
    default:
      throw ArgumentError.value(arguments, 'arguments', 'unknown probe');
  }
}

Future<void> _expectInfrastructure<T>(Future<T> operation) async {
  try {
    await operation;
  } on PtyInfrastructureException {
    return;
  }
  throw StateError('operation did not fail with PtyInfrastructureException');
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

  failNextNativePost();

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

  killNativeBroker();

  await outputFailure.timeout(const Duration(seconds: 5));
  await _expectInfrastructure(session.exitCode);
  await _expectInfrastructure(session.inputDone);
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
