import 'dart:convert';
import 'dart:io';

import 'package:ptyx/ptyx.dart';

const _size = PtySize(rows: 24, columns: 80);

Future<String> _childDirectory() async {
  final session = await PtySession.spawn(
    PtySpawnOptions(
      executable: Platform.isWindows
          ? r'C:\Windows\System32\cmd.exe'
          : '/bin/pwd',
      arguments: Platform.isWindows ? const ['/d', '/c', 'cd'] : const [],
      initialSize: _size,
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

Future<void> main() async {
  final original = Directory.current;
  final root = await Directory.systemTemp.createTemp('ptyx-cwd-');
  final first = await Directory('${root.path}/first').create();
  final second = await Directory('${root.path}/second').create();
  try {
    Directory.current = first;
    await _childDirectory();
    Directory.current = second;
    final actual = await _childDirectory();
    final expected = second.resolveSymbolicLinksSync();
    if (Platform.isWindows
        ? actual.toLowerCase() != expected.toLowerCase()
        : actual != expected) {
      throw StateError('child cwd mismatch: $actual != $expected');
    }
  } finally {
    Directory.current = original;
    await root.delete(recursive: true);
  }
}
