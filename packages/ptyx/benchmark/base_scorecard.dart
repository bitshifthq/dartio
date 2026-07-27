import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math';
import 'dart:typed_data';

import 'package:ptyx/ptyx.dart';

const _size = PtySize(rows: 24, columns: 80);
const _ready = [82, 69, 65, 68, 89];
const _bufferSize = 64 * 1024;

Future<void> main(List<String> arguments) async {
  final byteCount = _option(arguments, '--bytes=', 32 * 1024 * 1024);
  final repetitions = _option(arguments, '--repetitions=', 5);
  final output = _textOption(arguments, '--output=');
  final result = <String, Object?>{
    'schema': 1,
    'suite': 'ptyx-base-compatible-scorecard',
    'boundary': 'Dart public API through native PTY and child',
    'revision': _git(['rev-parse', 'HEAD']),
    'tree_dirty': _git(['status', '--porcelain']).isNotEmpty,
    'platform': Platform.operatingSystem,
    'architecture': _architecture(),
    'dart_version': Platform.version,
    'byte_count': byteCount,
    'repetitions': repetitions,
    'interactive': [
      for (var run = 0; run < repetitions; run++) await _interactive(),
    ],
    'transport_output': [
      for (var run = 0; run < repetitions; run++) await _output(byteCount),
    ],
    'transport_input': [
      for (var run = 0; run < repetitions; run++) await _input(byteCount),
    ],
    'spawn_close': await _spawnClose(50),
  };
  final encoded = const JsonEncoder.withIndent('  ').convert(result);
  stdout.writeln(encoded);
  if (output != null) {
    await File(output).writeAsString('$encoded\n', flush: true);
  }
}

Future<Map<String, Object?>> _interactive() async {
  final session = await _spawn(
    const PtySpawnOptions(
      executable: '/bin/sh',
      arguments: ['-c', 'stty raw -echo; printf READY; cat'],
      initialSize: _size,
    ),
  );
  final iterator = StreamIterator<int>(session.output.expand((chunk) => chunk));
  final samples = <int>[];
  try {
    await _expectReady(iterator);
    for (var index = 0; index < 400; index++) {
      final byte = index & 0xff;
      final stopwatch = Stopwatch()..start();
      await _write(session, Uint8List.fromList([byte]));
      if (!await iterator.moveNext() || iterator.current != byte) {
        throw StateError('interactive mismatch at $index');
      }
      stopwatch.stop();
      samples.add(stopwatch.elapsedMicroseconds);
    }
    return _distribution(samples);
  } finally {
    await iterator.cancel();
    await session.close();
  }
}

Future<Map<String, Object?>> _output(int byteCount) async {
  final session = await _spawn(
    PtySpawnOptions(
      executable: '/bin/sh',
      arguments: ['-c', _outputScript(byteCount)],
      initialSize: _size,
    ),
  );
  final iterator = StreamIterator<Uint8List>(session.output);
  var received = 0;
  final stopwatch = Stopwatch();
  try {
    await _expectReadyChunks(iterator);
    await _write(session, Uint8List.fromList([1]));
    stopwatch.start();
    while (received < byteCount) {
      if (!await iterator.moveNext()) {
        throw StateError('output EOF at $received of $byteCount');
      }
      final chunk = iterator.current;
      if (received + chunk.length > byteCount) {
        throw StateError('output exceeded $byteCount bytes');
      }
      if (chunk.any((byte) => byte != 0)) {
        throw StateError('output mismatch at $received');
      }
      received += chunk.length;
    }
    stopwatch.stop();
    final exitCode = await session.exitCode;
    return {
      'bytes': received,
      'elapsed_us': stopwatch.elapsedMicroseconds,
      'mib_per_second': _throughput(received, stopwatch),
      'exit_code': exitCode,
    };
  } finally {
    await iterator.cancel();
    await session.close();
  }
}

