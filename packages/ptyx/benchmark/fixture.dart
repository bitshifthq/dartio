import 'dart:async';
import 'dart:io';
import 'dart:typed_data';

const _ready = [82, 69, 65, 68, 89];

Future<void> main(List<String> arguments) async {
  if (arguments.isEmpty) {
    stderr.writeln('missing fixture operation');
    exitCode = 64;
    return;
  }
  if (stdin.hasTerminal) {
    stdin.echoMode = false;
    stdin.lineMode = false;
  }
  switch (arguments.first) {
    case 'exit':
      exit(int.parse(arguments[1]));
    case 'output':
      _writeReady();
      await _writeBytes(int.parse(arguments[1]), 0);
    case 'input-count':
      _writeReady();
      final count = await _readBytes(int.parse(arguments[1]), echo: false);
      stdout.writeln(count);
      await stdout.flush();
    case 'echo-count':
      _writeReady();
      await _readBytes(int.parse(arguments[1]), echo: true);
    case 'ready-cat':
      _writeReady();
      await for (final chunk in stdin) {
        stdout.add(chunk);
        await stdout.flush();
      }
    case 'idle':
      await stdin.drain<void>();
    default:
      stderr.writeln('unknown fixture operation: ${arguments.first}');
      exitCode = 64;
  }
}

void _writeReady() {
  stdout.add(_ready);
}

Future<void> _writeBytes(int byteCount, int value) async {
  final chunk = Uint8List(64 * 1024)..fillRange(0, 64 * 1024, value);
  var remaining = byteCount;
  while (remaining != 0) {
    final count = remaining < chunk.length ? remaining : chunk.length;
    stdout.add(count == chunk.length ? chunk : chunk.sublist(0, count));
    remaining -= count;
  }
  await stdout.flush();
}

Future<int> _readBytes(int byteCount, {required bool echo}) async {
  var received = 0;
  await for (final chunk in stdin) {
    final count = byteCount - received < chunk.length
        ? byteCount - received
        : chunk.length;
    if (echo) {
      stdout.add(count == chunk.length ? chunk : chunk.sublist(0, count));
      await stdout.flush();
    }
    received += count;
    if (received == byteCount) break;
  }
  return received;
}
