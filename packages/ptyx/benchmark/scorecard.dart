import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math';
import 'dart:typed_data';

import 'package:crypto/crypto.dart';
import 'package:ptyx/ptyx.dart';

const _size = PtySize(rows: 24, columns: 80);
const _timeout = Duration(seconds: 60);

Future<void> main(List<String> arguments) async {
  final selected = arguments
      .where((argument) => !argument.startsWith('--'))
      .singleOrNull;
  final outputPath = arguments
      .where((argument) => argument.startsWith('--output='))
      .map((argument) => argument.substring('--output='.length))
      .singleOrNull;
  final repetitions = _integerOption(arguments, 'repetitions', 5);
  final warmups = _integerOption(arguments, 'warmups', 1);
  final allowDirty = arguments.contains('--allow-dirty');
  if (repetitions <= 0 || warmups < 0) {
    throw ArgumentError('repetitions must be positive and warmups nonnegative');
  }
  final revision = await _commandOutput('git', const ['rev-parse', 'HEAD']);
  final status = await _commandOutput('git', const [
    'status',
    '--porcelain=v1',
    '--untracked-files=all',
  ]);
  final dirty = status?.isNotEmpty ?? false;
  if (outputPath != null && dirty && !allowDirty) {
    throw StateError(
      'refusing to retain a benchmark from a dirty tree; commit the exact '
      'artifact or pass --allow-dirty to retain a diagnostic result',
    );
  }
  final results = <String, Object?>{
    'schema': 4,
    'suite': 'ptyx-diagnostic-scorecard',
    'acceptance_result': false,
    'missing_acceptance_workloads': const [
      'complete process-tree CPU and memory',
      'descriptor and handle return',
      'active noisy-neighbor fairness',
      'resize and mode overhead',
      'forced close and descendant cleanup timing',
      'multi-gigabyte exact integrity',
      'base, direct-native, and competitor comparisons',
    ],
    'boundary':
        'Dart public API through native PTY and child; release native asset',
    'platform': Platform.operatingSystem,
    'platform_version': Platform.operatingSystemVersion,
    'architecture': await _architecture(),
    'dart_version': Platform.version,
    'rustc_version': await _commandOutput('rustc', const ['--version']),
    'cargo_version': await _commandOutput('cargo', const ['--version']),
    'processors': Platform.numberOfProcessors,
    'revision': Platform.environment['PTYX_BENCHMARK_REVISION'] ?? revision,
    'tree_dirty': dirty,
    'working_tree_sha256': dirty
        ? sha256.convert(utf8.encode(status ?? '')).toString()
        : null,
    'command': arguments,
    'fixture_sha256': await _fileHash(
      Platform.script.resolve('fixture.dart').toFilePath(),
    ),
    'scorecard_sha256': await _fileHash(Platform.script.toFilePath()),
    'warmups': warmups,
    'repetitions': repetitions,
    'pid': pid,
    'rss_before_bytes': ProcessInfo.currentRss,
  };

  if (selected == null || selected == 'all' || selected == 'interactive') {
    results['interactive'] = await _interactiveRoundTrips(400);
  }
  if (selected == null || selected == 'all' || selected == 'output') {
    results['output'] = await _repeated(
      repetitions: repetitions,
      warmups: warmups,
      metric: 'mib_per_second',
      run: () => _outputThroughput(32 * 1024 * 1024),
    );
  }
  if (selected == null || selected == 'all' || selected == 'input') {
    results['input'] = await _repeated(
      repetitions: repetitions,
      warmups: warmups,
      metric: 'mib_per_second',
      run: () => _inputThroughput(32 * 1024 * 1024),
    );
  }
  if (selected == null || selected == 'all' || selected == 'bidirectional') {
    results['bidirectional'] = await _repeated(
      repetitions: repetitions,
      warmups: warmups,
      metric: 'aggregate_mib_per_second',
      run: () => _bidirectionalThroughput(16 * 1024 * 1024),
    );
  }
  if (selected == null || selected == 'all' || selected == 'pause_resume') {
    results['pause_resume'] = await _repeated(
      repetitions: repetitions,
      warmups: warmups,
      metric: 'resume_to_eof_us',
      run: () => _pauseResume(8 * 1024 * 1024),
    );
  }
  if (selected == null || selected == 'all' || selected == 'discard') {
    results['discard'] = await _repeated(
      repetitions: repetitions,
      warmups: warmups,
      metric: 'elapsed_us',
      run: () => _discardOutput(32 * 1024 * 1024),
    );
  }
  if (selected == null || selected == 'all' || selected == 'fairness') {
    results['fairness'] = await _fairness(16, 100);
  }
  if (selected == null || selected == 'all' || selected == 'spawn_close') {
    results['spawn_close'] = await _spawnClose(50);
  }
  if (selected == null || selected == 'all' || selected.startsWith('idle')) {
    for (final count in const [1, 10, 100]) {
      if (selected == null ||
          selected == 'all' ||
          selected == 'idle' ||
          selected == 'idle_$count') {
        results['idle_$count'] = await _idleSessions(count);
      }
    }
  }
  results['rss_after_bytes'] = ProcessInfo.currentRss;

  final json = const JsonEncoder.withIndent('  ').convert(results);
  if (outputPath != null) {
    await File(outputPath).writeAsString('$json\n', flush: true);
  }
  stdout.writeln(json);
}

