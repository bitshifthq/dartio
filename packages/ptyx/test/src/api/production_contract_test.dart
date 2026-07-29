import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:ptyx/ptyx.dart';
import 'package:test/test.dart';

import '../../../benchmark/vt_payload.dart';

void main() {
  const size = PtySize(rows: 24, columns: 80);

  Stream<Uint8List> fixtureOutput(PtySession session) =>
      Platform.isWindows ? fixturePayload(session.output) : session.output;

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

    final output = await fixtureOutput(
      session,
    ).expand((chunk) => chunk).toList();

    expect(String.fromCharCodes(output), 'ready');
  });

  test('spawn executes a snapshot of caller-owned collections', () async {
    final arguments = [
      File('benchmark/fixture.dart').absolute.path,
      'environment',
      'PTYX_SPAWN_SNAPSHOT',
    ];
    final environment = {'PTYX_SPAWN_SNAPSHOT': 'original'};
    final pending = PtySession.spawn(
      PtySpawnOptions(
        executable: Platform.resolvedExecutable,
        arguments: arguments,
        environment: environment,
        environmentMode: .replace,
        initialSize: size,
      ),
    );
    arguments
      ..[1] = 'exit'
      ..[2] = '99';
    environment['PTYX_SPAWN_SNAPSHOT'] = 'mutated';

    final session = await pending;
    addTearDown(session.close);
    final output = await fixtureOutput(
      session,
    ).expand((chunk) => chunk).toList();

    expect(utf8.decode(output), 'original|');
    expect(await session.exitCode, 0);
  });

  test(
    'fast exits cannot outrun Dart session publication',
    () async {
      for (var iteration = 0; iteration < 100; iteration++) {
        final options = Platform.isWindows
            ? shell('exit 7')
            : const PtySpawnOptions(
                executable: '/usr/bin/true',
                initialSize: size,
              );
        final session = await PtySession.spawn(options);
        expect(await session.exitCode, Platform.isWindows ? 7 : 0);
        await session.close();
      }
    },
    timeout: const Timeout(Duration(minutes: 2)),
  );

  test('write rejects saturation without failing the session', () async {
    const capacity = 1024 * 1024;
    final session = await PtySession.spawn(
      shell(
        r'stty raw -echo; printf ready; kill -STOP $$; sleep 10',
        inputCapacity: capacity,
      ),
    );
    addTearDown(() => session.close().onError<PtyInputException>((_, _) {}));
    await fixtureOutput(session).first;
    session.write(Uint8List(capacity));

    expect(
      () => session.write(Uint8List(capacity)),
      throwsA(isA<PtyBackpressureException>()),
    );
  }, testOn: 'posix');

  test('oversized writes fail without waiting for capacity', () async {
    final session = await PtySession.spawn(
      shell(
        platformScript(posix: 'sleep 10', windows: 'Start-Sleep -Seconds 10'),
      ),
    );
    addTearDown(session.close);

    expect(
      () => session.write(Uint8List(4097)),
      throwsA(isA<PtyInvalidArgumentException>()),
    );
  });

  test('Dart write admission has a bounded leaf-call copy', () async {
    final session = await PtySession.spawn(
      shell(
        platformScript(posix: 'sleep 10', windows: 'Start-Sleep -Seconds 10'),
        inputCapacity: 2 * 1024 * 1024,
      ),
    );
    addTearDown(session.close);

    expect(
      () => session.write(Uint8List(1024 * 1024 + 1)),
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
    addTearDown(() => session.close().onError<PtyInputException>((_, _) {}));
    await fixtureOutput(session).first;
    session.write(Uint8List(capacity));
    expect(session.kill(ProcessSignal.sigkill), isTrue);

    await session.exitCode;
    expect(
      () => session.write(Uint8List(1)),
      throwsA(isA<PtyInputException>()),
    );
  });

  test('write throws its closed identity after close', () async {
    const capacity = 1024 * 1024;
    final session = await PtySession.spawn(
      shell(
        r'stty raw -echo; printf ready; kill -STOP $$; sleep 10',
        inputCapacity: capacity,
      ),
    );
    await fixtureOutput(session).first;
    await session.close();

    expect(
      () => session.write(Uint8List(capacity)),
      throwsA(
        isA<PtyClosedException>().having(
          (error) => error.operation,
          'operation',
          'write',
        ),
      ),
    );
  }, testOn: 'posix');

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
      final nativeUnitBytes = Platform.isWindows ? 2 : 1;
      int nativeBytes(String value) => Platform.isWindows
          ? value.length * nativeUnitBytes
          : utf8.encode(value).length;
      final executableBytes = nativeBytes(missingExecutable);
      final workingDirectoryBytes = nativeBytes(Directory.current.path);
      const framingBytes = 36 + 4 * 2;
      final atLimit =
          'x' *
          ((64 * 1024 -
                  framingBytes -
                  executableBytes -
                  workingDirectoryBytes) ~/
              nativeUnitBytes);

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
      const maximum = 32767;
      await expectLater(
        PtySession.spawn(
          missing(
            initialSize: const PtySize(rows: maximum, columns: maximum),
          ),
        ),
        throwsA(isA<PtySpawnException>()),
      );
      await expectLater(
        PtySession.spawn(
          missing(initialSize: const PtySize(rows: maximum + 1, columns: 80)),
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
              .decode(
                await fixtureOutput(session).expand((chunk) => chunk).toList(),
              )
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
        final canonicalActual = Platform.isWindows
            ? Directory(actual).resolveSymbolicLinksSync()
            : actual;
        expect(
          Platform.isWindows ? canonicalActual.toLowerCase() : canonicalActual,
          Platform.isWindows ? expected.toLowerCase() : expected,
        );
      } finally {
        Directory.current = original;
        await root.delete(recursive: true);
      }
    },
  );
}
