import 'dart:async';
import 'dart:io';
import 'dart:typed_data';

import 'package:ptyx/ptyx.dart';
import 'package:test/test.dart';

void main() {
  const size = PtySize(rows: 24, columns: 80);

  PtySpawnOptions shell(String script, {int inputCapacity = 4096}) {
    final executable = Platform.isWindows
        ? r'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe'
        : '/bin/sh';
    final arguments = Platform.isWindows
        ? ['-NoProfile', '-NonInteractive', '-Command', script]
        : ['-c', script];
    return PtySpawnOptions(
      executable: executable,
      arguments: arguments,
      initialSize: size,
      maxBufferedInput: inputCapacity,
      maxBufferedOutput: 64 * 1024,
    );
  }

  String platformScript({required String posix, required String windows}) {
    return Platform.isWindows ? windows : posix;
  }

  test('spawn is asynchronous and publishes a fully routed session', () async {
    final Future<PtySession> pending = PtySession.spawn(
      shell(
        platformScript(
          posix: 'printf ready',
          windows: '[Console]::Write("ready")',
        ),
      ),
    );
    final session = await pending;
    addTearDown(session.close);

    final output = await session.output.expand((chunk) => chunk).toList();

    expect(String.fromCharCodes(output), 'ready');
  });

  test('fast exits cannot outrun Dart session publication', () async {
    for (var iteration = 0; iteration < 100; iteration++) {
      final session = await PtySession.spawn(shell('exit 7'));
      expect(await session.exitCode, 7);
      await session.close();
    }
  });

  test(
    'input exposes all-or-reject, capacity, flush, and terminal state',
    () async {
      final session = await PtySession.spawn(
        shell(
          platformScript(
            posix: 'sleep 0.1; cat >/dev/null',
            windows:
                'Start-Sleep -Milliseconds 100; '
                r'$null = [Console]::In.ReadToEnd()',
          ),
        ),
      );
      addTearDown(session.close);
      final bytes = Uint8List(4096);

      expect(session.tryWrite(Uint8List(4097)), isFalse);
      expect(session.tryWrite(bytes), isTrue);
      await session.waitForInputCapacity(1);
      expect(session.tryWrite(Uint8List(1)), isTrue);
      await session.flush();

      await session.close();
      await expectLater(session.inputDone, completes);
    },
  );

  test('impossible capacity waits fail without hanging', () async {
    final session = await PtySession.spawn(
      shell(
        platformScript(posix: 'sleep 10', windows: 'Start-Sleep -Seconds 10'),
      ),
    );
    addTearDown(session.close);

    await expectLater(
      session.waitForInputCapacity(4097),
      throwsA(isA<PtyInputException>()),
    );
  });

  test('accepted input reports a typed failure when the child exits', () async {
    const capacity = 1024 * 1024;
    final session = await PtySession.spawn(
      shell(
        Platform.isWindows
            ? '[Console]::Write("ready"); Start-Sleep -Seconds 10'
            : r'stty raw -echo; printf ready; kill -STOP $$; sleep 10',
        inputCapacity: capacity,
      ),
    );
    addTearDown(session.close);
    await session.output.first;
    final inputDone = expectLater(
      session.inputDone,
      throwsA(isA<PtyInputException>()),
    );

    expect(session.tryWrite(Uint8List(capacity)), isTrue);
    expect(session.kill(ProcessSignal.sigkill), isTrue);

    await session.exitCode;
    await inputDone;
  });

  test('capabilities describe platform-specific behavior', () async {
    final session = await PtySession.spawn(shell('exit 0'));
    addTearDown(session.close);

    expect(session.capabilities.processGroups, !Platform.isWindows);
    expect(session.capabilities.signals, !Platform.isWindows);
    expect(session.capabilities.terminalModes, !Platform.isWindows);
    expect(session.capabilities.conPty, Platform.isWindows);
  });
}
