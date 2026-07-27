import 'dart:convert';
import 'dart:io';

import 'package:test/test.dart';

void main() {
  group('standalone isolate liveness', () {
    test('awaiting native output and exit keeps a CLI alive', () async {
      final script = File(
        'test/src/api/standalone_liveness_probe.dart',
      ).absolute.path;
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
        final output = await stdoutFuture;
        final errorOutput = await stderrFuture;
        expect(exitCode, 0, reason: errorOutput);
        expect(output, contains('ptyx-standalone-alive'));
      } finally {
        if (!exited) {
          process.kill();
        }
      }
    });
  });
}
