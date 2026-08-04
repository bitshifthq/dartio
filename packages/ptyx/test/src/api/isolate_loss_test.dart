import 'dart:async';
import 'dart:io';
import 'dart:isolate';

import 'package:ptyx/ptyx.dart';
import 'package:test/test.dart';

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
  final deadline = DateTime.now().add(const Duration(seconds: 10));
  while (await _processExists(pid) && DateTime.now().isBefore(deadline)) {
    await Future<void>.delayed(const Duration(milliseconds: 10));
  }
  reports.send(!await _processExists(pid));
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
