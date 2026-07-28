import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math';
import 'dart:typed_data';

import 'package:ptyx/ptyx.dart';
import 'package:test/test.dart';

import '../../../benchmark/vt_payload.dart';

void main() {
  group('PtySession', () {
    const defaultSize = PtySize(rows: 24, columns: 80);
    const shortTimeout = Duration(seconds: 5);
    const longTimeout = Duration(seconds: 10);

    Future<PtySession> spawn(PtySpawnOptions options) async {
      final session = await PtySession.spawn(options);
      addTearDown(session.close);
      return session;
    }

    String platformScript({required String posix, required String windows}) {
      return Platform.isWindows ? windows : posix;
    }

    Stream<Uint8List> fixtureOutput(PtySession session) =>
        Platform.isWindows ? fixturePayload(session.output) : session.output;

    ({String executable, List<String> arguments}) shell(String script) {
      if (Platform.isWindows) {
        return (
          executable:
              r'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe',
          arguments: ['-NoProfile', '-NonInteractive', '-Command', script],
        );
      }

      return (executable: '/bin/sh', arguments: ['-c', script]);
    }

    final inputEcho = platformScript(
      posix: r'IFS= read -r line; printf "%s" "$line"',
      windows: '[Console]::Write([Console]::In.ReadLine())',
    );

    Future<PtySession> spawnCommand(
      ({String executable, List<String> arguments}) command, {
      PtySize initialSize = defaultSize,
      Map<String, String> environment = const {},
      PtyEnvironmentMode environmentMode = PtyEnvironmentMode.overlay,
    }) {
      return spawn(
        PtySpawnOptions(
          executable: command.executable,
          arguments: command.arguments,
          environment: environment,
          environmentMode: environmentMode,
          initialSize: initialSize,
        ),
      );
    }

    Future<PtySession> spawnScript(
      String script, {
      PtySize initialSize = defaultSize,
    }) {
      return spawnCommand(shell(script), initialSize: initialSize);
    }

    Future<PtySession> spawnFixture(
      String operation, {
      List<String> arguments = const [],
      PtySize initialSize = defaultSize,
      Map<String, String> environment = const {},
      PtyEnvironmentMode environmentMode = .overlay,
    }) {
      return spawnCommand(
        (
          executable: Platform.resolvedExecutable,
          arguments: [
            File('benchmark/fixture.dart').absolute.path,
            operation,
            ...arguments,
          ],
        ),
        initialSize: initialSize,
        environment: environment,
        environmentMode: environmentMode,
      );
    }

    Future<bool> processExists(int pid) async {
      if ((await Process.run('/bin/kill', ['-0', '$pid'])).exitCode != 0) {
        return false;
      }
      final state = await Process.run('/bin/ps', [
        '-o',
        'state=',
        '-p',
        '$pid',
      ]);
      return state.exitCode == 0 &&
          !(state.stdout as String).trimLeft().startsWith('Z');
    }

    ({String executable, List<String> arguments}) finiteOutputCommand(
      int byteCount,
    ) {
      if (!Platform.isWindows) {
        return (
          executable: '/usr/bin/head',
          arguments: ['-c', '$byteCount', '/dev/zero'],
        );
      }

      return shell(
        r'$out = [Console]::OpenStandardOutput(); '
        r'$chunk = [Text.Encoding]::ASCII.GetBytes(("x" * 8192)); '
        r'$remaining = '
        '$byteCount; '
        r'while ($remaining -gt 0) { '
        r'$count = [Math]::Min($chunk.Length, $remaining); '
        r'$out.Write($chunk, 0, $count); '
        r'$remaining -= $count '
        '}',
      );
    }

    ({String executable, List<String> arguments}) infiniteOutputCommand() {
      if (!Platform.isWindows) {
        return (executable: '/usr/bin/yes', arguments: ['x']);
      }

      return shell(r'while ($true) { [Console]::WriteLine("x") }');
    }

    StreamIterator<String> outputLines(PtySession session) {
      final lines = StreamIterator(
        fixtureOutput(session)
            .map<List<int>>((chunk) => chunk)
            .transform(utf8.decoder)
            .transform(const LineSplitter())
            .map((line) => line.trim()),
      );
      addTearDown(lines.cancel);
      return lines;
    }

    Future<String> nextLine(StreamIterator<String> lines) async {
      final hasLine = await lines.moveNext().timeout(shortTimeout);
      if (!hasLine) throw StateError('PTY output ended before next line');
      return lines.current;
    }

    group('spawn', () {
      test('streams child output', () async {
        final session = await spawnScript(
          platformScript(
            posix: 'printf "hello-ptyx"',
            windows: '[Console]::Write("hello-ptyx")',
          ),
        );

        final bytes = await fixtureOutput(session)
            .expand((chunk) => chunk)
            .take('hello-ptyx'.length)
            .toList()
            .timeout(shortTimeout);

        expect(utf8.decode(bytes), 'hello-ptyx');
      });

      test(
        'resolves a bare executable with a cleared child environment',
        () async {
          final session = await spawn(
            PtySpawnOptions(
              executable: Platform.isWindows ? 'cmd.exe' : 'sh',
              arguments: Platform.isWindows
                  ? const ['/d', '/c', '<nul set /p =bare']
                  : const ['-c', 'printf bare'],
              environmentMode: PtyEnvironmentMode.clear,
              initialSize: defaultSize,
            ),
          );

          final output = await fixtureOutput(
            session,
          ).expand((chunk) => chunk).toList().timeout(shortTimeout);

          expect(utf8.decode(output), 'bare');
        },
      );

      test('throws PtyException for a missing executable', () async {
        const options = PtySpawnOptions(
          executable: 'definitely-not-a-real-ptyx-command',
          initialSize: defaultSize,
        );

        await expectLater(
          PtySession.spawn(options),
          throwsA(
            isA<PtyException>().having(
              (error) => error.toString(),
              'message',
              isNot(contains('Pointer')),
            ),
          ),
        );
      });

      test('applies initial cell size to the child', () async {
        final session = await spawnScript(
          platformScript(
            posix: 'stty size',
            windows:
                r'$size = $Host.UI.RawUI.WindowSize; '
                r'[Console]::WriteLine("$($size.Height) $($size.Width)")',
          ),
          initialSize: const PtySize(rows: 33, columns: 101),
        );
        final lines = outputLines(session);

        final line = await nextLine(lines);

        expect(line, '33 101');
      });

      test('exposes initial pixel size', () async {
        const initialSize = PtySize(
          rows: 31,
          columns: 97,
          pixelWidth: 1234,
          pixelHeight: 567,
        );
        final session = await spawnScript(inputEcho, initialSize: initialSize);

        final size = session.size;

        expect(size, initialSize);
      });
    });

    group('output', () {
      test('closes after native EOF', () async {
        final session = await spawnScript(
          platformScript(
            posix: 'printf done',
            windows: '[Console]::Write("done")',
          ),
        );

        final bytes = await fixtureOutput(
          session,
        ).expand((chunk) => chunk).toList().timeout(longTimeout * 3);

        expect(utf8.decode(bytes), 'done');
      });

      test('reads every byte of large finite output', () async {
        const byteCount = 2 * 1024 * 1024;
        final session = await spawnCommand(finiteOutputCommand(byteCount));

        final received = await fixtureOutput(
          session,
        ).expand((chunk) => chunk).toList().timeout(longTimeout);

        expect(received, hasLength(byteCount));
      }, testOn: 'posix');

      test('observes the final state of large ConPTY output', () async {
        const byteCount = 2 * 1024 * 1024;
        final session = await spawnFixture(
          'output',
          arguments: const ['$byteCount'],
        );
        final output = fixtureOutput(
          session,
        ).map<List<int>>((chunk) => chunk).transform(utf8.decoder).join();

        session.write(Uint8List.fromList(const [1]));
        final text = await output.timeout(longTimeout);

        expect(text, contains('PTYX-OUTPUT-OK $byteCount'));
      }, testOn: 'windows');

      test('continues after a paused subscription resumes', () async {
        final session = await spawnCommand(infiniteOutputCommand());
        final firstChunk = Completer<void>();
        final secondChunk = Completer<void>();
        late final StreamSubscription<Uint8List> subscription;
        subscription = fixtureOutput(session).listen((_) {
          if (!firstChunk.isCompleted) {
            firstChunk.complete();
            subscription.pause();
            return;
          }
          if (!secondChunk.isCompleted) {
            secondChunk.complete();
          }
        });
        addTearDown(subscription.cancel);

        await firstChunk.future.timeout(shortTimeout);
        subscription.resume();
        await expectLater(secondChunk.future.timeout(shortTimeout), completes);
      });

      test('applies backpressure until output is listened to', () async {
        const byteCount = 8 * 1024 * 1024;
        final session = await spawnCommand(finiteOutputCommand(byteCount));

        final exitBeforeListen = await session.exitCode.timeout(
          const Duration(milliseconds: 500),
          onTimeout: () => -1,
        );

        expect(exitBeforeListen, -1);

        final received = await fixtureOutput(
          session,
        ).expand((chunk) => chunk).take(byteCount).length.timeout(longTimeout);
        final exitCode = await session.exitCode.timeout(shortTimeout);

        expect(
          (received: received, exitCode: exitCode),
          (received: byteCount, exitCode: 0),
        );
      }, testOn: 'posix');

      test(
        'resumes ConPTY output through its final terminal state',
        () async {
          const byteCount = 8 * 1024 * 1024;
          final session = await spawnFixture(
            'output',
            arguments: const ['$byteCount'],
          );
          session.write(Uint8List.fromList(const [1]));

          final exitBeforeListen = await session.exitCode.timeout(
            const Duration(milliseconds: 500),
            onTimeout: () => -1,
          );
          final text = await fixtureOutput(session)
              .map<List<int>>((chunk) => chunk)
              .transform(utf8.decoder)
              .join()
              .timeout(longTimeout);

          expect(
            (
              exitBeforeListen: exitBeforeListen,
              finalStateVisible: text.contains('PTYX-OUTPUT-OK $byteCount'),
            ),
            (exitBeforeListen: -1, finalStateVisible: true),
          );
        },
        testOn: 'windows',
      );

      test('discards output after the subscription is canceled', () async {
        const byteCount = 8 * 1024 * 1024;
        final session = await spawnCommand(finiteOutputCommand(byteCount));
        final firstChunk = Completer<void>();
        late final StreamSubscription<Uint8List> subscription;
        subscription = fixtureOutput(session).listen((_) {
          if (!firstChunk.isCompleted) firstChunk.complete();
          unawaited(subscription.cancel());
        });
        addTearDown(subscription.cancel);

        await firstChunk.future.timeout(shortTimeout);
        final exitCode = await session.exitCode.timeout(longTimeout);

        expect(exitCode, 0);
      });

      test('explicitly discards output without attaching a listener', () async {
        final session = await spawnCommand(
          finiteOutputCommand(2 * 1024 * 1024),
        );

        session.discardOutput();

        await session.exitCode.timeout(longTimeout);
        await fixtureOutput(session).drain<void>().timeout(longTimeout);
      });

      test(
        'reports accepted input loss as the terminal output error',
        () async {
          const capacity = 512 * 1024;
          final command = shell(
            '[Console]::Write("ready"); Start-Sleep -Seconds 10',
          );
          final session = await PtySession.spawn(
            PtySpawnOptions(
              executable: command.executable,
              arguments: command.arguments,
              initialSize: defaultSize,
              maxBufferedInput: capacity,
            ),
          );
          addTearDown(
            () => session.close().onError<PtyInputException>((_, _) {}),
          );
          final terminalOutput = fixtureOutput(session).drain<void>();
          session.write(Uint8List(capacity));

          session.kill(ProcessSignal.sigkill);

          await expectLater(terminalOutput, throwsA(isA<PtyInputException>()));
        },
        testOn: 'windows',
      );
    });

    group('environment', () {
      Future<String> environmentText({
        required String probe,
        Map<String, String> environment = const {},
        PtyEnvironmentMode environmentMode = PtyEnvironmentMode.overlay,
      }) async {
        final session = await spawnFixture(
          'environment',
          arguments: [probe],
          environment: environment,
          environmentMode: environmentMode,
        );

        final bytes = await fixtureOutput(
          session,
        ).expand((chunk) => chunk).toList().timeout(longTimeout);
        return utf8.decode(bytes);
      }

      test(
        'applies each environment mode',
        () async {
          final [overlay, inherit, replace, clear] = await Future.wait([
            environmentText(
              probe: 'PTYX_OVERLAY_TEST',
              environment: {'PTYX_OVERLAY_TEST': 'overlay'},
            ),
            environmentText(
              probe: 'PTYX_INHERIT_IGNORED_TEST',
              environment: {'PTYX_INHERIT_IGNORED_TEST': 'ignored'},
              environmentMode: PtyEnvironmentMode.inherit,
            ),
            environmentText(
              probe: 'PTYX_REPLACE_TEST',
              environment: {'PTYX_REPLACE_TEST': 'replace'},
              environmentMode: PtyEnvironmentMode.replace,
            ),
            environmentText(
              probe: 'PTYX_CLEAR_IGNORED_TEST',
              environment: {'PTYX_CLEAR_IGNORED_TEST': 'ignored'},
              environmentMode: PtyEnvironmentMode.clear,
            ),
          ]);

          final modes = (
            overlay: overlay.startsWith('overlay|'),
            inherit: inherit.startsWith('ignored|'),
            replace: (
              hasValue: replace.startsWith('replace|'),
              hasPath: replace.split('|').last.isNotEmpty,
            ),
            clear: (
              hasValue: clear.startsWith('ignored|'),
              hasPath: clear.split('|').last.isNotEmpty,
            ),
          );

          expect(modes, (
            overlay: true,
            inherit: false,
            replace: (hasValue: true, hasPath: false),
            clear: (hasValue: false, hasPath: false),
          ));
        },
        timeout: const Timeout(Duration(minutes: 2)),
      );

      test('throws PtyException for invalid environment entries', () async {
        Future<PtySession> spawnWithEmptyKey() => PtySession.spawn(
          const PtySpawnOptions(
            executable: 'env',
            environment: {'': 'value'},
            initialSize: defaultSize,
          ),
        );

        Future<PtySession> spawnWithNulValue() => PtySession.spawn(
          const PtySpawnOptions(
            executable: 'env',
            environment: {'PTYX_INVALID': 'bad\u0000value'},
            initialSize: defaultSize,
          ),
        );

        await expectLater(spawnWithEmptyKey(), throwsA(isA<PtyException>()));
        await expectLater(spawnWithNulValue(), throwsA(isA<PtyException>()));
      });
    });

    group('write', () {
      test('sends bytes to child input', () async {
        final session = await spawnScript(inputEcho);

        session.write(Uint8List.fromList(utf8.encode('ping\n')));
        final bytes = await fixtureOutput(
          session,
        ).expand((chunk) => chunk).take(4).toList().timeout(shortTimeout);

        expect(utf8.decode(bytes), 'ping');
      });

      test('preserves high-volume input', () async {
        const byteCount = 512 * 1024;
        final session = await spawnFixture(
          'input-verify',
          arguments: const ['$byteCount'],
        );
        final output = StreamIterator(
          fixtureOutput(session).expand((chunk) => chunk),
        );
        addTearDown(output.cancel);
        for (final expected in utf8.encode('READY')) {
          expect(await output.moveNext().timeout(shortTimeout), isTrue);
          expect(output.current, expected);
        }
        final chunk = Uint8List(64 * 1024);
        var sent = 0;
        while (sent < byteCount) {
          final count = min(chunk.length, byteCount - sent);
          for (var index = 0; index < count; index++) {
            chunk[index] = 32 + (((sent + index) * 31 + 17) % 95);
          }
          session.write(
            count == chunk.length
                ? chunk
                : Uint8List.sublistView(chunk, 0, count),
          );
          sent += count;
        }
        final report = <int>[];
        while (await output.moveNext().timeout(shortTimeout)) {
          if (output.current == 10) break;
          if (output.current != 13) report.add(output.current);
        }
        expect(utf8.decode(report), 'OK $byteCount');
        expect(await session.exitCode, 0);
      }, timeout: const Timeout(Duration(minutes: 2)));

      test('throws PtyClosedException after close', () async {
        final session = await spawnScript(inputEcho);
        await session.close();

        expect(
          () => session.write(Uint8List.fromList(const [1])),
          throwsA(isA<PtyClosedException>()),
        );
      });
    });

    group('exitCode', () {
      test('completes with the child exit code', () async {
        final session = await spawnScript('exit 7');

        final exitCode = await session.exitCode.timeout(shortTimeout);

        expect(exitCode, 7);
        expect(await session.exitStatus, const PtyExited(7));
        await expectLater(session.close(), completes);
      });

      test('preserves the complete unsigned Windows exit code', () async {
        final session = await spawnScript('exit -1');

        expect(await session.exitCode.timeout(shortTimeout), 0xffffffff);
        expect(await session.exitStatus, const PtyExited(0xffffffff));
      }, testOn: 'windows');
    });

    group('modeChanges', () {
      test('emits password-like terminal mode', () async {
        final session = await spawnScript(
          'stty -echo; IFS= read -r _; stty echo',
        );

        final mode = await session.modeChanges
            .where((mode) => mode.passwordLike ?? false)
            .first
            .timeout(shortTimeout);

        expect(mode.echo, isFalse);
      }, testOn: 'posix');

      test('remains silent when terminal modes are unavailable', () async {
        final session = await spawnScript('Start-Sleep -Seconds 10');
        final modes = session.modeChanges.toList();
        final modeExpectation = expectLater(modes, completion(isEmpty));

        await Future<void>.value();
        await session.close();

        await modeExpectation;
      }, testOn: 'windows');
    });

    group('metadata', () {
      test('exposes live process and terminal properties', () async {
        final session = await spawnScript(inputEcho);

        final metadata = (
          hasPid: session.pid != null,
          hasTtyName: session.ttyName?.isNotEmpty ?? false,
          hasMode: session.mode != null,
          size: session.size,
        );

        expect(metadata, (
          hasPid: true,
          hasTtyName: !Platform.isWindows,
          hasMode: !Platform.isWindows,
          size: defaultSize,
        ));
      });
    });

    group('resize', () {
      test('reports updated cell size to the child', () async {
        final session = await spawnFixture(
          'size',
          initialSize: const PtySize(rows: 18, columns: 70),
        );
        final lines = outputLines(session);

        await nextLine(lines);
        session.resize(const PtySize(rows: 42, columns: 120));
        session.write(Uint8List.fromList(const [1]));
        final line = await nextLine(lines);

        expect(line, '42 120');
      }, testOn: 'posix');

      test('reports the updated ConPTY cell size', () async {
        final session = await spawnFixture(
          'idle',
          initialSize: const PtySize(rows: 18, columns: 70),
        );

        session.resize(const PtySize(rows: 42, columns: 120));

        expect(session.size, const PtySize(rows: 42, columns: 120));
      }, testOn: 'windows');
    });

    group('kill', () {
      test('terminates a running child', () async {
        final session = await spawnCommand(infiniteOutputCommand());

        await fixtureOutput(session).first.timeout(shortTimeout);
        final killed = session.kill();
        await session.exitCode.timeout(shortTimeout);

        expect(killed, isTrue);
        expect(
          await session.exitStatus,
          Platform.isWindows ? isA<PtyExited>() : const PtySignaled(15),
        );
      });

      test('preserves the requested Unix signal', () async {
        final session = await spawnScript(
          'trap "exit 42" INT; printf ready; while :; do sleep 1; done',
        );

        await fixtureOutput(session).first.timeout(shortTimeout);
        final killed = session.kill(ProcessSignal.sigint);
        final exitCode = await session.exitCode.timeout(shortTimeout);

        expect(killed, isTrue);
        expect(exitCode, 42);
        expect(await session.exitStatus, const PtyExited(42));
        expect(session.kill(), isFalse);
      }, testOn: 'posix');
    });

    group('close', () {
      test('reports accepted input lost during cleanup', () async {
        const capacity = 512 * 1024;
        final command = shell(
          platformScript(
            posix: r'stty raw -echo; printf ready; kill -STOP $$; sleep 10',
            windows: '[Console]::Write("ready"); Start-Sleep -Seconds 10',
          ),
        );
        final session = await PtySession.spawn(
          PtySpawnOptions(
            executable: command.executable,
            arguments: command.arguments,
            initialSize: defaultSize,
            maxBufferedInput: capacity,
            gracefulCloseTimeout: Duration.zero,
          ),
        );
        await fixtureOutput(session).first.timeout(shortTimeout);
        session.discardOutput();
        session.write(Uint8List(capacity));

        final closing = session.close();

        await expectLater(closing, throwsA(isA<PtyInputException>()));
      });

      test('completes while output is active', () async {
        final session = await spawnCommand(infiniteOutputCommand());

        await fixtureOutput(session).first.timeout(shortTimeout);

        await expectLater(session.close().timeout(shortTimeout), completes);
      });

      test('does not wait for output held by an escaped descendant', () async {
        final session = await spawnScript(
          "setsid /bin/sh -c 'exec sleep 30' & printf '%s\\n' \"\$!\"",
        );
        final lines = outputLines(session);
        final escapedPid = int.parse(await nextLine(lines));
        addTearDown(() => Process.killPid(escapedPid, ProcessSignal.sigkill));
        await session.exitCode.timeout(shortTimeout);

        await expectLater(session.close().timeout(shortTimeout), completes);
      }, testOn: 'linux');

      test('is idempotent', () async {
        final session = await spawnScript(inputEcho);
        await session.close();

        await expectLater(session.close(), completes);
      });

      test('forced close waits for native child termination', () async {
        final session = await PtySession.spawn(
          const PtySpawnOptions(
            executable: '/bin/sh',
            arguments: [
              '-c',
              "trap '' HUP TERM; printf ready; while :; do sleep 1; done",
            ],
            initialSize: PtySize(rows: 24, columns: 80),
            gracefulCloseTimeout: Duration.zero,
          ),
        );
        await fixtureOutput(session).first.timeout(shortTimeout);

        await session.close().timeout(shortTimeout);

        await expectLater(session.exitCode, completes);
      }, testOn: 'posix');

      test(
        'broker processes close bursts beyond one fairness quantum',
        () async {
          final sessions = await Future.wait(
            List.generate(
              24,
              (_) => PtySession.spawn(
                const PtySpawnOptions(
                  executable: '/bin/sh',
                  arguments: ['-c', "trap '' HUP TERM; exec sleep 30"],
                  initialSize: PtySize(rows: 24, columns: 80),
                  gracefulCloseTimeout: Duration.zero,
                ),
              ),
            ),
          );

          await Future.wait(
            sessions.map((session) => session.close()),
          ).timeout(const Duration(seconds: 10));
          await Future.wait(
            sessions.map((session) => session.exitCode),
          ).timeout(shortTimeout);
        },
        testOn: 'posix',
      );

      test(
        'reclaims the terminal process group after its leader exits',
        () async {
          final session = await spawnScript(
            r'(trap "" HUP TERM; sleep 30) & child=$!; '
            r'printf "%s\n" "$child"; exit 0',
          );
          final lines = outputLines(session);
          final descendant = int.parse(await nextLine(lines));
          await session.exitCode.timeout(shortTimeout);

          await session.close().timeout(shortTimeout);

          final deadline = DateTime.now().add(shortTimeout);
          while (await processExists(descendant) &&
              DateTime.now().isBefore(deadline)) {
            await Future<void>.delayed(const Duration(milliseconds: 20));
          }
          expect(await processExists(descendant), isFalse);
        },
        testOn: 'posix',
      );
    });
  });
}
