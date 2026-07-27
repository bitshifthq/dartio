@DefaultAsset('package:ptyx/ptyx.dart')
library;

import 'dart:ffi';
import 'dart:io';

import 'package:ptyx/ptyx.dart';
import 'package:test/test.dart';

@Native<Void Function()>(symbol: 'ptyi_test_fail_next_post')
external void failNextNativePost();

@Native<Void Function()>(symbol: 'ptyi_test_kill_broker')
external void killNativeBroker();

const _brokerLossChild = 'PTYX_BROKER_LOSS_TEST_CHILD';

void main() {
  if (Platform.environment[_brokerLossChild] == '1') {
    test('broker loss child case', _exerciseBrokerLoss);
    return;
  }

  test(
    'a failed native-port post becomes typed infrastructure failure',
    () async {
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
      final outputFailure = expectLater(
        session.output,
        emitsError(isA<PtyInfrastructureException>()),
      );

      failNextNativePost();

      await outputFailure.timeout(const Duration(seconds: 5));
      await expectLater(
        session.exitCode,
        throwsA(isA<PtyInfrastructureException>()),
      );
      await expectLater(
        session.close(),
        throwsA(
          isA<PtyException>().having(
            (error) => error.category,
            'category',
            anyOf(PtyErrorCategory.infrastructure, PtyErrorCategory.cleanup),
          ),
        ),
      );
    },
  );

  test('broker loss fails every public completion channel', () async {
    if (Platform.isWindows) return;
    final result = await Process.run(
      Platform.resolvedExecutable,
      ['test', '-r', 'compact', 'test/src/api/fault_injection_test.dart'],
      environment: const {_brokerLossChild: '1'},
    ).timeout(const Duration(seconds: 30));
    expect(
      result.exitCode,
      0,
      reason:
          'child stdout:\n${result.stdout}\nchild stderr:\n${result.stderr}',
    );
  });
}

Future<void> _exerciseBrokerLoss() async {
  final session = await PtySession.spawn(
    const PtySpawnOptions(
      executable: '/bin/sh',
      arguments: ['-c', 'sleep 30'],
      initialSize: PtySize(rows: 24, columns: 80),
    ),
  );
  final outputFailure = expectLater(
    session.output,
    emitsError(isA<PtyInfrastructureException>()),
  );

  killNativeBroker();

  await outputFailure.timeout(const Duration(seconds: 5));
  await expectLater(
    session.exitCode,
    throwsA(isA<PtyInfrastructureException>()),
  );
  await expectLater(
    session.inputDone,
    throwsA(isA<PtyInfrastructureException>()),
  );
  await expectLater(
    session.close(),
    throwsA(isA<PtyInfrastructureException>()),
  );
}
