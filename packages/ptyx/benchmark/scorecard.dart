import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:isolate';
import 'dart:math';
import 'dart:typed_data';

import 'package:crypto/crypto.dart';
import 'package:ptyx/ptyx.dart';

import 'progress.dart';
import 'vt_payload.dart';

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
  final progressPath = arguments
      .where((argument) => argument.startsWith('--progress='))
      .map((argument) => argument.substring('--progress='.length))
      .singleOrNull;
  final progress = DiagnosticProgressReporter(path: progressPath);
  await progress.record(
    'process-started',
    details: {'platform': Platform.operatingSystem, 'arguments': arguments},
  );
  final repetitions = _integerOption(arguments, 'repetitions', 5);
  final warmups = _integerOption(arguments, 'warmups', 1);
  final integrityBytes = _integerOption(
    arguments,
    'integrity-bytes',
    2 * 1024 * 1024 * 1024,
  );
  final benchmarkBytes = _integerOption(arguments, 'bytes', 32 * 1024 * 1024);
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
  if (revision == null || status == null) {
    throw StateError('benchmark retention requires a readable Git revision');
  }
  await progress.record('provenance-read');
  final dirty = status.isNotEmpty;
  if (outputPath != null && dirty && !allowDirty) {
    throw StateError(
      'refusing to retain a benchmark from a dirty tree; commit the exact '
      'artifact or pass --allow-dirty to retain a diagnostic result',
    );
  }
  final fixtureExecutable = Platform.environment['PTYX_FIXTURE_EXECUTABLE'];
  final results = <String, Object?>{
    'schema': 4,
    'suite': 'ptyx-diagnostic-scorecard',
    'acceptance_result': false,
    'missing_acceptance_workloads': const [
      'clean exact-revision acceptance manifest for multi-gigabyte integrity',
      'base, direct-native, and competitor comparisons',
      'allocation and copy instrumentation',
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
    'working_tree_sha256': dirty ? await _workingTreeHash() : null,
    'command': arguments,
    'fixture_sha256': await _fileHash(
      Platform.script.resolve('fixture.dart').toFilePath(),
    ),
    'fixture_executable': fixtureExecutable,
    'fixture_executable_sha256': fixtureExecutable == null
        ? null
        : await _fileHash(fixtureExecutable),
    'scorecard_sha256': await _fileHash(Platform.script.toFilePath()),
    'warmups': warmups,
    'repetitions': repetitions,
    'pid': pid,
    'rss_before_bytes': ProcessInfo.currentRss,
  };
  await progress.record('metadata-ready');

  if (selected == null || selected == 'all' || selected == 'interactive') {
    results['interactive'] = await _runPhase(
      progress,
      'interactive',
      () => _interactiveRoundTrips(400),
    );
  }
  if (selected == null || selected == 'all' || selected == 'output') {
    results['output'] = await _runPhase(
      progress,
      'output',
      () => _repeated(
        repetitions: repetitions,
        warmups: warmups,
        metric: 'mib_per_second',
        run: () => _outputThroughput(benchmarkBytes),
      ),
    );
  }
  if (selected == null || selected == 'all' || selected == 'transport_output') {
    results['transport_output'] = await _runPhase(
      progress,
      'transport-output',
      () => _repeated(
        repetitions: repetitions,
        warmups: warmups,
        metric: 'mib_per_second',
        run: () => _transportOutput(benchmarkBytes),
      ),
    );
  }
  if (selected == null || selected == 'all' || selected == 'input') {
    results['input'] = await _runPhase(
      progress,
      'input',
      () => _repeated(
        repetitions: repetitions,
        warmups: warmups,
        metric: 'mib_per_second',
        run: () => _inputThroughput(benchmarkBytes),
      ),
    );
  }
  if (selected == null || selected == 'all' || selected == 'transport_input') {
    results['transport_input'] = await _runPhase(
      progress,
      'transport-input',
      () => _repeated(
        repetitions: repetitions,
        warmups: warmups,
        metric: 'mib_per_second',
        run: () => _transportInput(benchmarkBytes),
      ),
    );
  }
  if (selected == null || selected == 'all' || selected == 'bidirectional') {
    results['bidirectional'] = await _runPhase(
      progress,
      'bidirectional',
      () => _repeated(
        repetitions: repetitions,
        warmups: warmups,
        metric: 'aggregate_mib_per_second',
        run: () => _bidirectionalThroughput(benchmarkBytes),
      ),
    );
  }
  if (selected == null || selected == 'all' || selected == 'pause_resume') {
    results['pause_resume'] = await _runPhase(
      progress,
      'pause-resume',
      () => _repeated(
        repetitions: repetitions,
        warmups: warmups,
        metric: 'resume_to_eof_us',
        run: () => _pauseResume(benchmarkBytes),
      ),
    );
  }
  if (selected == null || selected == 'all' || selected == 'discard') {
    results['discard'] = await _runPhase(
      progress,
      'discard',
      () => _repeated(
        repetitions: repetitions,
        warmups: warmups,
        metric: 'elapsed_us',
        run: () => _discardOutput(benchmarkBytes),
      ),
    );
  }
  if (selected == null || selected == 'all' || selected == 'no_listener') {
    results['no_listener'] = await _runPhase(
      progress,
      'no-listener',
      () => _noListener(8 * 1024 * 1024),
    );
  }
  if (selected == null || selected == 'all' || selected == 'saturation') {
    results['saturation'] = await _runPhase(
      progress,
      'saturation',
      () => _inputSaturation(1024 * 1024),
    );
  }
  if (selected == null || selected == 'all' || selected == 'fairness') {
    results['fairness'] = await _runPhase(
      progress,
      'fairness',
      () => _fairness(16, 100),
    );
  }
  if (selected == null || selected == 'all' || selected == 'active_output') {
    results['active_output'] = await _runPhase(
      progress,
      'active-output',
      () => _activeOutputFairness(16, 8 * 1024 * 1024),
    );
  }
  if (selected == null || selected == 'all' || selected == 'spawn_close') {
    results['spawn_close'] = await _runPhase(
      progress,
      'spawn-close',
      () => _spawnClose(50),
    );
  }
  if (selected == null || selected == 'all' || selected == 'observation') {
    results['observation'] = await _runPhase(
      progress,
      'observation',
      () => _observationOverhead(1000),
    );
  }
  if (selected == null || selected == 'all' || selected == 'forced_close') {
    results['forced_close'] = await _runPhase(
      progress,
      'forced-close',
      _forcedClose,
    );
  }
  if (selected == null || selected == 'all' || selected.startsWith('idle')) {
    for (final count in const [1, 10, 100]) {
      if (selected == null ||
          selected == 'all' ||
          selected == 'idle' ||
          selected == 'idle_$count') {
        results['idle_$count'] = await _runPhase(
          progress,
          'idle-$count',
          () => _idleSessions(count),
        );
      }
    }
  }
  if (selected == 'integrity') {
    results['integrity'] = await _runPhase(progress, 'integrity', () async {
      return {
        'passed': true,
        'byte_count': integrityBytes,
        'output': await _outputThroughput(integrityBytes),
        'input': await _inputThroughput(integrityBytes),
        'bidirectional': await _bidirectionalThroughput(integrityBytes),
      };
    });
  }
  results['rss_after_bytes'] = ProcessInfo.currentRss;

  await progress.record('final-artifact-writing');
  final json = const JsonEncoder.withIndent('  ').convert(results);
  if (outputPath != null) {
    await File(outputPath).writeAsString('$json\n', flush: true);
  }
  stdout.writeln(json);
  await stdout.flush();
  await progress.record('completed');
}

