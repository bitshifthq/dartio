import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:crypto/crypto.dart';
import 'package:ptyx/ptyx.dart';

import '../benchmark/progress.dart';
import '../benchmark/vt_payload.dart';

const _size = PtySize(rows: 24, columns: 80);
const _operationTimeout = Duration(seconds: 30);
const _ready = [82, 69, 65, 68, 89];
const _maximumReadinessPrelude = 64 * 1024;
// Dart VM worker-pool sampling can vary by one or two transient threads even
// after every PTY-owned descriptor and process has returned to baseline.
const _threadGrowthBudget = 2;
const _windowsHandleGrowthBudget = 8;

Stream<Uint8List> _fixturePayload(PtySession session) => Platform.isWindows
    ? fixturePayload(session.output, discardC0: true)
    : session.output;

Future<void> main(List<String> arguments) async {
  final durationArgument = arguments
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
  final duration = durationArgument == null
      ? const Duration(minutes: 5)
      : Duration(seconds: int.parse(durationArgument));
  final startedAt = DateTime.now().toUtc();
  final revision = await _commandOutput('git', const ['rev-parse', 'HEAD']);
  final status = await _commandOutput('git', const [
    'status',
    '--porcelain=v1',
    '--untracked-files=all',
  ]);
  if (revision == null || status == null) {
    throw StateError('soak retention requires a readable Git revision');
  }
  await progress.record('provenance-read');
  final dirty = status.isNotEmpty;
  final fixtureExecutable = Platform.environment['PTYX_FIXTURE_EXECUTABLE'];
  await progress.record('warm-cycle-started');
  await _runCycle(-1);
  await progress.record('warm-cycle-completed');
  await progress.record('long-lived-warmup-started');
  await _warmLongLivedSession();
  await progress.record('long-lived-warmup-completed');
  final resourceBefore = await _resourceSnapshot();
  await progress.record('baseline-captured');
  final resourceSamples = <Map<String, Object?>>[];
  var cycles = 0;
  var bytes = 0;
  var longLivedBytes = 0;
  final longSession = await _spawnFixture(
    Platform.isWindows ? 'input-report' : 'ready-cat',
    0,
  );
  final longOutput = StreamIterator(
    _fixturePayload(longSession).expand((chunk) => chunk),
  );
  await _expectReady(longOutput);
  await progress.record('long-lived-session-ready');
  final deadline = DateTime.now().add(duration);
  await progress.record('cycle-loop-started');
  try {
    while (DateTime.now().isBefore(deadline)) {
      bytes += await _runCycle(cycles);
      final interactiveLength = Platform.isWindows ? 1 : 1024;
      final interactive = Uint8List(interactiveLength)
        ..setAll(
          0,
          List<int>.generate(
            interactiveLength,
            (index) => Platform.isWindows
                ? 33 + ((cycles * interactiveLength + index) % 94)
                : 32 + ((cycles * interactiveLength + index) % 95),
          ),
        );
      longSession.resize(
        PtySize(rows: 24 + cycles % 8, columns: 80 + cycles % 16),
      );
      await _writeFixtureInput(longSession, interactive);
      if (Platform.isWindows) {
        await _expectReport(
          longOutput,
          'PTYX-INPUT ${cycles + 1} ${interactive.single}',
          cycles,
        );
      } else {
        for (var index = 0; index < interactive.length; index++) {
          if (!await longOutput.moveNext().timeout(_operationTimeout)) {
            throw StateError(
              'long-lived session ended in cycle $cycles at $index',
            );
          }
          if (longOutput.current != interactive[index]) {
            throw StateError(
              'long-lived session mismatch in cycle $cycles at $index',
            );
          }
        }
      }
      longLivedBytes += interactive.length;
      cycles++;
      if (cycles % 50 == 0) {
        resourceSamples.add({
          'elapsed_seconds': DateTime.now().difference(startedAt).inSeconds,
          ...await _resourceSnapshot(),
        });
        await progress.record(
          'cycle-checkpoint',
          details: {'cycles': cycles, 'verified_bytes': bytes},
        );
      }
    }
  } finally {
    await longOutput.cancel();
    await longSession.close().timeout(_operationTimeout);
  }
  await progress.record(
    'cycle-loop-completed',
    details: {'cycles': cycles, 'verified_bytes': bytes},
  );
  await progress.record('resource-stabilization-started');
  final stabilization = await _waitForResourceStability(resourceBefore);
  await progress.record('resource-stabilization-completed');
  final resourceAfter = stabilization.snapshot;
  final threadsWithinGrowthBudget =
      (resourceAfter['tree_threads']! as int) <=
      (resourceBefore['tree_threads']! as int) + _threadGrowthBudget;
  final resourceGrowthBudget = Platform.isWindows
      ? _windowsHandleGrowthBudget
      : 0;
  final resourceUnitsWithinGrowthBudget =
      _resourceUnits(resourceAfter) <=
      _resourceUnits(resourceBefore) + resourceGrowthBudget;
  const cleanupRssGrowthBudget = 32 * 1024 * 1024;
  final cleanupRssWithinBudget =
      (resourceAfter['tree_rss_bytes']! as int) <=
      (resourceBefore['tree_rss_bytes']! as int) + cleanupRssGrowthBudget;
  final steadyRssGrowthBudget =
      (fixtureExecutable == null ? 256 : 64) * 1024 * 1024;
  final peakTreeRss = [resourceBefore, ...resourceSamples, resourceAfter]
      .map((sample) => sample['tree_rss_bytes']! as int)
      .reduce((maximum, value) => value > maximum ? value : maximum);
  final peakRssGrowth =
      peakTreeRss - (resourceBefore['tree_rss_bytes']! as int);
  final steadyRssWithinBudget = peakRssGrowth <= steadyRssGrowthBudget;
  final cleanupPassed =
      stabilization.stable && cleanupRssWithinBudget && steadyRssWithinBudget;
  final encoded = jsonEncode({
    'schema': 1,
    'suite': 'ptyx-exact-integrity-soak',
    'revision': revision,
    'tree_dirty': dirty,
    'working_tree_sha256': dirty ? await _workingTreeHash() : null,
    'command': arguments,
    'platform': Platform.operatingSystem,
    'platform_version': Platform.operatingSystemVersion,
    'architecture': await _architecture(),
    'dart_version': Platform.version,
    'fixture_source_sha256': await _fileHash(
      Platform.script.resolve('../benchmark/fixture.dart').toFilePath(),
    ),
    'fixture_executable': fixtureExecutable,
    'fixture_executable_sha256': fixtureExecutable == null
        ? null
        : await _fileHash(fixtureExecutable),
    'started_at_utc': startedAt.toIso8601String(),
    'finished_at_utc': DateTime.now().toUtc().toIso8601String(),
    'duration_seconds': duration.inSeconds,
    'cycles': cycles,
    'verified_bytes': bytes,
    'long_lived_verified_bytes': longLivedBytes,
    'resource_before': resourceBefore,
    'resource_samples': resourceSamples,
    'resource_sampling_cycle_interval': 50,
    'resource_after': resourceAfter,
    'resource_stabilization_ms': stabilization.elapsed.inMilliseconds,
    'resource_counts_stabilized': stabilization.stable,
    'thread_growth_budget': _threadGrowthBudget,
    'threads_within_growth_budget': threadsWithinGrowthBudget,
    'resource_unit_growth_budget': resourceGrowthBudget,
    'resource_units_within_growth_budget': resourceUnitsWithinGrowthBudget,
    'peak_tree_rss_bytes': peakTreeRss,
    'rss_peak_kind': 'sampled-steady-state',
    'peak_rss_growth_bytes': peakRssGrowth,
    'steady_rss_growth_budget_bytes': steadyRssGrowthBudget,
    'steady_rss_within_growth_budget': steadyRssWithinBudget,
    'cleanup_rss_growth_budget_bytes': cleanupRssGrowthBudget,
    'cleanup_rss_within_growth_budget': cleanupRssWithinBudget,
    'cleanup_passed': cleanupPassed,
  });
  await progress.record('final-artifact-writing');
  if (outputPath != null) {
    await File(outputPath).writeAsString('$encoded\n', flush: true);
  }
  stdout.writeln(encoded);
  await stdout.flush();
  await progress.record('completed');
  if (!cleanupPassed) {
    exitCode = 1;
  }
}