int _integerOption(List<String> arguments, String name, int fallback) {
  final prefix = '--$name=';
  final value = arguments
      .where((argument) => argument.startsWith(prefix))
      .map((argument) => argument.substring(prefix.length))
      .singleOrNull;
  return value == null ? fallback : int.parse(value);
}

Future<Map<String, Object?>> _repeated({
  required int repetitions,
  required int warmups,
  required String metric,
  required Future<Map<String, Object?>> Function() run,
}) async {
  for (var index = 0; index < warmups; index++) {
    await run();
  }
  final raw = <Map<String, Object?>>[];
  for (var index = 0; index < repetitions; index++) {
    raw.add(await run());
  }
  return {
    'warmups': warmups,
    'repetitions': repetitions,
    'metric': metric,
    'distribution': _numericDistribution([
      for (final result in raw) result[metric]! as num,
    ]),
    'raw_runs': raw,
  };
}

Future<PtySession> _spawnFixture(
  String operation, [
  List<String> arguments = const [],
]) {
  return PtySession.spawn(
    PtySpawnOptions(
      executable: Platform.resolvedExecutable,
      arguments: [
        Platform.script.resolve('fixture.dart').toFilePath(),
        operation,
        ...arguments,
      ],
      initialSize: _size,
    ),
  );
}

Future<PtySession> _spawnExit() {
  return PtySession.spawn(
    PtySpawnOptions(
      executable: Platform.isWindows
          ? r'C:\Windows\System32\cmd.exe'
          : '/usr/bin/true',
      arguments: Platform.isWindows
          ? const ['/d', '/c', 'exit', '0']
          : const [],
      initialSize: _size,
    ),
  );
}

Future<PtySession> _spawnIdle() {
  return PtySession.spawn(
    PtySpawnOptions(
      executable: Platform.isWindows
          ? r'C:\Windows\System32\cmd.exe'
          : '/bin/cat',
      arguments: Platform.isWindows ? const ['/d', '/q'] : const [],
      initialSize: _size,
    ),
  );
}