Future<T> _runPhase<T>(
  DiagnosticProgressReporter progress,
  String name,
  Future<T> Function() operation,
) async {
  await progress.record('$name-started');
  final result = await operation();
  await progress.record('$name-completed');
  return result;
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
  int? maxBufferedInput,
]) {
  final fixture = Platform.environment['PTYX_FIXTURE_EXECUTABLE'];
  return PtySession.spawn(
    PtySpawnOptions(
      executable: fixture ?? Platform.resolvedExecutable,
      arguments: [
        if (fixture == null)
          Platform.script.resolve('fixture.dart').toFilePath(),
        operation,
        ...arguments,
      ],
      initialSize: _size,
      maxBufferedInput: maxBufferedInput ?? 1024 * 1024,
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
  const marker = [82, 69, 65, 68, 89];
  var matched = 0;
  for (var consumed = 0; consumed < 64 * 1024; consumed++) {
    final byte = await bytes.readByte().timeout(_timeout);
    if (byte == null) break;
    if (byte == marker[matched]) {
      matched++;
      if (matched == marker.length) {
        return (session: session, bytes: bytes);
      }
    } else {
      matched = byte == marker.first ? 1 : 0;
    }
  }
  await bytes.cancel();
  await session.close();
  throw StateError('child did not emit READY');
}

Future<Map<String, Object?>> _interactiveRoundTrips(int repetitions) async {
  final (:session, :bytes) = await _readySession(
    Platform.isWindows ? 'input-report' : 'ready-cat',
  );
  final samples = <int>[];
  try {
    for (var i = 0; i < repetitions; i++) {
      final value = _interactiveByte(i);
      final stopwatch = Stopwatch()..start();
      session.write(Uint8List.fromList([value]));
      final received = Platform.isWindows
          ? await _readReport(bytes, 'PTYX-INPUT ${i + 1} $value')
          : await bytes.readByte().timeout(_timeout);
      stopwatch.stop();
      if (Platform.isWindows
          ? received != 'PTYX-INPUT ${i + 1} $value'
          : received != value) {
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
  if (Platform.isWindows) {
    return _windowsTerminalOutput('output', byteCount);
  }
  final (:session, :bytes) = await _readySession('output', ['$byteCount']);
  var received = 0;
  final stopwatch = Stopwatch()..start();
  try {
    session.write(Uint8List.fromList(const [1]));
    while (received < byteCount) {
      final chunk = await bytes.readChunk().timeout(_timeout);
      if (chunk == null) {
        throw StateError('output child reached EOF at $received bytes');
      }
      for (final byte in chunk) {
        final accepted = _acceptPatternByte(byte, received);
        if (accepted < 0) {
          throw StateError(
            'output mismatch at $received: $byte != ${_pattern(received)}',
          );
        }
        received += accepted;
      }
    }
    var trailingBytes = 0;
    while (await bytes.readByte().timeout(_timeout) != null) {
      trailingBytes++;
    }
    if (trailingBytes != 0) {
      throw StateError('output contained $trailingBytes trailing bytes');
    }
    stopwatch.stop();
    final exitCode = await session.exitCode.timeout(_timeout);
    return {
      'bytes': received,
      'trailing_bytes': trailingBytes,
      'elapsed_us': stopwatch.elapsedMicroseconds,
      'mib_per_second':
          received / (1024 * 1024) / (stopwatch.elapsedMicroseconds / 1e6),
      'exit_code': exitCode,
      'integrity_scope': 'ordered child output bytes',
    };
  } finally {
    await bytes.cancel();
    await session.close();
  }
}

Future<Map<String, Object?>> _transportOutput(int byteCount) async {
  if (Platform.isWindows) {
    return _windowsTerminalOutput('output-constant', byteCount);
  }

  final posixScript =
      '''
stty raw -echo
printf READY
dd bs=1 count=1 of=/dev/null 2>/dev/null
head -c $byteCount /dev/zero
''';
  final session = await PtySession.spawn(
    PtySpawnOptions(
      executable: '/bin/sh',
      arguments: ['-c', posixScript],
      initialSize: _size,
    ),
  );
  final bytes = _ChunkReader(session.output);
  try {
    for (final expected in ascii.encode('READY')) {
      if (await bytes.readByte().timeout(_timeout) != expected) {
        throw StateError('transport child did not emit READY');
      }
    }
    session.write(Uint8List.fromList(const [1]));
    final stopwatch = Stopwatch()..start();
    var received = 0;
    while (received < byteCount) {
      final chunk = await bytes.readChunk().timeout(_timeout);
      if (chunk == null) {
        throw StateError('transport output reached EOF at $received');
      }
      if (chunk.any((byte) => byte != 0)) {
        throw StateError('transport output mismatch at $received');
      }
      received += chunk.length;
    }
    var trailingBytes = 0;
    while (await bytes.readByte().timeout(_timeout) != null) {
      trailingBytes++;
    }
    if (trailingBytes != 0) {
      throw StateError(
        'transport output contained $trailingBytes trailing bytes',
      );
    }
    stopwatch.stop();
    return {
      'bytes': received,
      'trailing_bytes': trailingBytes,
      'elapsed_us': stopwatch.elapsedMicroseconds,
      'mib_per_second':
          received / (1024 * 1024) / (stopwatch.elapsedMicroseconds / 1e6),
      'exit_code': await session.exitCode.timeout(_timeout),
      'integrity_scope': 'ordered transport output bytes',
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
      await _writeAfterBackpressure(
        session,
        count == chunk.length ? chunk : chunk.sublist(0, count),
      );
      sent += count;
    }

    final expectedReport = 'OK $byteCount';
    final result = await _readReport(bytes, expectedReport);
    stopwatch.stop();
    final report = result.trim();
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
      'integrity_scope': 'ordered child input validation report',
    };
  } finally {
    await bytes.cancel();
    await session.close();
  }
}

Future<Map<String, Object?>> _transportInput(int byteCount) async {
  final windowsScript =
      r'''
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class PtyxConsoleMode {
  [DllImport("kernel32.dll", SetLastError = true)]
  public static extern IntPtr GetStdHandle(int handle);

  [DllImport("kernel32.dll", SetLastError = true)]
  public static extern bool GetConsoleMode(IntPtr handle, out uint mode);

  [DllImport("kernel32.dll", SetLastError = true)]
  public static extern bool SetConsoleMode(IntPtr handle, uint mode);
}
'@
$handle = [PtyxConsoleMode]::GetStdHandle(-10)
[uint32]$mode = 0
if (-not [PtyxConsoleMode]::GetConsoleMode($handle, [ref]$mode)) {
  throw [ComponentModel.Win32Exception]::new(
    [Runtime.InteropServices.Marshal]::GetLastWin32Error()
  )
}
# Disable ENABLE_LINE_INPUT and ENABLE_ECHO_INPUT so the transport child
# drains binary writes without waiting for a newline.
if (-not [PtyxConsoleMode]::SetConsoleMode(
  $handle,
  [uint32]($mode -band 4294967289)
)) {
  throw [ComponentModel.Win32Exception]::new(
    [Runtime.InteropServices.Marshal]::GetLastWin32Error()
  )
}
[Console]::Write("READY")
$input = [Console]::OpenStandardInput()
$buffer = [byte[]]::new(65536)
$received = 0
while ($received -lt {bytes}) {
  $count = $input.Read(
    $buffer,
    0,
    [Math]::Min($buffer.Length, {bytes} - $received)
  )
  if ($count -eq 0) { break }
  $received += $count
}
[Console]::WriteLine($received)
'''
          .replaceAll('{bytes}', '$byteCount');
  final session = await PtySession.spawn(
    PtySpawnOptions(
      executable: Platform.isWindows
          ? r'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe'
          : '/bin/sh',
      arguments: Platform.isWindows
          ? ['-NoProfile', '-NonInteractive', '-Command', windowsScript]
          : ['-c', 'stty raw -echo; printf READY; head -c $byteCount | wc -c'],
      initialSize: _size,
    ),
  );
  final bytes = _ChunkReader(session.output);
  final chunk = Uint8List(64 * 1024)..fillRange(0, 64 * 1024, 120);
  try {
    for (final expected in ascii.encode('READY')) {
      if (await bytes.readByte().timeout(_timeout) != expected) {
        throw StateError('transport child did not emit READY');
      }
    }
    final stopwatch = Stopwatch()..start();
    var sent = 0;
    while (sent < byteCount) {
      final count = min(chunk.length, byteCount - sent);
      await _writeAfterBackpressure(
        session,
        count == chunk.length ? chunk : chunk.sublist(0, count),
      );
      sent += count;
    }
    final report = await _readReport(bytes, '$byteCount');
    stopwatch.stop();
    if (report.trim() != '$byteCount') {
      throw StateError('transport input mismatch: $report');
    }
    return {
      'bytes': sent,
      'elapsed_us': stopwatch.elapsedMicroseconds,
      'mib_per_second':
          sent / (1024 * 1024) / (stopwatch.elapsedMicroseconds / 1e6),
      'exit_code': await session.exitCode.timeout(_timeout),
    };
  } finally {
    await bytes.cancel();
    await session.close();
  }
}

Future<Map<String, Object?>> _bidirectionalThroughput(int byteCount) async {
  if (Platform.isWindows) {
    final (:session, :bytes) = await _readySession('bidirectional-report', [
      '$byteCount',
    ]);
    final chunk = Uint8List(64 * 1024);
    var sent = 0;
    var received = 0;
    var trailingBytes = 0;
    final stopwatch = Stopwatch()..start();
    try {
      final sender = Future<void>(() async {
        while (sent < byteCount) {
          final count = min(chunk.length, byteCount - sent);
          _fillPattern(chunk, sent, count);
          await _writeAfterBackpressure(
            session,
            count == chunk.length ? chunk : chunk.sublist(0, count),
          );
          sent += count;
        }
      });
      final receiver = Future<void>(() async {
        final expectedReport = ascii.encode('PTYX-BIDI-OK $byteCount');
        final reportBytes = <int>[];
        while (received < byteCount) {
          final next = await bytes.readChunk().timeout(_timeout);
          if (next == null) {
            throw StateError('bidirectional child reached EOF at $received');
          }
          for (final byte in next) {
            if (received == byteCount) {
              reportBytes.add(byte);
              continue;
            }
            final accepted = _acceptPatternByte(byte, received);
            if (accepted < 0) {
              throw StateError(
                'bidirectional mismatch at $received: '
                '$byte != ${_pattern(received)}',
              );
            }
            received += accepted;
          }
        }
        while (reportBytes.length < expectedReport.length) {
          final byte = await bytes.readByte().timeout(_timeout);
          if (byte == null) {
            throw StateError('bidirectional receipt ended before its report');
          }
          reportBytes.add(byte);
        }
        var reportMatches = true;
        for (var index = 0; index < expectedReport.length; index++) {
          if (reportBytes[index] != expectedReport[index]) {
            reportMatches = false;
            break;
          }
        }
        if (!reportMatches) {
          throw StateError(
            'bidirectional receipt mismatch: '
            '${String.fromCharCodes(reportBytes.take(expectedReport.length))}',
          );
        }
        final reportTrailing = _TrailingLineEnding();
        for (final byte in reportBytes.skip(expectedReport.length)) {
          reportTrailing.add(byte);
        }
        while (true) {
          final byte = await bytes.readByte().timeout(_timeout);
          if (byte == null) break;
          reportTrailing.add(byte);
        }
        trailingBytes = reportTrailing.invalidBytes;
      });
      await Future.wait([sender, receiver]);
      stopwatch.stop();
      if (sent != byteCount || received != byteCount || trailingBytes != 0) {
        throw StateError(
          'bidirectional transfer was not exact: sent=$sent '
          'received=$received trailing=$trailingBytes',
        );
      }
      final exitCode = await session.exitCode.timeout(_timeout);
      return {
        'sent_bytes': sent,
        'received_bytes': received,
        'trailing_bytes': trailingBytes,
        'elapsed_us': stopwatch.elapsedMicroseconds,
        'aggregate_mib_per_second':
            (sent + received) /
            (1024 * 1024) /
            (stopwatch.elapsedMicroseconds / 1e6),
        'exit_code': exitCode,
        'exact_output_history_supported': true,
        'integrity_scope': 'ordered child echo plus terminal receipt',
      };
    } finally {
      await bytes.cancel();
      await session.close();
    }
  }
  final (:session, :bytes) = await _readySession('echo-count', ['$byteCount']);
  final chunk = Uint8List(64 * 1024);
  var sent = 0;
  var received = 0;
  var trailingBytes = 0;
  final stopwatch = Stopwatch()..start();
  try {
    final sender = Future<void>(() async {
      while (sent < byteCount) {
        final count = min(chunk.length, byteCount - sent);
        _fillPattern(chunk, sent, count);
        await _writeAfterBackpressure(
          session,
          count == chunk.length ? chunk : chunk.sublist(0, count),
        );
        sent += count;
      }
    });
    final receiver = Future<void>(() async {
      while (received < byteCount) {
        final next = await bytes.readChunk().timeout(_timeout);
        if (next == null) {
          throw StateError('bidirectional child reached EOF at $received');
        }
        for (final byte in next) {
          if (received == byteCount) {
            trailingBytes++;
            continue;
          }
          final accepted = _acceptPatternByte(byte, received);
          if (accepted < 0) {
            throw StateError(
              'bidirectional mismatch at $received: '
              '$byte != ${_pattern(received)}',
            );
          }
          received += accepted;
        }
      }
    });
    await Future.wait([sender, receiver]);
    while (await bytes.readByte().timeout(_timeout) != null) {
      trailingBytes++;
    }
    stopwatch.stop();
    final exitCode = await session.exitCode.timeout(_timeout);
    return {
      'sent_bytes': sent,
      'received_bytes': received,
      'trailing_bytes': trailingBytes,
      'elapsed_us': stopwatch.elapsedMicroseconds,
      'aggregate_mib_per_second':
          (sent + received) /
          (1024 * 1024) /
          (stopwatch.elapsedMicroseconds / 1e6),
      'exit_code': exitCode,
      'integrity_scope': 'ordered child echo bytes',
    };
  } finally {
    await bytes.cancel();
    await session.close();
  }
}

Future<Map<String, Object?>> _pauseResume(int byteCount) async {
  if (Platform.isWindows) {
    return _windowsPauseResume(byteCount);
  }
  final (:session, :bytes) = await _readySession('output', ['$byteCount']);
  var received = 0;
  var invalid = false;
  final done = Completer<void>();
  late final StreamSubscription<Uint8List> subscription;
  subscription = bytes.remaining.listen(
    (chunk) {
      for (final byte in chunk) {
        final accepted = _acceptPatternByte(byte, received);
        invalid = invalid || accepted < 0;
        if (accepted >= 0) received += accepted;
      }
    },
    onError: done.completeError,
    onDone: done.complete,
  );
  subscription.pause();
  session.write(Uint8List.fromList(const [1]));
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

Future<Map<String, Object?>> _windowsTerminalOutput(
  String operation,
  int byteCount,
) async {
  final (:session, :bytes) = await _readySession(operation, ['$byteCount']);
  final marker = _MarkerTracker('PTYX-OUTPUT-OK $byteCount');
  var observed = 0;
  final stopwatch = Stopwatch()..start();
  try {
    session.write(Uint8List.fromList(const [1]));
    while (true) {
      final chunk = await bytes.readChunk().timeout(_timeout);
      if (chunk == null) break;
      observed += chunk.length;
      marker.add(chunk);
    }
    stopwatch.stop();
    if (!marker.matched) {
      throw StateError(
        'ConPTY output ended without the terminal-state report '
        'PTYX-OUTPUT-OK $byteCount',
      );
    }
    if (marker.trailingBytes != 0) {
      throw StateError(
        'ConPTY output contained ${marker.trailingBytes} trailing bytes',
      );
    }
    final exitCode = await session.exitCode.timeout(_timeout);
    return {
      'bytes': byteCount,
      'observed_transport_bytes': observed,
      'trailing_bytes': marker.trailingBytes,
      'elapsed_us': stopwatch.elapsedMicroseconds,
      'mib_per_second':
          byteCount / (1024 * 1024) / (stopwatch.elapsedMicroseconds / 1e6),
      'exit_code': exitCode,
      'exact_output_history_supported': false,
      'integrity_scope': 'child-reported generation and terminal final state',
    };
  } finally {
    await bytes.cancel();
    await session.close();
  }
}

Future<Map<String, Object?>> _windowsPauseResume(int byteCount) async {
  final (:session, :bytes) = await _readySession('output', ['$byteCount']);
  final marker = _MarkerTracker('PTYX-OUTPUT-OK $byteCount');
  final done = Completer<void>();
  var observed = 0;
  late final StreamSubscription<Uint8List> subscription;
  subscription = bytes.remaining.listen(
    (chunk) {
      observed += chunk.length;
      marker.add(chunk);
    },
    onError: done.completeError,
    onDone: done.complete,
  );
  subscription.pause();
  session.write(Uint8List.fromList(const [1]));
  final rssBefore = ProcessInfo.currentRss;
  await Future<void>.delayed(const Duration(milliseconds: 250));
  final rssWhilePaused = ProcessInfo.currentRss;
  final stopwatch = Stopwatch()..start();
  subscription.resume();
  try {
    await done.future.timeout(_timeout);
    stopwatch.stop();
    if (!marker.matched) {
      throw StateError(
        'paused ConPTY output omitted its terminal-state report',
      );
    }
    if (marker.trailingBytes != 0) {
      throw StateError(
        'paused ConPTY output contained ${marker.trailingBytes} trailing bytes',
      );
    }
    return {
      'bytes': byteCount,
      'observed_transport_bytes': observed,
      'trailing_bytes': marker.trailingBytes,
      'pause_ms': 250,
      'paused_rss_delta_bytes': rssWhilePaused - rssBefore,
      'resume_to_eof_us': stopwatch.elapsedMicroseconds,
      'exit_code': await session.exitCode.timeout(_timeout),
      'exact_output_history_supported': false,
      'integrity_scope': 'terminal final-state report after resume',
    };
  } finally {
    await subscription.cancel();
    await session.close();
  }
}

Future<Map<String, Object?>> _discardOutput(int byteCount) async {
  final (:session, :bytes) = await _readySession('output-raw', ['$byteCount']);
  final stopwatch = Stopwatch()..start();
  try {
    await bytes.cancel();
    session.write(Uint8List.fromList(const [1]));
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

Future<Map<String, Object?>> _noListener(int byteCount) async {
  final session = await _spawnFixture('output-raw', ['$byteCount']);
  final before = await _resourceSnapshot();
  try {
    session.write(Uint8List.fromList(const [1]));
    await Future<void>.delayed(const Duration(milliseconds: 250));
    final bounded = await _resourceSnapshot();
    final stopwatch = Stopwatch()..start();
    final output = session.output.listen(null);
    await output.cancel();
    final exitCode = await session.exitCode.timeout(_timeout);
    stopwatch.stop();
    return {
      'generated_bytes': byteCount,
      'unobserved_ms': 250,
      'resource_before': before,
      'resource_at_bound': bounded,
      'discard_to_exit_us': stopwatch.elapsedMicroseconds,
      'exit_code': exitCode,
    };
  } finally {
    await session.close();
  }
}

Future<Map<String, Object?>> _inputSaturation(int capacity) async {
  final temporary = Directory.systemTemp.createTempSync('ptyx-saturation-');
  final gate = File('${temporary.path}/release');
  PtySession? session;
  _ChunkReader? bytes;
  try {
    final activeSession = session = await _spawnFixture('gated-input-verify', [
      '${capacity * 2}',
      gate.path,
    ], capacity);
    final activeBytes = bytes = _ChunkReader(activeSession.output);
    for (final expected in ascii.encode('READY')) {
      if (await activeBytes.readByte().timeout(_timeout) != expected) {
        throw StateError('saturation child did not emit READY');
      }
    }
    final payload = Uint8List(capacity);
    _fillPattern(payload, 0, payload.length);
    final resourceBefore = await _resourceSnapshot();
    activeSession.write(payload);
    final resourceAtCapacity = await _resourceSnapshot();
    final second = Uint8List(capacity);
    _fillPattern(second, capacity, second.length);
    final stopwatch = Stopwatch()..start();
    try {
      activeSession.write(second);
      throw StateError('saturated input accepted beyond its byte budget');
    } on PtyBackpressureException {
      // Expected saturation is part of this benchmark's measured contract.
    }
    gate.createSync();
    await _writeAfterBackpressure(activeSession, second);
    stopwatch.stop();
    final resourceAfterRecovery = await _resourceSnapshot();
    final expectedReport = 'OK ${capacity * 2}';
    final report = await _readReport(activeBytes, expectedReport);
    if (report != expectedReport) {
      throw StateError('saturation integrity failure: $report');
    }
    return {
      'capacity_bytes': capacity,
      'release_handshake': 'external gate created after capacity snapshot',
      'capacity_recovery_us': stopwatch.elapsedMicroseconds,
      'resource_before': resourceBefore,
      'resource_at_capacity': resourceAtCapacity,
      'resource_after_recovery': resourceAfterRecovery,
      'report': report,
      'exit_code': await activeSession.exitCode.timeout(_timeout),
    };
  } finally {
    await bytes?.cancel();
    await session?.close();
    temporary.deleteSync(recursive: true);
  }
}

Future<void> _writeAfterBackpressure(
  PtySession session,
  Uint8List bytes,
) async {
  final deadline = DateTime.now().add(_timeout);
  while (true) {
    try {
      session.write(bytes);
      return;
    } on PtyBackpressureException {
      if (DateTime.now().isAfter(deadline)) {
        rethrow;
      }
      await Future<void>.delayed(Duration.zero);
    }
  }
}

Future<Map<String, Object?>> _fairness(
  int sessionCount,
  int repetitions,
) async {
  final pairs = <({PtySession session, _ChunkReader bytes})>[];
  try {
    for (var i = 0; i < sessionCount; i++) {
      pairs.add(
        await _readySession(Platform.isWindows ? 'input-report' : 'ready-cat'),
      );
    }
    final perSession = List.generate(sessionCount, (_) => <int>[]);
    for (var round = 0; round < repetitions; round++) {
      await Future.wait([
        for (var index = 0; index < pairs.length; index++)
          Future<void>(() async {
            final value = _interactiveByte(round + index);
            final stopwatch = Stopwatch()..start();
            await _writeAfterBackpressure(
              pairs[index].session,
              Uint8List.fromList([value]),
            );
            final received = Platform.isWindows
                ? await _readReport(
                    pairs[index].bytes,
                    'PTYX-INPUT ${round + 1} $value',
                  )
                : await pairs[index].bytes.readByte().timeout(_timeout);
            stopwatch.stop();
            if (Platform.isWindows
                ? received != 'PTYX-INPUT ${round + 1} $value'
                : received != value) {
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

Future<Map<String, Object?>> _activeOutputFairness(
  int sessionCount,
  int byteCount,
) async {
  if (Platform.isWindows) {
    final keepAlive = ReceivePort();
    try {
      final resourcesBefore = await _resourceSnapshot();
      final results = await Future.wait([
        for (var index = 0; index < sessionCount; index++)
          _windowsTerminalOutput('output', byteCount),
      ]);
      final throughputs = [
        for (final result in results) result['mib_per_second']! as double,
      ];
      final sortedThroughputs = [...throughputs]..sort();
      return {
        'sessions': sessionCount,
        'application_bytes_per_session': byteCount,
        'aggregate_mib_per_second': throughputs.reduce((a, b) => a + b),
        'slowest_to_fastest_ratio':
            sortedThroughputs.first / sortedThroughputs.last,
        'resource_before': resourcesBefore,
        'resource_busy': null,
        'integrity_scope': 'terminal final-state reports',
        'per_session': [
          for (var index = 0; index < results.length; index++)
            {'session': index, ...results[index]},
        ],
      };
    } finally {
      keepAlive.close();
    }
  }
  final pairs = <({PtySession session, _ChunkReader bytes})>[];
  try {
    for (var index = 0; index < sessionCount; index++) {
      pairs.add(await _readySession('output', ['$byteCount']));
    }
    final resourcesBefore = await _resourceSnapshot();
    for (final pair in pairs) {
      pair.session.write(Uint8List.fromList(const [1]));
    }
    final elapsed = List<int>.filled(sessionCount, 0);
    final transfers = [
      for (var sessionIndex = 0; sessionIndex < sessionCount; sessionIndex++)
        Future<void>(() async {
          final stopwatch = Stopwatch()..start();
          var received = 0;
          while (received < byteCount) {
            final chunk = await pairs[sessionIndex].bytes.readChunk().timeout(
              _timeout,
            );
            if (chunk == null) {
              throw StateError(
                'active session $sessionIndex reached EOF at $received',
              );
            }
            for (final byte in chunk) {
              final accepted = _acceptPatternByte(byte, received);
              if (accepted < 0) {
                throw StateError(
                  'active session $sessionIndex mismatch at $received',
                );
              }
              received += accepted;
            }
          }
          stopwatch.stop();
          elapsed[sessionIndex] = stopwatch.elapsedMicroseconds;
          await pairs[sessionIndex].session.exitCode.timeout(_timeout);
        }),
    ];
    await Future<void>.delayed(const Duration(milliseconds: 10));
    final resourcesBusy = await _resourceSnapshot();
    await Future.wait(transfers);
    final throughputs = [
      for (final micros in elapsed) byteCount / (1024 * 1024) / (micros / 1e6),
    ];
    final sortedThroughputs = [...throughputs]..sort();
    return {
      'sessions': sessionCount,
      'bytes_per_session': byteCount,
      'aggregate_mib_per_second': throughputs.reduce((a, b) => a + b),
      'slowest_to_fastest_ratio':
          sortedThroughputs.first / sortedThroughputs.last,
      'resource_busy': resourcesBusy,
      'resource_before': resourcesBefore,
      'per_session': [
        for (var index = 0; index < sessionCount; index++)
          {
            'session': index,
            'elapsed_us': elapsed[index],
            'mib_per_second': throughputs[index],
          },
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
  final resourceBefore = await _resourceSnapshot();
  final samples = <int>[];
  for (var i = 0; i < repetitions; i++) {
    final stopwatch = Stopwatch()..start();
    final session = await _spawnExit();
    await fixturePayload(session.output).drain<void>();
    final exit = session.exitCode;
    await exit.timeout(_timeout);
    await session.close().timeout(_timeout);
    stopwatch.stop();
    samples.add(stopwatch.elapsedMicroseconds);
  }
  await Future<void>.delayed(const Duration(milliseconds: 250));
  return {
    ..._distribution(samples),
    'resource_before': resourceBefore,
    'resource_after': await _resourceSnapshot(),
  };
}

Future<Map<String, Object?>> _observationOverhead(int repetitions) async {
  final (:session, :bytes) = await _readySession('ready-cat');
  try {
    final resize = Stopwatch()..start();
    for (var index = 0; index < repetitions; index++) {
      session.resize(
        PtySize(rows: 24 + (index & 1), columns: 80 + (index & 1)),
      );
    }
    resize.stop();
    final mode = Stopwatch()..start();
    var modeSamples = 0;
    if (session.capabilities.terminalModes) {
      for (var index = 0; index < repetitions; index++) {
        if (session.mode != null) modeSamples++;
      }
    }
    mode.stop();
    return {
      'repetitions': repetitions,
      'resize_total_us': resize.elapsedMicroseconds,
      'resize_mean_us': resize.elapsedMicroseconds / repetitions,
      'mode_samples': modeSamples,
      'mode_total_us': mode.elapsedMicroseconds,
      'mode_mean_us': modeSamples == 0
          ? null
          : mode.elapsedMicroseconds / modeSamples,
    };
  } finally {
    await bytes.cancel();
    await session.close();
  }
}

Future<Map<String, Object?>> _forcedClose() async {
  const windowsScript = r'''
$child = Start-Process `
  powershell.exe `
  -PassThru `
  -ArgumentList "-NoProfile -NonInteractive -Command Start-Sleep -Seconds 30"
[Console]::WriteLine($child.Id)
while ($true) { Start-Sleep -Seconds 1 }
''';
  const posixScript = r'''
(trap "" HUP TERM; sleep 30) & child=$!
printf "%s\n" "$child"
trap "" HUP TERM
while :; do sleep 1; done
''';
  final command = Platform.isWindows
      ? (
          executable:
              r'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe',
          arguments: const [
            '-NoProfile',
            '-NonInteractive',
            '-Command',
            windowsScript,
          ],
        )
      : (executable: '/bin/sh', arguments: const ['-c', posixScript]);
  final session = await PtySession.spawn(
    PtySpawnOptions(
      executable: command.executable,
      arguments: command.arguments,
      initialSize: _size,
      gracefulCloseTimeout: Duration.zero,
    ),
  );
  final lines = StreamIterator(
    fixturePayload(session.output)
        .map<List<int>>((chunk) => chunk)
        .transform(utf8.decoder)
        .transform(const LineSplitter()),
  );
  try {
    if (!await lines.moveNext().timeout(_timeout)) {
      throw StateError('forced-close child did not report a descendant');
    }
    final descendant = int.parse(lines.current.trim());
    final resourcesBefore = await _resourceSnapshot();
    final stopwatch = Stopwatch()..start();
    await session.close().timeout(_timeout);
    while (await _processExists(descendant) &&
        stopwatch.elapsed < const Duration(seconds: 10)) {
      await Future<void>.delayed(const Duration(milliseconds: 10));
    }
    stopwatch.stop();
    final descendantReclaimed = !await _processExists(descendant);
    if (!descendantReclaimed) {
      throw StateError('forced close retained descendant $descendant');
    }
    return {
      'descendant_pid': descendant,
      'close_and_descendant_reclaim_us': stopwatch.elapsedMicroseconds,
      'descendant_reclaimed': descendantReclaimed,
      'exit_code': await session.exitCode.timeout(_timeout),
      'resource_before': resourcesBefore,
      'resource_after': await _resourceSnapshot(),
    };
  } finally {
    await lines.cancel();
    await session.close();
  }
}

Future<Map<String, Object?>> _idleSessions(int count) async {
  final resourcesBefore = await _resourceSnapshot();
  final stopwatch = Stopwatch()..start();
  final sessions = <PtySession>[];
  final result = <String, Object?>{};
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
    await Future<void>.delayed(const Duration(milliseconds: 250));
    result.addAll({
      'requested_sessions': count,
      'created_sessions': sessions.length,
      'failure': failure?.toString(),
      'spawn_elapsed_us': stopwatch.elapsedMicroseconds,
      'resource_before': resourcesBefore,
      'resource_idle': failure == null ? await _resourceSnapshot() : null,
    });
    return result;
  } finally {
    for (final session in sessions.reversed) {
      await session.close().timeout(_timeout);
    }
    await Future<void>.delayed(const Duration(milliseconds: 250));
    result['resource_after'] = await _resourceSnapshot();
  }
}

Future<String> _readReport(_ChunkReader bytes, String expected) async {
  if (!Platform.isWindows) return (await _readLine(bytes)).trim();
  final result = StringBuffer();
  while (result.length < expected.length) {
    final value = await bytes.readByte().timeout(_timeout);
    if (value == null) {
      throw StateError(
        'child reached EOF before reporting ${expected.length} bytes',
      );
    }
    result.writeCharCode(value);
  }
  return result.toString();
}

Future<String> _readLine(_ChunkReader bytes) async {
  final result = StringBuffer();
  while (true) {
    final value = await bytes.readByte().timeout(_timeout);
    if (value == null) {
      throw StateError('child reached EOF before reporting a line');
    }
    if (value == 10 || value == 13) {
      if (result.isNotEmpty) return result.toString();
    } else {
      result.writeCharCode(value);
    }
  }
}

Future<Map<String, Object?>> _resourceSnapshot() async {
  if (Platform.isWindows) {
    final result = await Process.run(
      r'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe',
      [
        '-NoProfile',
        '-NonInteractive',
        '-Command',
        r'''
$rootPid = [uint32]$env:PTYX_RESOURCE_ROOT_PID
$all = Get-CimInstance Win32_Process
$ids = [System.Collections.Generic.HashSet[uint32]]::new()
[void]$ids.Add($rootPid)
do {
  $before = $ids.Count
  foreach ($process in $all) {
    if ($ids.Contains([uint32]$process.ParentProcessId)) {
      [void]$ids.Add([uint32]$process.ProcessId)
    }
  }
} while ($ids.Count -ne $before)
[void]$ids.Remove([uint32]$PID)
$processes = Get-Process -Id @($ids) -ErrorAction SilentlyContinue
$cpu = ($processes | Measure-Object CPU -Sum).Sum
$rss = ($processes | Measure-Object WorkingSet64 -Sum).Sum
$handles = ($processes | Measure-Object HandleCount -Sum).Sum
"$([int64]($cpu * 1000000))|$([int64]$rss)|$([int64]$handles)|$($processes.Count)"
''',
      ],
      environment: {'PTYX_RESOURCE_ROOT_PID': '$pid'},
    );
    if (result.exitCode != 0) {
      throw StateError('Windows resource query failed: ${result.stderr}');
    }
    final fields = '${result.stdout}'.trim().split('|').map(int.parse).toList();
    return {
      'tree_cpu_us': fields[0],
      'tree_rss_bytes': fields[1],
      'tree_handles': fields[2],
      'tree_processes': fields[3],
      'dart_threads': await _threadCount(),
    };
  }
  final sampler = await Process.start('ps', const [
    '-axo',
    'pid=,ppid=,rss=,time=',
  ]);
  final stdout = sampler.stdout.transform(utf8.decoder).join();
  final stderr = sampler.stderr.transform(utf8.decoder).join();
  final exitCode = await sampler.exitCode;
  final output = await stdout;
  final errorOutput = await stderr;
  if (exitCode != 0) {
    throw StateError('ps resource query failed: $errorOutput');
  }
  final entries = <int, ({int parent, int rssKiB, int cpuUs})>{};
  for (final line in const LineSplitter().convert(output)) {
    final fields = line.trim().split(RegExp(r'\s+'));
    if (fields.length != 4) continue;
    final process = int.tryParse(fields[0]);
    final parent = int.tryParse(fields[1]);
    final rss = int.tryParse(fields[2]);
    if (process == null || parent == null || rss == null) continue;
    if (process == sampler.pid) continue;
    entries[process] = (
      parent: parent,
      rssKiB: rss,
      cpuUs: _parseCpuMicros(fields[3]),
    );
  }
  final tree = <int>{pid};
  var changed = true;
  while (changed) {
    changed = false;
    for (final entry in entries.entries) {
      if (tree.contains(entry.value.parent) && tree.add(entry.key)) {
        changed = true;
      }
    }
  }
  final treeEntries = tree
      .map((process) => entries[process])
      .whereType<({int parent, int rssKiB, int cpuUs})>();
  final liveTree = tree.where(entries.containsKey).toSet();
  final descriptors = await _unixDescriptorCount(liveTree);
  return {
    'tree_cpu_us': treeEntries.fold<int>(
      0,
      (total, entry) => total + entry.cpuUs,
    ),
    'tree_rss_bytes': treeEntries.fold<int>(
      0,
      (total, entry) => total + entry.rssKiB * 1024,
    ),
    'tree_descriptors': descriptors,
    'tree_processes': liveTree.length,
    'dart_threads': await _threadCount(),
  };
}

Future<int> _unixDescriptorCount(Set<int> processes) async {
  if (Platform.isLinux) {
    var total = 0;
    for (final process in processes) {
      try {
        total += await Directory(
          '/proc/$process/fd',
        ).list(followLinks: false).length;
      } on FileSystemException {
        // A short-lived child may exit between the process and descriptor
        // snapshots. It contributes no live descriptors at the later instant.
      }
    }
    return total;
  }
  final result = await Process.run('/usr/sbin/lsof', [
    '-a',
    '-p',
    processes.join(','),
    '-Fn',
  ]);
  // lsof exits 1 if any process exits between the ps and lsof snapshots while
  // still emitting complete records for the processes that remain alive.
  if (result.exitCode != 0 && '${result.stdout}'.trim().isEmpty) {
    throw StateError('lsof descriptor query failed: ${result.stderr}');
  }
  return const LineSplitter()
      .convert('${result.stdout}')
      .where((line) => line.startsWith('f'))
      .length;
}

int _parseCpuMicros(String value) {
  final dayParts = value.split('-');
  final days = dayParts.length == 2 ? int.parse(dayParts[0]) : 0;
  final clock = dayParts.last.split(':').map(double.parse).toList();
  var seconds = 0.0;
  for (final part in clock) {
    seconds = seconds * 60 + part;
  }
  return ((days * 86400 + seconds) * 1e6).round();
}

Future<bool> _processExists(int process) async {
  if (Platform.isWindows) {
    final script =
        '''
if (Get-Process -Id $process -ErrorAction SilentlyContinue) {
  exit 0
} else {
  exit 1
}''';
    final result = await Process.run(
      r'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe',
      ['-NoProfile', '-NonInteractive', '-Command', script],
    );
    return result.exitCode == 0;
  }
  if ((await Process.run('/bin/kill', ['-0', '$process'])).exitCode != 0) {
    return false;
  }
  final state = await Process.run('/bin/ps', [
    '-o',
    'state=',
    '-p',
    '$process',
  ]);
  return state.exitCode == 0 &&
      !(state.stdout as String).trimLeft().startsWith('Z');
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
    return '${result.stdout}'.trim();
  } on ProcessException {
    return null;
  }
}

Future<String> _fileHash(String path) async {
  return sha256.convert(await File(path).readAsBytes()).toString();
}

Future<String?> _workingTreeHash() async {
  final status = await Process.run('git', const [
    'status',
    '--porcelain=v1',
    '-z',
    '--untracked-files=all',
  ], stdoutEncoding: null);
  final diff = await Process.run('git', const [
    'diff',
    '--binary',
    'HEAD',
  ], stdoutEncoding: null);
  if (status.exitCode != 0 || diff.exitCode != 0) {
    return null;
  }
  final statusBytes = status.stdout! as List<int>;
  final bytes = BytesBuilder(copy: false)
    ..add(statusBytes)
    ..add(diff.stdout! as List<int>);
  for (final entry in _nulSeparated(statusBytes)) {
    if (!entry.startsWith('?? ')) continue;
    final path = entry.substring(3);
    final file = File(path);
    if (!file.existsSync()) continue;
    bytes
      ..add(utf8.encode(path))
      ..add(utf8.encode(sha256.convert(file.readAsBytesSync()).toString()));
  }
  return sha256.convert(bytes.takeBytes()).toString();
}

Iterable<String> _nulSeparated(List<int> bytes) sync* {
  var start = 0;
  for (var index = 0; index < bytes.length; index++) {
    if (bytes[index] != 0) continue;
    yield utf8.decode(bytes.sublist(start, index));
    start = index + 1;
  }
}

final class _MarkerTracker {
  final List<int> _marker;
  var _matchedBytes = 0;
  final _trailing = _TrailingLineEnding();

  _MarkerTracker(String marker) : _marker = ascii.encode(marker);

  bool get matched => _matchedBytes == _marker.length;

  int get trailingBytes => _trailing.invalidBytes;

  void add(List<int> bytes) {
    if (matched) {
      for (final byte in bytes) {
        _trailing.add(byte);
      }
      return;
    }
    for (var index = 0; index < bytes.length; index++) {
      final byte = bytes[index];
      if (byte == _marker[_matchedBytes]) {
        _matchedBytes++;
        if (matched) {
          for (final trailing in bytes.skip(index + 1)) {
            _trailing.add(trailing);
          }
          return;
        }
      } else {
        _matchedBytes = byte == _marker.first ? 1 : 0;
      }
    }
  }
}

/// Allows the line terminator emitted by a text-mode child report while
/// rejecting every other byte after the exact marker.
final class _TrailingLineEnding {
  var _state = 0;
  var _invalid = 0;

  int get invalidBytes => _invalid;

  void add(int byte) {
    switch (_state) {
      case 0:
        if (byte == 13) {
          _state = 1;
        } else if (byte == 10) {
          _state = 2;
        } else {
          _invalid++;
        }
      case 1:
        if (byte == 10) {
          _state = 2;
        } else {
          _invalid++;
          _state = 2;
        }
      case 2:
        _invalid++;
    }
  }
}

int _pattern(int offset) => 32 + ((offset * 31 + 17) % 95);

int _interactiveByte(int offset) =>
    Platform.isWindows ? 33 + (offset % 94) : 32 + (offset % 95);

int _acceptPatternByte(int byte, int offset) {
  if (byte == _pattern(offset)) return 1;
  return -1;
}

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

  _ChunkReader(Stream<Uint8List> stream)
    : _chunks = StreamIterator(
        Platform.isWindows ? fixturePayload(stream, discardC0: true) : stream,
      );

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