Future<Map<String, Object?>> _input(int byteCount) async {
  final session = await _spawn(
    PtySpawnOptions(
      executable: '/bin/sh',
      arguments: [
        '-c',
        'stty raw -echo; printf READY; head -c $byteCount | wc -c',
      ],
      initialSize: _size,
    ),
  );
  final iterator = StreamIterator<int>(session.output.expand((chunk) => chunk));
  final source = Uint8List(_bufferSize)..fillRange(0, _bufferSize, 120);
  final stopwatch = Stopwatch();
  try {
    await _expectReady(iterator);
    stopwatch.start();
    var sent = 0;
    while (sent < byteCount) {
      final count = min(_bufferSize, byteCount - sent);
      await _write(
        session,
        count == source.length
            ? source
            : Uint8List.sublistView(source, 0, count),
      );
      sent += count;
    }
    final report = StringBuffer();
    while (await iterator.moveNext()) {
      final byte = iterator.current;
      if (byte == 10 || byte == 13) {
        if (report.isNotEmpty) break;
      } else {
        report.writeCharCode(byte);
      }
    }
    stopwatch.stop();
    final received = int.parse(report.toString());
    final exitCode = await session.exitCode;
    if (received != byteCount) {
      throw StateError('input mismatch: $received != $byteCount');
    }
    return {
      'bytes': received,
      'elapsed_us': stopwatch.elapsedMicroseconds,
      'mib_per_second': _throughput(received, stopwatch),
      'exit_code': exitCode,
    };
  } finally {
    await iterator.cancel();
    await session.close();
  }
}

Future<Map<String, Object?>> _spawnClose(int repetitions) async {
  final samples = <int>[];
  for (var run = 0; run < repetitions; run++) {
    final stopwatch = Stopwatch()..start();
    final session = await _spawn(
      const PtySpawnOptions(
        executable: '/bin/sh',
        arguments: ['-c', 'exit 0'],
        initialSize: _size,
      ),
    );
    final exitCode = await session.exitCode;
    await session.close();
    stopwatch.stop();
    if (exitCode != 0) throw StateError('spawn child exited $exitCode');
    samples.add(stopwatch.elapsedMicroseconds);
  }
  return _distribution(samples);
}

Future<PtySession> _spawn(PtySpawnOptions options) =>
    Future<PtySession>.value(PtySession.spawn(options));

String _outputScript(int byteCount) => [
  'stty raw -echo; printf READY; dd bs=1 count=1 of=/dev/null 2>/dev/null;',
  'head -c $byteCount /dev/zero',
].join(' ');

Future<void> _write(PtySession session, Uint8List bytes) async {
  final result = Function.apply(session.write, [bytes]);
  if (result is Future<void>) await result;
}

Future<void> _expectReady(StreamIterator<int> iterator) async {
  for (final byte in _ready) {
    if (!await iterator.moveNext() || iterator.current != byte) {
      throw StateError('readiness marker mismatch');
    }
  }
}

Future<void> _expectReadyChunks(StreamIterator<Uint8List> iterator) async {
  var matched = 0;
  while (matched != _ready.length) {
    if (!await iterator.moveNext()) {
      throw StateError('readiness marker reached EOF');
    }
    final chunk = iterator.current;
    for (var index = 0; index < chunk.length; index++) {
      if (chunk[index] != _ready[matched]) {
        throw StateError('readiness marker mismatch');
      }
      matched++;
      if (matched == _ready.length) {
        if (index + 1 != chunk.length) {
          throw StateError('unexpected bytes followed readiness marker');
        }
        return;
      }
    }
  }
}

double _throughput(int bytes, Stopwatch stopwatch) =>
    bytes / (1024 * 1024) / (stopwatch.elapsedMicroseconds / 1e6);

Map<String, Object?> _distribution(List<int> values) {
  values.sort();
  int percentile(double value) => values[((values.length - 1) * value).round()];
  final mean = values.reduce((left, right) => left + right) / values.length;
  final squaredError = values
      .map((value) => pow(value - mean, 2))
      .reduce((left, right) => left + right);
  return {
    'samples': values.length,
    'mean_us': mean,
    'stddev_us': sqrt(squaredError / values.length),
    'p50_us': percentile(0.50),
    'p95_us': percentile(0.95),
    'p99_us': percentile(0.99),
    'min_us': values.first,
    'max_us': values.last,
  };
}

int _option(List<String> arguments, String prefix, int fallback) {
  final value = _textOption(arguments, prefix);
  return value == null ? fallback : int.parse(value);
}

String? _textOption(List<String> arguments, String prefix) {
  final values = arguments.where((value) => value.startsWith(prefix));
  return values.isEmpty ? null : values.single.substring(prefix.length);
}

String _git(List<String> arguments) {
  final result = Process.runSync('git', arguments);
  if (result.exitCode != 0) throw StateError('${result.stderr}');
  return '${result.stdout}'.trim();
}

String _architecture() {
  final result = Process.runSync('uname', ['-m']);
  return result.exitCode == 0 ? '${result.stdout}'.trim() : 'unknown';
}
