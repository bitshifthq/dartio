import 'dart:convert';
import 'dart:io';

import 'package:test/test.dart';

import '../../benchmark/progress.dart';

void main() {
  group('DiagnosticProgressReporter', () {
    group('record', () {
      test('flushes a structured checkpoint to disk', () async {
        final directory = await Directory.systemTemp.createTemp(
          'ptyx-progress-',
        );
        addTearDown(() => directory.delete(recursive: true));
        final path = '${directory.path}/progress.jsonl';
        final reporter = DiagnosticProgressReporter(
          path: path,
          log: (_) async {},
        );

        await reporter.record('started', details: const {'target': 'test'});
        final checkpoint =
            jsonDecode(await File(path).readAsString()) as Map<String, Object?>;

        expect(checkpoint, containsPair('phase', 'started'));
      });

      test('emits the checkpoint to the diagnostic log', () async {
        String? logged;
        final reporter = DiagnosticProgressReporter(
          path: null,
          log: (line) async {
            logged = line;
          },
        );

        await reporter.record('finished');
        final checkpoint = jsonDecode(logged!) as Map<String, Object?>;

        expect(checkpoint, containsPair('phase', 'finished'));
      });
    });
  });
}