Future<void> _warmLongLivedSession() async {
  final session = await _spawnFixture(
    Platform.isWindows ? 'input-report' : 'ready-cat',
    0,
  );
  final output = StreamIterator(
    _fixturePayload(session).expand((chunk) => chunk),
  );
  try {
    await _expectReady(output);
    await _writeFixtureInput(session, Uint8List.fromList(const [65]));
    if (Platform.isWindows) {
      await _expectReport(output, 'PTYX-INPUT 1 65', -1);
    } else {
      if (!await output.moveNext().timeout(_operationTimeout) ||
          output.current != 65) {
        throw StateError('long-lived warmup mismatch');
      }
    }
  } finally {
    await output.cancel();
    await session.close().timeout(_operationTimeout);
  }
}

Future<int> _runCycle(int cycle) async {
  const byteCount = 64 * 1024;
  final session = await _spawnFixture(
    Platform.isWindows ? 'input-verify' : 'echo-count',
    byteCount,
  );
  final iterator = StreamIterator(
    _fixturePayload(session).expand((chunk) => chunk),
  );
  try {
    await _expectReady(iterator);
    final input = Uint8List(byteCount)
      ..setAll(
        0,
        List<int>.generate(byteCount, (index) => 32 + ((index * 31 + 17) % 95)),
      );
    final outputDone = Platform.isWindows
        ? _expectReport(iterator, 'OK $byteCount', cycle)
        : Future<void>(() async {
            for (var index = 0; index < input.length; index++) {
              if (!await iterator.moveNext().timeout(_operationTimeout)) {
                throw StateError('cycle $cycle ended at $index');
              }
              if (iterator.current != input[index]) {
                throw StateError('cycle $cycle byte mismatch at $index');
              }
            }
          });
    await _writeFixtureInput(session, input);
    await outputDone.timeout(_operationTimeout);
    final exitCode = await session.exitCode.timeout(_operationTimeout);
    if (exitCode != 0) {
      throw StateError('cycle $cycle child exited with $exitCode');
    }
    return input.length;
  } finally {
    await iterator.cancel();
    await session.close().timeout(_operationTimeout);
  }
}

