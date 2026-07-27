@TestOn('!windows')
library;

import 'dart:async';
import 'dart:typed_data';

import 'package:ptyx/ptyx.dart';
import 'package:test/test.dart';

void main() {
  const size = PtySize(rows: 24, columns: 80);

  PtySpawnOptions shell(String script, {int inputCapacity = 4096}) {
    return PtySpawnOptions(
      executable: '/bin/sh',
      arguments: ['-c', script],
      initialSize: size,
      maxBufferedInput: inputCapacity,
      maxBufferedOutput: 64 * 1024,
    );
  }

  test('spawn is asynchronous and publishes a fully routed session', () async {
    final Future<PtySession> pending = PtySession.spawn(shell('printf ready'));
    final session = await pending;
    addTearDown(session.close);

    final output = await session.output.expand((chunk) => chunk).toList();

    expect(String.fromCharCodes(output), 'ready');
  });

  test(
    'input exposes all-or-reject, capacity, flush, and terminal state',
    () async {
      final session = await PtySession.spawn(
        shell('sleep 0.1; cat >/dev/null'),
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
    final session = await PtySession.spawn(shell('sleep 10'));
    addTearDown(session.close);

    await expectLater(
      session.waitForInputCapacity(4097),
      throwsA(isA<PtyInputException>()),
    );
  });

  test('capabilities describe platform-specific behavior', () async {
    final session = await PtySession.spawn(shell('exit 0'));
    addTearDown(session.close);

    expect(session.capabilities.processGroups, isTrue);
    expect(session.capabilities.signals, isTrue);
    expect(session.capabilities.terminalModes, isTrue);
    expect(session.capabilities.conPty, isFalse);
  });
}
