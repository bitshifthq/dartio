import 'dart:convert';
import 'dart:io';

import 'package:ptyx/ptyx.dart';

import '../../../benchmark/vt_payload.dart';

Future<void> main() async {
  for (var iteration = 0; iteration < 5; iteration++) {
    final fast = await PtySession.spawn(
      PtySpawnOptions(
        executable: Platform.resolvedExecutable,
        arguments: [File('benchmark/fixture.dart').absolute.path, 'exit', '0'],
        initialSize: const PtySize(rows: 24, columns: 80),
      ),
    );
    if (await fast.exitCode != 0) {
      throw StateError('fast probe child failed at iteration $iteration');
    }
    await fast.close();
  }

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
              "Start-Sleep -Seconds 1; Write-Output 'ptyx-standalone-alive'",
            ]
          : const ['-c', r"sleep 1; printf 'ptyx-standalone-alive\n'"],
      initialSize: const PtySize(rows: 24, columns: 80),
    ),
  );
  final output =
      (Platform.isWindows ? fixturePayload(session.output) : session.output)
          .expand((chunk) => chunk)
          .toList();
  final exitCode = await session.exitCode;
  final bytes = await output;
  await session.close();
  if (exitCode != 0) {
    throw StateError('probe child exited with $exitCode');
  }
  stdout.write(utf8.decode(bytes));
}
