import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:ptyx/ptyx.dart';

const _size = PtySize(rows: 24, columns: 80);
const _operationTimeout = Duration(seconds: 30);
const _ready = [82, 69, 65, 68, 89];
const _maximumReadinessPrelude = 64 * 1024;

Future<void> main(List<String> arguments) async {
  final duration = arguments.isEmpty
      ? const Duration(minutes: 5)
      : Duration(seconds: int.parse(arguments.first));
  final warmup = await _spawnFixture('ready-cat', 0);
  warmup.discardOutput();
  await warmup.close().timeout(_operationTimeout);
  final rssBefore = ProcessInfo.currentRss;
  final resourcesBefore = await _resourceCount();
  final threadsBefore = await _threadCount();
  var cycles = 0;
  var bytes = 0;
  var longLivedBytes = 0;
  final longSession = await _spawnFixture('ready-cat', 0);
  final longOutput = StreamIterator(
    longSession.output.expand((chunk) => chunk),
  );
  await _expectReady(longOutput);
  final deadline = DateTime.now().add(duration);
  try {
    while (DateTime.now().isBefore(deadline)) {
      bytes += await _runCycle(cycles);
      final interactive = Uint8List(1024)
        ..setAll(
          0,
          List<int>.generate(
            1024,
            (index) => 32 + ((cycles * 1024 + index) % 95),
          ),
        );
      longSession.resize(
        PtySize(rows: 24 + cycles % 8, columns: 80 + cycles % 16),
      );
      await longSession.write(interactive).timeout(_operationTimeout);
      await longSession.flush().timeout(_operationTimeout);
      for (var index = 0; index < interactive.length; index++) {
        if (!await longOutput.moveNext().timeout(_operationTimeout) ||
            longOutput.current != interactive[index]) {
          throw StateError(
            'long-lived session mismatch in cycle $cycles at $index',
          );
        }
      }
      longLivedBytes += interactive.length;
      cycles++;
    }
  } finally {
    await longOutput.cancel();
    await longSession.close().timeout(_operationTimeout);
  }
  stdout.writeln(
    jsonEncode({
      'duration_seconds': duration.inSeconds,
      'cycles': cycles,
      'verified_bytes': bytes,
      'long_lived_verified_bytes': longLivedBytes,
      'rss_before_bytes': rssBefore,
      'rss_after_bytes': ProcessInfo.currentRss,
      'resources_before': resourcesBefore,
      'resources_after': await _resourceCount(),
      'threads_before': threadsBefore,
      'threads_after': await _threadCount(),
    }),
  );
}

Future<int> _runCycle(int cycle) async {
  final session = await _spawnFixture('ready-cat', 0);
  final iterator = StreamIterator(session.output.expand((chunk) => chunk));
  try {
    await _expectReady(iterator);
    final input = Uint8List(64 * 1024)
      ..setAll(0, List<int>.generate(64 * 1024, (index) => 32 + index % 95));
    final outputDone = Future<void>(() async {
      for (var index = 0; index < input.length; index++) {
        if (!await iterator.moveNext().timeout(_operationTimeout) ||
            iterator.current != input[index]) {
          throw StateError('cycle $cycle byte mismatch at $index');
        }
      }
    });
    await session.write(input).timeout(_operationTimeout);
    await Future.wait([
      session.flush().timeout(_operationTimeout),
      outputDone.timeout(_operationTimeout),
    ]);
    return input.length;
  } finally {
    await iterator.cancel();
    await session.close().timeout(_operationTimeout);
  }
}

Future<PtySession> _spawnFixture(String operation, int byteCount) async {
  final session = await PtySession.spawn(
    PtySpawnOptions(
      executable: Platform.resolvedExecutable,
      arguments: [
        Platform.script.resolve('../benchmark/fixture.dart').toFilePath(),
        operation,
        if (byteCount != 0) '$byteCount',
      ],
      initialSize: _size,
      maxBufferedInput: 64 * 1024,
      maxBufferedOutput: 64 * 1024,
    ),
  ).timeout(_operationTimeout);
  unawaited(session.inputDone.catchError((Object _) {}));
  return session;
}

Future<void> _expectReady(StreamIterator<int> iterator) async {
  var matched = 0;
  for (var consumed = 0; consumed < _maximumReadinessPrelude; consumed++) {
    if (!await iterator.moveNext().timeout(_operationTimeout)) {
      throw StateError('fixture exited before its readiness marker');
    }
    final byte = iterator.current;
    if (byte == _ready[matched]) {
      matched++;
      if (matched == _ready.length) {
        return;
      }
    } else {
      matched = byte == _ready.first ? 1 : 0;
    }
  }
  throw StateError('fixture readiness prelude exceeded its bound');
}

Future<int?> _resourceCount() async {
  if (Platform.isLinux) {
    return Directory('/proc/self/fd').listSync(followLinks: false).length;
  }
  if (Platform.isMacOS) {
    final result = await Process.run('lsof', ['-n', '-p', '$pid']);
    if (result.exitCode != 0) return null;
    return const LineSplitter()
            .convert('${result.stdout}')
            .where((line) => line.trim().isNotEmpty)
            .length -
        1;
  }
  final result = await Process.run(
    r'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe',
    [
      '-NoProfile',
      '-NonInteractive',
      '-Command',
      '(Get-Process -Id $pid).HandleCount',
    ],
  );
  return result.exitCode == 0 ? int.tryParse('${result.stdout}'.trim()) : null;
}

Future<int?> _threadCount() async {
  if (Platform.isLinux) {
    return Directory('/proc/self/task').listSync().length;
  }
  if (Platform.isMacOS) {
    final result = await Process.run('ps', ['-M', '-p', '$pid']);
    if (result.exitCode == 0) {
      return const LineSplitter()
              .convert('${result.stdout}')
              .where((line) => line.trim().isNotEmpty)
              .length -
          1;
    }
    return null;
  }
  final result = await Process.run(
    r'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe',
    [
      '-NoProfile',
      '-NonInteractive',
      '-Command',
      '(Get-Process -Id $pid).Threads.Count',
    ],
  );
  return result.exitCode == 0 ? int.tryParse('${result.stdout}'.trim()) : null;
}
