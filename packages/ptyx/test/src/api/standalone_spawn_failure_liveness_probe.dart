import 'dart:io';
import 'dart:isolate';

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
    final completionLease = ReceivePort();
    try {
      stdout.writeln('ptyx-standalone-spawn-failure-alive');
      await stdout.flush();
    } finally {
      completionLease.close();
    }
  }
}
