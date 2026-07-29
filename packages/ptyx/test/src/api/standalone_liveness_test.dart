import 'dart:convert';
import 'dart:io';

import 'package:test/test.dart';

void main() {
  group('standalone isolate liveness', () {
    Future<({int exitCode, String output, String errorOutput})> runProbe(
      String name,
    ) async {
      final script = File('test/src/api/$name.dart').absolute.path;
      final process = await Process.start(Platform.resolvedExecutable, [
        script,
      ]);
      final stdoutFuture = process.stdout.transform(utf8.decoder).join();
      final stderrFuture = process.stderr.transform(utf8.decoder).join();
      var exited = false;
      try {
        final exitCode = await process.exitCode.timeout(
          const Duration(seconds: 20),
        );
        exited = true;
        return (
          exitCode: exitCode,
          output: await stdoutFuture,
          errorOutput: await stderrFuture,
        );
      } finally {
        if (!exited) {
          process.kill();
        }
      }
    }

    test('awaiting native output and exit keeps a CLI alive', () async {
      final result = await runProbe('standalone_liveness_probe');

      expect(result.exitCode, 0, reason: result.errorOutput);
      expect(result.output, contains('ptyx-standalone-alive'));
    });

    test('awaiting a failed spawn keeps a CLI alive', () async {
      final result = await runProbe(
        'standalone_spawn_failure_liveness_probe',
      );

      expect(result.exitCode, 0, reason: result.errorOutput);
      expect(
        result.output,
        contains('ptyx-standalone-spawn-failure-alive'),
      );
    });
  });
}