Future<void> _expectReport(
  StreamIterator<int> iterator,
  String report,
  int cycle,
) async {
  final expected = utf8.encode(report);
  var matched = 0;
  while (await iterator.moveNext().timeout(_operationTimeout)) {
    final byte = iterator.current;
    if (byte == expected[matched]) {
      matched++;
      if (matched == expected.length) return;
    } else {
      matched = byte == expected.first ? 1 : 0;
    }
  }
  throw StateError('cycle $cycle ended before child report $report');
}

Future<void> _writeFixtureInput(PtySession session, Uint8List bytes) async {
  final deadline = DateTime.now().add(_operationTimeout);
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

Future<PtySession> _spawnFixture(String operation, int byteCount) async {
  final fixture = Platform.environment['PTYX_FIXTURE_EXECUTABLE'];
  final session = await PtySession.spawn(
    PtySpawnOptions(
      executable: fixture ?? Platform.resolvedExecutable,
      arguments: [
        if (fixture == null)
          Platform.script.resolve('../benchmark/fixture.dart').toFilePath(),
        operation,
        if (byteCount != 0) '$byteCount',
      ],
      initialSize: _size,
      maxBufferedInput: 64 * 1024,
      maxBufferedOutput: 64 * 1024,
    ),
  ).timeout(_operationTimeout);
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

Future<({Map<String, Object?> snapshot, bool stable, Duration elapsed})>
_waitForResourceStability(Map<String, Object?> baseline) async {
  final stopwatch = Stopwatch()..start();
  late Map<String, Object?> snapshot;
  var stable = false;
  do {
    await Future<void>.delayed(const Duration(milliseconds: 100));
    snapshot = await _resourceSnapshot();
    final resourceGrowthBudget = Platform.isWindows
        ? _windowsHandleGrowthBudget
        : 0;
    stable =
        _resourceUnits(snapshot) <=
            _resourceUnits(baseline) + resourceGrowthBudget &&
        (snapshot['tree_processes']! as int) <=
            (baseline['tree_processes']! as int) &&
        (snapshot['tree_threads']! as int) <=
            (baseline['tree_threads']! as int) + _threadGrowthBudget;
  } while (!stable && stopwatch.elapsed < const Duration(seconds: 3));
  stopwatch.stop();
  return (snapshot: snapshot, stable: stable, elapsed: stopwatch.elapsed);
}

int _resourceUnits(Map<String, Object?> snapshot) =>
    snapshot[(Platform.isWindows ? 'tree_handles' : 'tree_descriptors')]!
        as int;

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
    # Exclude this sampler before walking descendants. Including it and then
    # removing only its PID can retain transient CIM helper processes.
    if ([uint32]$process.ProcessId -eq [uint32]$PID) {
      continue
    }
    if ($ids.Contains([uint32]$process.ParentProcessId)) {
      [void]$ids.Add([uint32]$process.ProcessId)
    }
  }
} while ($ids.Count -ne $before)
# Aggregate the same CIM snapshot used to establish ancestry. Resolving the
# collected numeric IDs again with Get-Process can attach a recycled child PID
# to an unrelated process while short-lived PTY children are churning.
$processes = @($all | Where-Object {
  $ids.Contains([uint32]$_.ProcessId) -and
    [uint32]$_.ProcessId -ne [uint32]$PID
})
[uint64]$cpu100ns = 0
[uint64]$rss = 0
[uint64]$handles = 0
[uint64]$threads = 0
foreach ($process in $processes) {
  $cpu100ns += [uint64]$process.KernelModeTime
  $cpu100ns += [uint64]$process.UserModeTime
  $rss += [uint64]$process.WorkingSetSize
  $handles += [uint64]$process.HandleCount
  $threads += [uint64]$process.ThreadCount
}
"$([int64]($cpu100ns / 10))|$([int64]$rss)|$([int64]$handles)|$([int64]$threads)|$($processes.Count)"
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
      'tree_threads': fields[3],
      'tree_processes': fields[4],
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
  final liveTree = tree.where(entries.containsKey).toSet();
  final treeEntries = liveTree.map((process) => entries[process]!);
  return {
    'tree_cpu_us': treeEntries.fold<int>(
      0,
      (total, entry) => total + entry.cpuUs,
    ),
    'tree_rss_bytes': treeEntries.fold<int>(
      0,
      (total, entry) => total + entry.rssKiB * 1024,
    ),
    'tree_descriptors': await _unixDescriptorCount(liveTree),
    'tree_threads': await _unixThreadCount(liveTree),
    'tree_processes': liveTree.length,
  };
}

