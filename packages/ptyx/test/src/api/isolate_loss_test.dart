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
  ready.send(session.pid);
  await Completer<void>().future;
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
  test('isolate loss abandons and reclaims a quiet native session', () async {
    final ready = ReceivePort();
    final isolate = await Isolate.spawn(_ownQuietSession, ready.sendPort);
    final pid = await ready.first.timeout(const Duration(seconds: 10)) as int;
    ready.close();

    isolate.kill(priority: Isolate.immediate);

    final deadline = DateTime.now().add(const Duration(seconds: 10));
    while (await _processExists(pid) && DateTime.now().isBefore(deadline)) {
      await Future<void>.delayed(const Duration(milliseconds: 20));
    }
    expect(await _processExists(pid), isFalse);
  });
}
