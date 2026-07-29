import 'dart:io';

import 'package:test/test.dart';

Future<ProcessResult> _runProbe(String operation) => Process.run(
  Platform.resolvedExecutable,
  ['run', 'test/src/api/fault_injection_probe.dart', operation],
).timeout(const Duration(seconds: 30));

void main() {
  test(
    'a failed native-port post becomes typed infrastructure failure',
    () async {
      final result = await _runProbe('failed-post');
      expect(
        result.exitCode,
        0,
        reason:
            'probe stdout:\n${result.stdout}\nprobe stderr:\n${result.stderr}',
      );
    },
  );

  test('broker loss fails every public completion channel', () async {
    if (Platform.isWindows) return;
    final result = await _runProbe('broker-loss');
    expect(
      result.exitCode,
      0,
      reason:
          'probe stdout:\n${result.stdout}\nprobe stderr:\n${result.stderr}',
    );
  });

  test('reentrant write preserves typed infrastructure failure', () async {
    final result = await _runProbe('reentrant-write');

    expect(
      result.exitCode,
      0,
      reason:
          'probe stdout:\n${result.stdout}\nprobe stderr:\n${result.stderr}',
    );
  });

  test(
    'exit observation failure preserves trailing output and cleanup',
    () async {
      final result = await _runProbe('exit-observation');

      expect(
        result.exitCode,
        0,
        reason:
            'probe stdout:\n${result.stdout}\nprobe stderr:\n${result.stderr}',
      );
    },
  );
}
