import 'dart:async';
import 'dart:convert';
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

      expect(
        () => session.tryWrite(Uint8List(4097)),
        throwsA(isA<PtyInvalidArgumentException>()),
      );
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

    expect(
      () => session.waitForInputCapacity(4097),
      throwsA(isA<PtyInvalidArgumentException>()),
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
    expect(
      () => session.tryWrite(Uint8List(1)),
      throwsA(isA<PtyInputException>()),
    );
  });

  test(
    'close fails pending input operations with their public identity',
    () async {
      const capacity = 1024 * 1024;
      final session = await PtySession.spawn(
        shell(
          r'stty raw -echo; printf ready; kill -STOP $$; sleep 10',
          inputCapacity: capacity,
        ),
      );
      await session.output.first;
      expect(session.tryWrite(Uint8List(capacity)), isTrue);

      Future<void> expectClosed(Future<void> future, String operation) =>
          expectLater(
            future,
            throwsA(
              isA<PtyClosedException>().having(
                (error) => error.operation,
                'operation',
                operation,
              ),
            ),
          );

      final capacityFailure = expectClosed(
        session.waitForInputCapacity(capacity),
        'waitForInputCapacity',
      );
      final flushFailure = expectClosed(session.flush(), 'flush');
      final writeFailure = expectClosed(
        session.write(Uint8List(capacity)),
        'write',
      );

      await session.close();
      await Future.wait([capacityFailure, flushFailure, writeFailure]);
    },
    testOn: 'posix',
  );

  test(
    'spawn validation is typed and stable with assertions enabled',
    () async {
      await expectLater(
        PtySession.spawn(
          PtySpawnOptions(
            executable: Platform.resolvedExecutable,
            initialSize: const PtySize(rows: 0, columns: 80),
            maxBufferedInput: 0,
          ),
        ),
        throwsA(
          isA<PtyInvalidArgumentException>().having(
            (error) => error.operation,
            'operation',
            'spawn',
          ),
        ),
      );
    },
  );

  group('spawn boundary validation', () {
    final missingExecutable = Platform.isWindows
        ? r'C:\definitely\missing\ptyx.exe'
        : '/definitely/missing/ptyx';

    PtySpawnOptions missing({
      List<String> arguments = const [],
      Map<String, String> environment = const {},
      PtyEnvironmentMode environmentMode = PtyEnvironmentMode.overlay,
      PtySize initialSize = size,
    }) => PtySpawnOptions(
      executable: missingExecutable,
      arguments: arguments,
      environment: environment,
      environmentMode: environmentMode,
      initialSize: initialSize,
    );

    test('accepts 256 arguments and rejects 257 before native spawn', () async {
      await expectLater(
        PtySession.spawn(missing(arguments: List.filled(256, 'x'))),
        throwsA(isA<PtySpawnException>()),
      );
      await expectLater(
        PtySession.spawn(missing(arguments: List.filled(257, 'x'))),
        throwsA(isA<PtyInvalidArgumentException>()),
      );
    });

    test(
      'accepts 4096 environment entries and rejects 4097 before native spawn',
      () async {
        Map<String, String> entries(int count) => {
          for (var index = 0; index < count; index++) 'K$index': '',
        };

        await expectLater(
          PtySession.spawn(
            missing(
              environment: entries(4096),
              environmentMode: PtyEnvironmentMode.replace,
            ),
          ),
          throwsA(isA<PtySpawnException>()),
        );
        await expectLater(
          PtySession.spawn(
            missing(
              environment: entries(4097),
              environmentMode: PtyEnvironmentMode.replace,
            ),
          ),
          throwsA(isA<PtyInvalidArgumentException>()),
        );
      },
    );

    test('enforces the exact 64 KiB encoded spawn payload', () async {
      final executableBytes = utf8.encode(missingExecutable).length;
      final workingDirectoryBytes = utf8.encode(Directory.current.path).length;
      const framingBytes = 36 + 4 * 2;
      final atLimit =
          'x' *
          (64 * 1024 - framingBytes - executableBytes - workingDirectoryBytes);

      await expectLater(
        PtySession.spawn(
          missing(
            arguments: [atLimit],
            environmentMode: PtyEnvironmentMode.replace,
          ),
        ),
        throwsA(isA<PtySpawnException>()),
      );
      await expectLater(
        PtySession.spawn(
          missing(
            arguments: ['$atLimit!'],
            environmentMode: PtyEnvironmentMode.replace,
          ),
        ),
        throwsA(isA<PtyInvalidArgumentException>()),
      );
    });

    test('uses platform cell bounds before native spawn', () async {
      final maximum = Platform.isWindows ? 32767 : 65535;
      await expectLater(
        PtySession.spawn(
          missing(
            initialSize: PtySize(rows: maximum, columns: maximum),
          ),
        ),
        throwsA(isA<PtySpawnException>()),
      );
      await expectLater(
        PtySession.spawn(
          missing(initialSize: PtySize(rows: maximum + 1, columns: 80)),
        ),
        throwsA(isA<PtyInvalidArgumentException>()),
      );
    });

    test('ignored environment maps are not validated', () async {
      for (final mode in [
        PtyEnvironmentMode.inherit,
        PtyEnvironmentMode.clear,
      ]) {
        final base = shell('exit 0');
        final session = await PtySession.spawn(
          PtySpawnOptions(
            executable: base.executable,
            arguments: base.arguments,
            environment: const {'invalid=key': '\u0000'},
            environmentMode: mode,
            initialSize: base.initialSize,
            maxBufferedInput: base.maxBufferedInput,
            maxBufferedOutput: base.maxBufferedOutput,
            gracefulCloseTimeout: base.gracefulCloseTimeout,
          ),
        );
        await session.exitCode;
        await session.close();
      }
    });
  });

  test('capabilities describe platform-specific behavior', () async {
    final session = await PtySession.spawn(shell('exit 0'));
    addTearDown(session.close);

    expect(session.capabilities.processGroups, !Platform.isWindows);
    expect(session.capabilities.signals, !Platform.isWindows);
    expect(session.capabilities.terminalModes, !Platform.isWindows);
    expect(session.capabilities.conPty, Platform.isWindows);
  });

  test(
    'null working directory snapshots the parent cwd for every spawn',
    () async {
      Future<String> childDirectory() async {
        final session = await PtySession.spawn(
          PtySpawnOptions(
            executable: Platform.isWindows
                ? r'C:\Windows\System32\cmd.exe'
                : '/bin/pwd',
            arguments: Platform.isWindows ? const ['/d', '/c', 'cd'] : const [],
            initialSize: size,
          ),
        );
        try {
          return utf8
              .decode(await session.output.expand((chunk) => chunk).toList())
              .trim();
        } finally {
          await session.close();
        }
      }

      final original = Directory.current;
      final root = await Directory.systemTemp.createTemp('ptyx-cwd-');
      final first = await Directory('${root.path}/first').create();
      final second = await Directory('${root.path}/second').create();
      try {
        Directory.current = first;
        await childDirectory();
        Directory.current = second;
        final actual = await childDirectory();
        final expected = second.resolveSymbolicLinksSync();
        expect(
          Platform.isWindows ? actual.toLowerCase() : actual,
          Platform.isWindows ? expected.toLowerCase() : expected,
        );
      } finally {
        Directory.current = original;
        await root.delete(recursive: true);
      }
    },
  );
}
