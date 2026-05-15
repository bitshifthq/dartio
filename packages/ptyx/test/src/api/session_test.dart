@TestOn('!windows')
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:ptyx/ptyx.dart';
import 'package:test/test.dart';

void main() {
  group('PtySession', () {
    const defaultSize = PtySize(rows: 24, columns: 80);
    const shortTimeout = Duration(seconds: 5);
    const longTimeout = Duration(seconds: 10);

    PtySession spawn(PtySpawnOptions options) {
      final session = PtySession.spawn(options);
      addTearDown(session.close);
      return session;
    }

    String platformScript({required String posix, required String windows}) {
      return Platform.isWindows ? windows : posix;
    }

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

    PtySession spawnCommand(
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

    PtySession spawnScript(String script, {PtySize initialSize = defaultSize}) {
      return spawnCommand(shell(script), initialSize: initialSize);
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
        r'$chunk = New-Object byte[] 8192; '
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
        session.output
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
        final session = spawnScript(
          platformScript(
            posix: 'printf "hello-ptyx"',
            windows: '[Console]::Write("hello-ptyx")',
          ),
        );

        final bytes = await session.output
            .expand((chunk) => chunk)
            .take('hello-ptyx'.length)
            .toList()
            .timeout(shortTimeout);

        expect(utf8.decode(bytes), 'hello-ptyx');
      });

      test('throws PtyException for a missing executable', () {
        const options = PtySpawnOptions(
          executable: 'definitely-not-a-real-ptyx-command',
          initialSize: defaultSize,
        );

        expect(
          () => PtySession.spawn(options),
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
        final session = spawnScript(
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

      test('exposes initial pixel size', () {
        const initialSize = PtySize(
          rows: 31,
          columns: 97,
          pixelWidth: 1234,
          pixelHeight: 567,
        );
        final session = spawnScript(inputEcho, initialSize: initialSize);

        final size = session.size;

        expect(size, initialSize);
      });
    });

    group('output', () {
      test('closes after native EOF', () async {
        final session = spawnScript(
          platformScript(
            posix: 'printf done',
            windows: '[Console]::Write("done")',
          ),
        );

        final bytes = await session.output
            .expand((chunk) => chunk)
            .toList()
            .timeout(shortTimeout);

        expect(utf8.decode(bytes), 'done');
      });

      test('reads large finite output', () async {
        const byteCount = 2 * 1024 * 1024;
        final session = spawnCommand(finiteOutputCommand(byteCount));

        final received = await session.output
            .expand((chunk) => chunk)
            .take(byteCount)
            .length
            .timeout(longTimeout);

        expect(received, byteCount);
      });

      test('continues after a paused subscription resumes', () async {
        final session = spawnCommand(infiniteOutputCommand());
        final firstChunk = Completer<void>();
        final secondChunk = Completer<void>();
        late final StreamSubscription<Uint8List> subscription;
        subscription = session.output.listen((_) {
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
        final session = spawnCommand(finiteOutputCommand(byteCount));

        final exitBeforeListen = await session.exitCode.timeout(
          const Duration(milliseconds: 500),
          onTimeout: () => -1,
        );

        expect(exitBeforeListen, -1);

        final received = await session.output
            .expand((chunk) => chunk)
            .take(byteCount)
            .length
            .timeout(longTimeout);
        final exitCode = await session.exitCode.timeout(shortTimeout);

        expect(
          (received: received, exitCode: exitCode),
          (received: byteCount, exitCode: 0),
        );
      });

      test('discards output after the subscription is canceled', () async {
        const byteCount = 8 * 1024 * 1024;
        final session = spawnCommand(finiteOutputCommand(byteCount));
        final firstChunk = Completer<void>();
        late final StreamSubscription<Uint8List> subscription;
        subscription = session.output.listen((_) {
          if (!firstChunk.isCompleted) firstChunk.complete();
          unawaited(subscription.cancel());
        });
        addTearDown(subscription.cancel);

        await firstChunk.future.timeout(shortTimeout);
        final exitCode = await session.exitCode.timeout(longTimeout);

        expect(exitCode, 0);
      });
    });

    group('environment', () {
      Future<String> environmentText({
        Map<String, String> environment = const {},
        PtyEnvironmentMode environmentMode = PtyEnvironmentMode.overlay,
      }) async {
        final session = spawnCommand(
          shell(
            platformScript(
              posix: '/usr/bin/env',
              windows:
                  'Get-ChildItem Env: | ForEach-Object { '
                  r'[Console]::WriteLine("$($_.Name)=$($_.Value)") '
                  '}',
            ),
          ),
          environment: environment,
          environmentMode: environmentMode,
        );

        final bytes = await session.output
            .expand((chunk) => chunk)
            .toList()
            .timeout(shortTimeout);
        return utf8.decode(bytes);
      }

      test('applies each environment mode', () async {
        final overlay = await environmentText(
          environment: {'PTYX_OVERLAY_TEST': 'overlay'},
        );
        final inherit = await environmentText(
          environment: {'PTYX_INHERIT_IGNORED_TEST': 'ignored'},
          environmentMode: PtyEnvironmentMode.inherit,
        );
        final replace = await environmentText(
          environment: {'PTYX_REPLACE_TEST': 'replace'},
          environmentMode: PtyEnvironmentMode.replace,
        );
        final clear = await environmentText(
          environment: {'PTYX_CLEAR_IGNORED_TEST': 'ignored'},
          environmentMode: PtyEnvironmentMode.clear,
        );

        final modes = (
          overlay: overlay.contains('PTYX_OVERLAY_TEST=overlay'),
          inherit: inherit.contains('PTYX_INHERIT_IGNORED_TEST=ignored'),
          replace: (
            hasValue: replace.contains('PTYX_REPLACE_TEST=replace'),
            hasPath: replace.contains('PATH='),
          ),
          clear: (
            hasValue: clear.contains('PTYX_CLEAR_IGNORED_TEST=ignored'),
            hasPath: clear.contains('PATH='),
          ),
        );

        expect(modes, (
          overlay: true,
          inherit: false,
          replace: (hasValue: true, hasPath: false),
          clear: (hasValue: false, hasPath: false),
        ));
      });

      test('throws PtyException for invalid environment entries', () {
        PtySession spawnWithEmptyKey() => PtySession.spawn(
          const PtySpawnOptions(
            executable: 'env',
            environment: {'': 'value'},
            initialSize: defaultSize,
          ),
        );

        PtySession spawnWithNulValue() => PtySession.spawn(
          const PtySpawnOptions(
            executable: 'env',
            environment: {'PTYX_INVALID': 'bad\u0000value'},
            initialSize: defaultSize,
          ),
        );

        expect(spawnWithEmptyKey, throwsA(isA<PtyException>()));
        expect(spawnWithNulValue, throwsA(isA<PtyException>()));
      });
    });

    group('write', () {
      test('sends bytes to child input', () async {
        final session = spawnScript(inputEcho);

        session.write(Uint8List.fromList(utf8.encode('ping\n')));
        final bytes = await session.output
            .expand((chunk) => chunk)
            .take(4)
            .toList()
            .timeout(shortTimeout);

        expect(utf8.decode(bytes), 'ping');
      });

      test('echoes high-volume input', () async {
        const byteCount = 512 * 1024;
        final session = spawnScript(
          platformScript(
            posix: 'stty raw -echo; printf READY; cat',
            windows:
                r'$out = [Console]::OpenStandardOutput(); '
                r'$ready = [Text.Encoding]::ASCII.GetBytes("READY"); '
                r'$out.Write($ready, 0, $ready.Length); '
                r'$input = [Console]::OpenStandardInput(); '
                r'$buffer = New-Object byte[] 8192; '
                r'while (($count = $input.Read($buffer, 0, '
                r'$buffer.Length)) -gt 0) { '
                r'$out.Write($buffer, 0, $count) '
                '}',
          ),
        );
        final data = Uint8List(byteCount)..fillRange(0, byteCount, 120);
        final ready = Completer<void>();
        final echoed = Completer<int>();
        final readyBytes = <int>[];
        var received = 0;
        late final StreamSubscription<Uint8List> subscription;
        subscription = session.output.listen((chunk) {
          if (!ready.isCompleted) {
            readyBytes.addAll(chunk);
            if (utf8
                .decode(readyBytes, allowMalformed: true)
                .contains('READY')) {
              ready.complete();
            }
            return;
          }
          received += chunk.length;
          if (received >= data.length && !echoed.isCompleted) {
            echoed.complete(received);
          }
        });
        addTearDown(subscription.cancel);

        await ready.future.timeout(shortTimeout);
        session.write(data);
        final echoedBytes = await echoed.future.timeout(longTimeout);

        expect(echoedBytes, greaterThanOrEqualTo(byteCount));
      });

      test('throws PtyClosedException after close', () async {
        final session = spawnScript(inputEcho);
        await session.close();

        expect(
          () => session.write(Uint8List.fromList(const [1])),
          throwsA(isA<PtyClosedException>()),
        );
      });
    });

    group('exitCode', () {
      test('completes with the child exit code', () async {
        final session = spawnScript('exit 7');

        final exitCode = await session.exitCode.timeout(shortTimeout);

        expect(exitCode, 7);
      });
    });

    group('modeChanges', () {
      test('emits password-like terminal mode', () async {
        final session = spawnScript('stty -echo; IFS= read -r _; stty echo');

        final mode = await session.modeChanges
            .where((mode) => mode.passwordLike ?? false)
            .first
            .timeout(shortTimeout);

        expect(mode.echo, isFalse);
      }, testOn: 'posix');
    });

    group('metadata', () {
      test('exposes live process and terminal properties', () {
        final session = spawnScript(inputEcho);

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
        final session = spawnScript(
          platformScript(
            posix: 'stty -echo; stty size; IFS= read -r _; stty size',
            windows:
                r'$size = $Host.UI.RawUI.WindowSize; '
                r'[Console]::WriteLine("$($size.Height) $($size.Width)"); '
                r'$null = [Console]::In.ReadLine(); '
                r'$size = $Host.UI.RawUI.WindowSize; '
                r'[Console]::WriteLine("$($size.Height) $($size.Width)")',
          ),
          initialSize: const PtySize(rows: 18, columns: 70),
        );
        final lines = outputLines(session);

        await nextLine(lines);
        session.resize(const PtySize(rows: 42, columns: 120));
        session.write(Uint8List.fromList(const [10]));
        final line = await nextLine(lines);

        expect(line, '42 120');
      });
    });

    group('kill', () {
      test('terminates a running child', () async {
        final session = spawnCommand(infiniteOutputCommand());

        await session.output.first.timeout(shortTimeout);
        final killed = session.kill();
        await session.exitCode.timeout(shortTimeout);

        expect(killed, isTrue);
      });
    });

    group('close', () {
      test('completes while output is active', () async {
        final session = spawnCommand(infiniteOutputCommand());

        await session.output.first.timeout(shortTimeout);

        await expectLater(session.close().timeout(shortTimeout), completes);
      });

      test('is idempotent', () async {
        final session = spawnScript(inputEcho);
        await session.close();

        await expectLater(session.close(), completes);
      });
    });
  });
}
