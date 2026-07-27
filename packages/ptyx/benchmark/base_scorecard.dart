import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math';
import 'dart:typed_data';

import 'package:ptyx/ptyx.dart';

const _size = PtySize(rows: 24, columns: 80);
const _timeout = Duration(seconds: 60);

Future<void> main(List<String> arguments) async {
  final selected = arguments.isEmpty ? 'all' : arguments.single;
  final results = <String, Object?>{
    'schema': 1,
    'boundary':
        'Dart public API through native PTY and child; release native asset',
    'pid': pid,
    'rss_before_bytes': ProcessInfo.currentRss,
  };

  if (selected == 'all' || selected == 'interactive') {
    results['interactive'] = await _interactiveRoundTrips(400);
  }
  if (selected == 'all' || selected == 'output') {
    results['output'] = await _outputThroughput(32 * 1024 * 1024);
  }
  if (selected == 'all' || selected == 'input') {
    results['input'] = await _inputThroughput(32 * 1024 * 1024);
  }
  if (selected == 'all' || selected == 'spawn_close') {
    results['spawn_close'] = await _spawnClose(50);
  }
  if (selected == 'all' || selected == 'idle_100') {
    results['idle_100'] = await _idleSessions(100);
  }
  results['rss_after_bytes'] = ProcessInfo.currentRss;

  stdout.writeln(const JsonEncoder.withIndent('  ').convert(results));
}

Future<PtySession> _spawn(String script) {
  return PtySession.spawn(
    PtySpawnOptions(
      executable: '/bin/sh',
      arguments: ['-c', script],
      initialSize: _size,
    ),
  );
}

Future<({PtySession session, _ChunkReader bytes})> _readySession(
  String script,
) async {
  final session = await _spawn(script);
  final bytes = _ChunkReader(session.output);
  for (final expected in ascii.encode('READY')) {
    if (await bytes.readByte().timeout(_timeout) != expected) {
      await bytes.cancel();
      await session.close();
      throw StateError('child did not emit READY');
    }
  }
  return (session: session, bytes: bytes);
}

Future<Map<String, Object?>> _interactiveRoundTrips(int repetitions) async {
  final (:session, :bytes) = await _readySession(
    'stty raw -echo; printf READY; cat',
  );
  final samples = <int>[];
  try {
    for (var i = 0; i < repetitions; i++) {
      final value = i & 0xff;
      final stopwatch = Stopwatch()..start();
      await session.write(Uint8List.fromList([value]));
      final received = await bytes.readByte().timeout(_timeout);
      stopwatch.stop();
      if (received != value) {
        throw StateError('interactive byte mismatch');
      }
      samples.add(stopwatch.elapsedMicroseconds);
    }
  } finally {
    await bytes.cancel();
    await session.close();
  }
  return _distribution(samples);
}

Future<Map<String, Object?>> _outputThroughput(int byteCount) async {
  final (:session, :bytes) = await _readySession(
    'stty raw -echo; printf READY; '
    'dd bs=1 count=1 of=/dev/null 2>/dev/null; '
    'head -c $byteCount /dev/zero',
  );
  var received = 0;
  final stopwatch = Stopwatch()..start();
  await session.write(Uint8List.fromList(const [1]));
  try {
    while (received < byteCount) {
      final chunk = await bytes.readChunk().timeout(_timeout);
      if (chunk == null) {
        throw StateError('output child reached EOF at $received bytes');
      }
      if (chunk.any((value) => value != 0)) {
        throw StateError('output byte mismatch in chunk at $received');
      }
      received += chunk.length;
    }
    stopwatch.stop();
    final exitCode = await session.exitCode.timeout(_timeout);
    return {
      'bytes': received,
      'elapsed_us': stopwatch.elapsedMicroseconds,
      'mib_per_second':
          received / (1024 * 1024) / (stopwatch.elapsedMicroseconds / 1e6),
      'exit_code': exitCode,
    };
  } finally {
    await bytes.cancel();
    await session.close();
  }
}

