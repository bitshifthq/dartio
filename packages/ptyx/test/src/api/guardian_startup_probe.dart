import 'dart:async';
import 'dart:io';
import 'dart:isolate';

import 'package:ptyx/ptyx.dart';

Future<void> _beginRuntimeCreation(SendPort started) async {
  final spawning = PtySession.spawn(
    PtySpawnOptions(
      executable: Platform.isWindows
          ? r'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe'
          : '/bin/sh',
      arguments: Platform.isWindows
          ? const ['-NoProfile', '-NonInteractive', '-Command', 'exit 0']
          : const ['-c', 'exit 0'],
      initialSize: const PtySize(rows: 24, columns: 80),
    ),
  );
  started.send(null);
  await spawning;
}

Future<void> main() async {
  for (var index = 0; index < 32; index++) {
    final started = ReceivePort();
    final owner = await Isolate.spawn(_beginRuntimeCreation, started.sendPort);
    await started.first;
    started.close();
    owner.kill(priority: Isolate.immediate);
  }
}
