import 'dart:async';
import 'dart:io';
import 'dart:isolate';
import 'dart:typed_data';

import 'package:ptyx/ptyx.dart';
import 'package:test/test.dart';

import '../ffi/ptyx_test.g.dart';

Future<void> _ownQuietSession(SendPort ready) async {
  final session = await PtySession.spawn(
    PtySpawnOptions(
      executable: Platform.isWindows
          ? r'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe'
          : '/bin/sh',
      arguments: Platform.isWindows
          ? const [
              '-NoProfile',
              '-NonInteractive',
              '-Command',
              'Start-Sleep -Seconds 30',
            ]
          : const ['-c', 'exec sleep 30'],
      initialSize: const PtySize(rows: 24, columns: 80),
    ),
  );
  final keepAlive = ReceivePort();
  ready.send(session.pid);
  await keepAlive.first;
}

Future<void> _closeResistantSession(SendPort ready) async {
  final session = await PtySession.spawn(
    PtySpawnOptions(
      executable: Platform.isWindows
          ? r'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe'
          : '/bin/sh',
      arguments: Platform.isWindows
          ? const [
              '-NoProfile',
              '-NonInteractive',
              '-Command',
              r'while ($true) { Start-Sleep -Seconds 1 }',
            ]
          : const ['-c', "trap '' HUP TERM; while :; do sleep 1; done"],
      initialSize: const PtySize(rows: 24, columns: 80),
      gracefulCloseTimeout: const Duration(seconds: 30),
    ),
  );
  final pid = session.pid;
  unawaited(session.close());
  final keepAlive = ReceivePort();
  ready.send(pid);
  await keepAlive.first;
}

Future<int> _spawnAndDropQuietSession() async {
  final session = await PtySession.spawn(
    PtySpawnOptions(
      executable: Platform.isWindows
          ? r'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe'
          : '/bin/sh',
      arguments: Platform.isWindows
          ? const [
              '-NoProfile',
              '-NonInteractive',
              '-Command',
              'Start-Sleep -Seconds 30',
            ]
          : const ['-c', 'exec sleep 30'],
      initialSize: const PtySize(rows: 24, columns: 80),
    ),
  );
  return session.pid!;
}

Future<void> _collectDroppedSession(SendPort reports) async {
  final pid = await _spawnAndDropQuietSession();
  reports.send(pid);
  final retained = <Uint8List>[];
  final deadline = DateTime.now().add(const Duration(seconds: 10));
  while (await _processExists(pid) && DateTime.now().isBefore(deadline)) {
    retained.add(Uint8List(1024 * 1024));
    if (retained.length == 16) {
      retained.clear();
    }
    await Future<void>.delayed(const Duration(milliseconds: 10));
  }
  reports.send(!await _processExists(pid));
}

Future<void> _loseDuringStagedSpawn((SendPort, String) message) async {
  final (ready, pidFile) = message;
  ptyd_test_delay_next_spawn(1000);
  ready.send(null);
  await PtySession.spawn(
    PtySpawnOptions(
      executable: Platform.isWindows
          ? r'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe'
          : '/bin/sh',
      arguments: Platform.isWindows
          ? const [
              '-NoProfile',
              '-NonInteractive',
              '-Command',
              r'''
$temporaryPidFile = "$env:PTYX_STAGE_PID_FILE.tmp"
[System.IO.File]::WriteAllText($temporaryPidFile, "$PID")
[System.IO.File]::Move($temporaryPidFile, $env:PTYX_STAGE_PID_FILE)
Start-Sleep -Seconds 30
''',
            ]
          : const [
              '-c',
              r'printf %s "$$" > "$PTYX_STAGE_PID_FILE"; exec sleep 30',
            ],
      environment: {'PTYX_STAGE_PID_FILE': pidFile},
      initialSize: const PtySize(rows: 24, columns: 80),
    ),
  );
}

Future<void> _loseDuringGuardianStartup(SendPort ready) async {
  ptyd_test_delay_next_attach(1000);
  ready.send(null);
  await PtySession.spawn(
    PtySpawnOptions(
      executable: Platform.isWindows
          ? r'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe'
          : '/bin/sh',
      arguments: Platform.isWindows
          ? const [
              '-NoProfile',
              '-NonInteractive',
              '-Command',
              'Start-Sleep -Seconds 30',
            ]
          : const ['-c', 'exec sleep 30'],
      initialSize: const PtySize(rows: 24, columns: 80),
    ),
  );
}

Future<bool> _processExists(int pid) async {
  if (!Platform.isWindows) {
    return (await Process.run('/bin/kill', ['-0', '$pid'])).exitCode == 0;
  }
  final result = await Process.run('powershell.exe', [
    '-NoProfile',
    '-NonInteractive',
    '-Command',
    '''
if (Get-Process -Id $pid -ErrorAction SilentlyContinue) {
  exit 0
} else {
  exit 1
}''',
  ]);
  return result.exitCode == 0;
}

