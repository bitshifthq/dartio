import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:ptyx/ptyx.dart';

const _size = PtySize(rows: 24, columns: 80);
const _operationTimeout = Duration(seconds: 30);

Future<void> main(List<String> arguments) async {
  final duration = arguments.isEmpty
      ? const Duration(minutes: 5)
      : Duration(seconds: int.parse(arguments.first));
  final deadline = DateTime.now().add(duration);
  final rssBefore = ProcessInfo.currentRss;
  var cycles = 0;
  var bytes = 0;
  while (DateTime.now().isBefore(deadline)) {
    final session = await PtySession.spawn(
      PtySpawnOptions(
        executable: Platform.resolvedExecutable,
        arguments: [
          Platform.script.resolve('../benchmark/fixture.dart').toFilePath(),
          'echo-count',
          '${64 * 1024}',
        ],
        initialSize: _size,
        maxBufferedInput: 64 * 1024,
        maxBufferedOutput: 64 * 1024,
      ),
    ).timeout(_operationTimeout);
    final output = <int>[];
    final iterator = StreamIterator(session.output.expand((chunk) => chunk));
    final ready = ascii.encode('READY');
    for (final expected in ready) {
      if (!await iterator.moveNext().timeout(_operationTimeout) ||
          iterator.current != expected) {
        throw StateError('fixture readiness bytes were corrupted');
      }
    }
    final input = Uint8List(64 * 1024)
      ..setAll(0, List<int>.generate(64 * 1024, (index) => 32 + index % 95));
    final outputDone = Future<void>(() async {
      while (await iterator.moveNext().timeout(_operationTimeout)) {
        output.add(iterator.current);
      }
    });
    await session.write(input).timeout(_operationTimeout);
    await session.flush().timeout(_operationTimeout);
    await outputDone.timeout(_operationTimeout);
    await session.exitCode.timeout(_operationTimeout);
    await session.close().timeout(_operationTimeout);
    if (!_equalBytes(output, input)) {
      throw StateError('cycle $cycles byte mismatch');
    }
    cycles++;
    bytes += input.length;
  }
  stdout.writeln(
    jsonEncode({
      'duration_seconds': duration.inSeconds,
      'cycles': cycles,
      'verified_bytes': bytes,
      'rss_before_bytes': rssBefore,
      'rss_after_bytes': ProcessInfo.currentRss,
    }),
  );
}

bool _equalBytes(List<int> left, List<int> right) {
  if (left.length != right.length) return false;
  for (var index = 0; index < left.length; index++) {
    if (left[index] != right[index]) return false;
  }
  return true;
}