Future<({PtySession session, _ChunkReader bytes})> _readySession(
  String operation, [
  List<String> arguments = const [],
]) async {
  final session = await _spawnFixture(operation, arguments);
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
  final (:session, :bytes) = await _readySession('ready-cat');
  final samples = <int>[];
  try {
    for (var i = 0; i < repetitions; i++) {
      final value = 32 + (i % 95);
      final stopwatch = Stopwatch()..start();
      await session.write(Uint8List.fromList([value]));
      final received = await bytes.readByte().timeout(_timeout);
      stopwatch.stop();
      if (received != value) {
        throw StateError('interactive byte mismatch: $received != $value');
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
  final (:session, :bytes) = await _readySession('output', ['$byteCount']);
  var received = 0;
  final stopwatch = Stopwatch()..start();
  try {
    await session.write(Uint8List.fromList(const [1]));
    await session.flush();
    while (received < byteCount) {
      final chunk = await bytes.readChunk().timeout(_timeout);
      if (chunk == null) {
        throw StateError('output child reached EOF at $received bytes');
      }
      for (var index = 0; index < chunk.length; index++) {
        final expected = _pattern(received + index);
        if (chunk[index] != expected) {
          throw StateError(
            'output mismatch at ${received + index}: '
            '${chunk[index]} != $expected',
          );
        }
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
  final (:session, :bytes) = await _readySession('input-verify', [
    '$byteCount',
  ]);
  final chunk = Uint8List(64 * 1024);
  final stopwatch = Stopwatch()..start();
  try {
    var sent = 0;
    while (sent < byteCount) {
      final count = min(chunk.length, byteCount - sent);
      _fillPattern(chunk, sent, count);
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
    final report = result.toString().trim();
    final exitCode = await session.exitCode.timeout(_timeout);
    if (report != 'OK $byteCount') {
      throw StateError('input integrity failure: $report');
    }
    return {
      'bytes': byteCount,
      'elapsed_us': stopwatch.elapsedMicroseconds,
      'mib_per_second':
          byteCount / (1024 * 1024) / (stopwatch.elapsedMicroseconds / 1e6),
      'exit_code': exitCode,
    };
  } finally {
    await bytes.cancel();
    await session.close();
  }
}

Future<Map<String, Object?>> _bidirectionalThroughput(int byteCount) async {
  final (:session, :bytes) = await _readySession('echo-count', ['$byteCount']);
  final chunk = Uint8List(64 * 1024);
  var sent = 0;
  var received = 0;
  final stopwatch = Stopwatch()..start();
  try {
    final sender = Future<void>(() async {
      while (sent < byteCount) {
        final count = min(chunk.length, byteCount - sent);
        _fillPattern(chunk, sent, count);
        await session.write(
          count == chunk.length ? chunk : chunk.sublist(0, count),
        );
        sent += count;
      }
      await session.flush();
    });
    final receiver = Future<void>(() async {
      while (received < byteCount) {
        final next = await bytes.readChunk().timeout(_timeout);
        if (next == null) {
          throw StateError('bidirectional child reached EOF at $received');
        }
        for (var index = 0; index < next.length; index++) {
          final expected = _pattern(received + index);
          if (next[index] != expected) {
            throw StateError(
              'bidirectional mismatch at ${received + index}: '
              '${next[index]} != $expected',
            );
          }
        }
        received += next.length;
      }
    });
    await Future.wait([sender, receiver]);
    stopwatch.stop();
    final exitCode = await session.exitCode.timeout(_timeout);
    return {
      'sent_bytes': sent,
      'received_bytes': received,
      'elapsed_us': stopwatch.elapsedMicroseconds,
      'aggregate_mib_per_second':
          (sent + received) /
          (1024 * 1024) /
          (stopwatch.elapsedMicroseconds / 1e6),
      'exit_code': exitCode,
    };
  } finally {
    await bytes.cancel();
    await session.close();
  }
}

Future<Map<String, Object?>> _pauseResume(int byteCount) async {
  final (:session, :bytes) = await _readySession('output', ['$byteCount']);
  var received = 0;
  var invalid = false;
  final done = Completer<void>();
  late final StreamSubscription<Uint8List> subscription;
  subscription = bytes.remaining.listen(
    (chunk) {
      invalid = invalid || chunk.any((value) => value != 0);
      received += chunk.length;
    },
    onError: done.completeError,
    onDone: done.complete,
  );
  subscription.pause();
  final rssBefore = ProcessInfo.currentRss;
  await Future<void>.delayed(const Duration(milliseconds: 250));
  final rssWhilePaused = ProcessInfo.currentRss;
  final stopwatch = Stopwatch()..start();
  subscription.resume();
  try {
    await done.future.timeout(_timeout);
    stopwatch.stop();
    final exitCode = await session.exitCode.timeout(_timeout);
    if (received != byteCount || invalid) {
      throw StateError('pause/resume output mismatch: $received');
    }
    return {
      'bytes': received,
      'pause_ms': 250,
      'paused_rss_delta_bytes': rssWhilePaused - rssBefore,
      'resume_to_eof_us': stopwatch.elapsedMicroseconds,
      'exit_code': exitCode,
    };
  } finally {
    await subscription.cancel();
    await session.close();
  }
}

Future<Map<String, Object?>> _discardOutput(int byteCount) async {
  final (:session, :bytes) = await _readySession('output', ['$byteCount']);
  final stopwatch = Stopwatch()..start();
  try {
    await bytes.cancel();
    session.discardOutput();
    await session.write(Uint8List.fromList(const [1]));
    final exitCode = await session.exitCode.timeout(_timeout);
    stopwatch.stop();
    return {
      'generated_bytes': byteCount,
      'elapsed_us': stopwatch.elapsedMicroseconds,
      'exit_code': exitCode,
    };
  } finally {
    await session.close();
  }
}

Future<Map<String, Object?>> _fairness(
  int sessionCount,
  int repetitions,
) async {
  final pairs = <({PtySession session, _ChunkReader bytes})>[];
  try {
    for (var i = 0; i < sessionCount; i++) {
      pairs.add(await _readySession('ready-cat'));
    }
    final perSession = List.generate(sessionCount, (_) => <int>[]);
    for (var round = 0; round < repetitions; round++) {
      await Future.wait([
        for (var index = 0; index < pairs.length; index++)
          Future<void>(() async {
            final value = 32 + ((round + index) % 95);
            final stopwatch = Stopwatch()..start();
            await pairs[index].session.write(Uint8List.fromList([value]));
            final received = await pairs[index].bytes.readByte().timeout(
              _timeout,
            );
            stopwatch.stop();
            if (received != value) {
              throw StateError('fairness byte mismatch for session $index');
            }
            perSession[index].add(stopwatch.elapsedMicroseconds);
          }),
      ]);
    }
    final p99Values = [
      for (final samples in perSession)
        _distribution(samples)['p99_us']! as int,
    ]..sort();
    return {
      'sessions': sessionCount,
      'round_trips_per_session': repetitions,
      'p99_us_min': p99Values.first,
      'p99_us_max': p99Values.last,
      'per_session': [
        for (var index = 0; index < perSession.length; index++)
          {'session': index, ..._distribution(perSession[index])},
      ],
    };
  } finally {
    for (final pair in pairs.reversed) {
      await pair.bytes.cancel();
      await pair.session.close();
    }
  }
}

Future<Map<String, Object?>> _spawnClose(int repetitions) async {
  final samples = <int>[];
  for (var i = 0; i < repetitions; i++) {
    final stopwatch = Stopwatch()..start();
    final session = await _spawnExit();
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
        sessions.add(await _spawnIdle());
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
  if (Platform.isWindows) {
    final result = await Process.run(
      r'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe',
      [
        '-NoProfile',
        '-NonInteractive',
        '-Command',
        '(Get-Process -Id $pid).Threads.Count',
      ],
    );
    if (result.exitCode != 0) {
      throw StateError('Get-Process failed: ${result.stderr}');
    }
    return int.parse('${result.stdout}'.trim());
  }
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

Future<String?> _architecture() async {
  if (Platform.isWindows) {
    return Platform.environment['PROCESSOR_ARCHITECTURE'];
  }
  return _commandOutput('uname', const ['-m']);
}

Future<String?> _commandOutput(
  String executable,
  List<String> arguments,
) async {
  try {
    final result = await Process.run(executable, arguments);
    if (result.exitCode != 0) return null;
    final output = '${result.stdout}'.trim();
    return output.isEmpty ? null : output;
  } on ProcessException {
    return null;
  }
}

Future<String> _fileHash(String path) async {
  return sha256.convert(await File(path).readAsBytes()).toString();
}

int _pattern(int offset) => 32 + ((offset * 31 + 17) % 95);

void _fillPattern(Uint8List bytes, int offset, int count) {
  for (var index = 0; index < count; index++) {
    bytes[index] = _pattern(offset + index);
  }
}

Map<String, Object?> _numericDistribution(List<num> values) {
  final sorted = values.map((value) => value.toDouble()).toList()..sort();
  double percentile(double p) => sorted[((sorted.length - 1) * p).round()];
  final mean = sorted.reduce((a, b) => a + b) / sorted.length;
  final squaredError = sorted
      .map((value) => pow(value - mean, 2))
      .reduce((a, b) => a + b);
  return {
    'samples': sorted.length,
    'mean': mean,
    'stddev': sqrt(squaredError / sorted.length),
    'p50': percentile(0.50),
    'p95': percentile(0.95),
    'p99': percentile(0.99),
    'min': sorted.first,
    'max': sorted.last,
  };
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

  Stream<Uint8List> get remaining async* {
    final current = _current;
    if (current != null && _offset < current.length) {
      yield Uint8List.sublistView(current, _offset);
      _offset = current.length;
    }
    while (await _chunks.moveNext()) {
      yield _chunks.current;
    }
  }
}
