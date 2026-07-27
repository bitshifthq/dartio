import 'dart:async';
import 'dart:io';

import 'package:test/test.dart';

void main() {
  group('benchmark fixture', () {
    test('flushes readiness before reading input', () async {
      final process = await Process.start(Platform.resolvedExecutable, [
        File('benchmark/fixture.dart').absolute.path,
        'ready-cat',
      ]);
      final output = StreamIterator(process.stdout.expand((chunk) => chunk));
      try {
        final marker = 'READY'.codeUnits;
        var matched = 0;
        for (var consumed = 0; consumed < 64 * 1024; consumed++) {
          expect(
            await output.moveNext().timeout(const Duration(seconds: 10)),
            isTrue,
          );
          if (output.current == marker[matched]) {
            matched++;
            if (matched == marker.length) {
              break;
            }
          } else {
            matched = output.current == marker.first ? 1 : 0;
          }
        }
        expect(matched, marker.length);
      } finally {
        await process.stdin.close();
        await output.cancel();
        process.kill();
        await process.exitCode.timeout(const Duration(seconds: 10));
      }
    });
  });
}
