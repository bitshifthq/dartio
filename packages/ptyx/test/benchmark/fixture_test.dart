import 'dart:async';
import 'dart:io';
import 'dart:typed_data';

import 'package:test/test.dart';

import '../../benchmark/vt_payload.dart';

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

    test('extracts fixture bytes across split VT sequences', () async {
      final chunks = Stream.fromIterable([
        Uint8List.fromList([0x1b, 0x5b, 0x32]),
        Uint8List.fromList([0x4a, 82, 69, 65]),
        Uint8List.fromList([68, 89, 0x1b, 0x5d, 0x30, 0x3b]),
        Uint8List.fromList([0x74, 0x69, 0x74, 0x6c, 0x65, 0x07, 65]),
      ]);

      expect(await fixturePayload(chunks).expand((chunk) => chunk).toList(), [
        ...'READY'.codeUnits,
        65,
      ]);
    });

    test('decodes ordered ConPTY fixture frames and ignores redraws', () async {
      final chunks = Stream.fromIterable([
        Uint8List.fromList('READY\u001b[2K\r~P'.codeUnits),
        Uint8List.fromList(
          'F~~0:QUJD~\r   \u001b[2K\r~0:QUJD~\r~1:REU=~'.codeUnits,
        ),
      ]);

      expect(
        await fixturePayload(
          chunks,
          discardC0: true,
        ).expand((chunk) => chunk).toList(),
        [...'READY'.codeUnits, ...'ABCDE'.codeUnits],
      );
    });

    test('preserves ordinary output containing frame delimiters', () async {
      final chunks = Stream.fromIterable([
        Uint8List.fromList('left~not-a-frame~right'.codeUnits),
      ]);

      expect(
        await fixturePayload(chunks).expand((chunk) => chunk).toList(),
        'left~not-a-frame~right'.codeUnits,
      );
    });
  });
}