Future<int> _unixDescriptorCount(Set<int> processes) async {
  if (Platform.isLinux) {
    var total = 0;
    for (final process in processes) {
      try {
        total += Directory(
          '/proc/$process/fd',
        ).listSync(followLinks: false).length;
      } on FileSystemException {
        // A descendant can exit between the process and descriptor snapshots.
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
  if (result.exitCode != 0 && '${result.stdout}'.trim().isEmpty) {
    throw StateError('lsof descriptor query failed: ${result.stderr}');
  }
  return const LineSplitter()
      .convert('${result.stdout}')
      .where((line) => line.startsWith('f'))
      .length;
}

Future<int> _unixThreadCount(Set<int> processes) async {
  if (Platform.isLinux) {
    var total = 0;
    for (final process in processes) {
      try {
        total += Directory('/proc/$process/task').listSync().length;
      } on FileSystemException {
        // A descendant can exit between the process and thread snapshots.
      }
    }
    return total;
  }
  final result = await Process.run('ps', ['-M', '-p', processes.join(',')]);
  if (result.exitCode != 0) {
    throw StateError('thread query failed: ${result.stderr}');
  }
  return const LineSplitter()
          .convert('${result.stdout}')
          .where((line) => line.trim().isNotEmpty)
          .length -
      1;
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

Future<String?> _commandOutput(
  String executable,
  List<String> arguments,
) async {
  final result = await Process.run(executable, arguments);
  return result.exitCode == 0 ? '${result.stdout}'.trim() : null;
}

Future<String> _fileHash(String path) async =>
    sha256.convert(await File(path).readAsBytes()).toString();

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

Future<String?> _architecture() {
  if (Platform.isWindows) {
    return Future.value(
      Platform.environment['PROCESSOR_ARCHITEW6432'] ??
          Platform.environment['PROCESSOR_ARCHITECTURE'],
    );
  }
  return _commandOutput('uname', const ['-m']);
}