Future<Map<String, Object?>> _inputThroughput(int byteCount) async {
  final (:session, :bytes) = await _readySession(
    'stty raw -echo; printf READY; head -c $byteCount | wc -c',
  );
  final chunk = Uint8List(64 * 1024)..fillRange(0, 64 * 1024, 120);
  final stopwatch = Stopwatch()..start();
  try {
    var sent = 0;
    while (sent < byteCount) {
      final count = min(chunk.length, byteCount - sent);
      await session.write(
        count == chunk.length ? chunk : chunk.sublist(0, count),
      );
      sent += count;
    }

    final result = StringBuffer();
    while (true) {
      final value = await bytes.readByte().timeout(_timeout);
      if (value == null) {
        throw StateError('input child reached EOF before reporting a count');
      }
      if (value == 10 || value == 13) {
        if (result.isNotEmpty) break;
      } else {
        result.writeCharCode(value);
      }
    }
    stopwatch.stop();
    final received = int.parse(result.toString().trim());
    final exitCode = await session.exitCode.timeout(_timeout);
    if (received != byteCount) {
      throw StateError('input count mismatch: $received != $byteCount');
    }
    return {
      'bytes': received,
      'elapsed_us': stopwatch.elapsedMicroseconds,
      'mib_per_second':
          received / (1024 * 1024) / (stopwatch.elapsedMicroseconds / 1e6),
      'exit_code': exitCode,
    };
  } finally {
    await bytes.cancel();
    await session.close();
  }
}

Future<Map<String, Object?>> _spawnClose(int repetitions) async {
  final samples = <int>[];
  for (var i = 0; i < repetitions; i++) {
    final stopwatch = Stopwatch()..start();
    final session = await _spawn('exit 0');
    await session.output.drain<void>();
    await session.exitCode.timeout(_timeout);
    await session.close();
    stopwatch.stop();
    samples.add(stopwatch.elapsedMicroseconds);
  }
  return _distribution(samples);
}

Future<Map<String, Object?>> _idleSessions(int count) async {
  final rssBefore = ProcessInfo.currentRss;
  final threadsBefore = await _threadCount();
  final stopwatch = Stopwatch()..start();
  final sessions = <PtySession>[];
  Object? failure;
  try {
    for (var i = 0; i < count; i++) {
      try {
        sessions.add(await _spawn('cat'));
      } on Object catch (error) {
        failure = error;
        break;
      }
    }
    stopwatch.stop();
    return {
      'requested_sessions': count,
      'created_sessions': sessions.length,
      'failure': failure?.toString(),
      'spawn_elapsed_us': stopwatch.elapsedMicroseconds,
      'rss_delta_bytes': ProcessInfo.currentRss - rssBefore,
      'threads_before': threadsBefore,
      'threads_after': failure == null ? await _threadCount() : null,
    };
  } finally {
    for (final session in sessions.reversed) {
      await session.close().timeout(_timeout);
    }
  }
}

Future<int> _threadCount() async {
  if (Platform.isMacOS) {
    final result = await Process.run('ps', ['-M', '-p', '$pid']);
    if (result.exitCode != 0) {
      throw StateError('ps failed: ${result.stderr}');
    }
    return const LineSplitter()
            .convert('${result.stdout}')
            .where((line) => line.trim().isNotEmpty)
            .length -
        1;
  }
  final result = await Process.run('ps', ['-o', 'nlwp=', '-p', '$pid']);
  if (result.exitCode != 0) {
    throw StateError('ps failed: ${result.stderr}');
  }
  return int.parse('${result.stdout}'.trim());
}

Map<String, Object?> _distribution(List<int> values) {
  values.sort();
  int percentile(double p) => values[((values.length - 1) * p).round()];
  final mean = values.reduce((a, b) => a + b) / values.length;
  final squaredError = values
      .map((value) => pow(value - mean, 2))
      .reduce((a, b) => a + b);
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

final class _ChunkReader {
  final StreamIterator<Uint8List> _chunks;
  Uint8List? _current;
  var _offset = 0;

  _ChunkReader(Stream<Uint8List> stream) : _chunks = StreamIterator(stream);

  Future<int?> readByte() async {
    while (_current == null || _offset == _current!.length) {
      if (!await _chunks.moveNext()) return null;
      _current = _chunks.current;
      _offset = 0;
    }
    return _current![_offset++];
  }

  Future<Uint8List?> readChunk() async {
    final current = _current;
    if (current != null && _offset < current.length) {
      final remainder = Uint8List.sublistView(current, _offset);
      _offset = current.length;
      return remainder;
    }
    if (!await _chunks.moveNext()) return null;
    _current = _chunks.current;
    _offset = _current!.length;
    return _current;
  }

  Future<void> cancel() => _chunks.cancel();
}
