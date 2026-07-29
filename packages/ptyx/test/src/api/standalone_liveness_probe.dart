import 'dart:async';
import 'dart:io';
import 'dart:typed_data';

import 'package:ptyx/ptyx.dart';

Future<void> main() async {
  try {
    await PtySession.spawn(
      const PtySpawnOptions(
        executable: '/definitely/missing/ptyx-liveness-probe',
        initialSize: PtySize(rows: 24, columns: 80),
      ),
    );
    throw StateError('missing executable unexpectedly spawned');
  } on PtyException {
    await Future<void>.value();
  }

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
      executable: Platform.resolvedExecutable,
      arguments: [
        File('benchmark/fixture.dart').absolute.path,
        'output',
        '${1024 * 1024}',
      ],
      initialSize: const PtySize(rows: 24, columns: 80),
    ),
  );
  final outputDone = Completer<void>();
  var received = 0;
  final output = session.output.listen(
    (chunk) => received += chunk.length,
    onError: outputDone.completeError,
    onDone: outputDone.complete,
  );
  session.write(Uint8List.fromList(const [1]));
  final exitCode = await session.exitCode;
  await outputDone.future;
  await output.cancel();
  await session.close();
  if (exitCode != 0) {
    throw StateError('probe child exited with $exitCode');
  }
  if (received < 1024 * 1024) {
    throw StateError('probe received only $received output bytes');
  }
  stdout.write('ptyx-standalone-alive');
}