void main() {
  group('native ownership after Dart loss', () {
    test('unarmed guardians expire after immediate owner loss', () async {
      final script = File(
        'test/src/api/guardian_startup_probe.dart',
      ).absolute.path;
      final process = await Process.start(Platform.resolvedExecutable, [
        script,
      ]);
      final stderrFuture = process.stderr
          .transform(systemEncoding.decoder)
          .join();
      var exited = false;
      try {
        final exitCode = await process.exitCode.timeout(
          const Duration(seconds: 20),
        );
        exited = true;
        expect(exitCode, 0, reason: await stderrFuture);
      } finally {
        if (!exited) {
          process.kill();
        }
      }
    });

    test('guardian owns runtime creation before the owner can exit', () async {
      final baseline = ptyd_test_adapter_count();
      final ready = ReceivePort();
      final isolate = await Isolate.spawn(
        _loseDuringGuardianStartup,
        ready.sendPort,
      );
      addTearDown(() => isolate.kill(priority: Isolate.immediate));
      await ready.first.timeout(const Duration(seconds: 10));
      ready.close();

      final activeDeadline = DateTime.now().add(const Duration(seconds: 5));
      while (ptyd_test_attach_delay_active() == 0 &&
          DateTime.now().isBefore(activeDeadline)) {
        await Future<void>.delayed(const Duration(milliseconds: 10));
      }
      expect(
        ptyd_test_attach_delay_active(),
        1,
        reason: 'owner must be killed while native attachment is in progress',
      );
      isolate.kill(priority: Isolate.immediate);

      final cleanupDeadline = DateTime.now().add(const Duration(seconds: 10));
      while ((ptyd_test_attach_delay_active() != 0 ||
              ptyd_test_adapter_count() != baseline) &&
          DateTime.now().isBefore(cleanupDeadline)) {
        await Future<void>.delayed(const Duration(milliseconds: 20));
      }
      expect(ptyd_test_attach_delay_active(), 0);
      expect(ptyd_test_adapter_count(), baseline);
    });

    test('reclaims an unreachable session in a live isolate', () async {
      final reports = ReceivePort();
      final messages = StreamIterator(reports);
      final isolate = await Isolate.spawn(
        _collectDroppedSession,
        reports.sendPort,
      );
      addTearDown(() {
        isolate.kill(priority: Isolate.immediate);
        reports.close();
      });

      expect(
        await messages.moveNext().timeout(const Duration(seconds: 10)),
        isTrue,
      );
      final pid = messages.current as int;
      expect(
        await messages.moveNext().timeout(const Duration(seconds: 15)),
        isTrue,
      );
      expect(messages.current, isTrue, reason: 'child $pid was not reclaimed');
      await messages.cancel();
    });

    test('isolate loss abandons and reclaims a quiet session', () async {
      final ready = ReceivePort();
      final isolate = await Isolate.spawn(_ownQuietSession, ready.sendPort);
      addTearDown(() => isolate.kill(priority: Isolate.immediate));
      final pid = await ready.first.timeout(const Duration(seconds: 10)) as int;
      ready.close();

      isolate.kill(priority: Isolate.immediate);

      final deadline = DateTime.now().add(const Duration(seconds: 10));
      while (await _processExists(pid) && DateTime.now().isBefore(deadline)) {
        await Future<void>.delayed(const Duration(milliseconds: 20));
      }
      expect(await _processExists(pid), isFalse);
    });

    test('isolate loss during staged spawn cannot orphan a child', () async {
      final temporary = await Directory.systemTemp.createTemp(
        'ptyx-staged-spawn-',
      );
      addTearDown(() => temporary.delete(recursive: true));
      final pidFile = File('${temporary.path}/pid');
      final ready = ReceivePort();
      final isolate = await Isolate.spawn(_loseDuringStagedSpawn, (
        ready.sendPort,
        pidFile.path,
      ));
      addTearDown(() => isolate.kill(priority: Isolate.immediate));
      await ready.first.timeout(const Duration(seconds: 10));
      ready.close();

      final publicationDeadline = DateTime.now().add(
        const Duration(seconds: 5),
      );
      while (!pidFile.existsSync() &&
          DateTime.now().isBefore(publicationDeadline)) {
        await Future<void>.delayed(const Duration(milliseconds: 10));
      }
      expect(pidFile.existsSync(), isTrue);
      final pid = int.parse(pidFile.readAsStringSync());
      expect(
        ptyd_test_spawn_delay_active() != 0,
        isTrue,
        reason: 'owner must be killed before staged spawn returns',
      );

      isolate.kill(priority: Isolate.immediate);

      final cleanupDeadline = DateTime.now().add(const Duration(seconds: 10));
      while (await _processExists(pid) &&
          DateTime.now().isBefore(cleanupDeadline)) {
        await Future<void>.delayed(const Duration(milliseconds: 20));
      }
      expect(await _processExists(pid), isFalse);
    });

    test('isolate loss during close retains forced cleanup', () async {
      final ready = ReceivePort();
      final isolate = await Isolate.spawn(
        _closeResistantSession,
        ready.sendPort,
      );
      addTearDown(() => isolate.kill(priority: Isolate.immediate));
      final pid = await ready.first.timeout(const Duration(seconds: 10)) as int;
      ready.close();

      isolate.kill(priority: Isolate.immediate);

      final deadline = DateTime.now().add(const Duration(seconds: 10));
      while (await _processExists(pid) && DateTime.now().isBefore(deadline)) {
        await Future<void>.delayed(const Duration(milliseconds: 20));
      }
      expect(await _processExists(pid), isFalse);
    });
  });
}
