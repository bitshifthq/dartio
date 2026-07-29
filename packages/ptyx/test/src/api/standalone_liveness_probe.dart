import 'dart:async';
import 'dart:io';
import 'dart:isolate';
import 'dart:typed_data';

import 'package:ptyx/ptyx.dart';

Future<void> main() async {
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
  final completionLease = ReceivePort();
  try {
    await output.cancel();
    if (exitCode != 0) {
      throw StateError('probe child exited with $exitCode');
    }
    if (received < 1024 * 1024) {
      throw StateError('probe received only $received output bytes');
    }
    await session.close();
    stdout.writeln('ptyx-standalone-alive');
    await stdout.flush();
  } finally {
    completionLease.close();
  }
}
