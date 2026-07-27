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
      await _waitForGate();
      await _writePattern(int.parse(arguments[1]));
    case 'input-verify':
      _writeReady();
      final result = await _readPattern(int.parse(arguments[1]), echo: false);
      stdout.writeln(result);
      await stdout.flush();
    case 'delayed-input-verify':
      _writeReady();
      await stdout.flush();
      await Future<void>.delayed(
        Duration(milliseconds: int.parse(arguments[2])),
      );
      final result = await _readPattern(int.parse(arguments[1]), echo: false);
      stdout.writeln(result);
      await stdout.flush();
    case 'echo-count':
      _writeReady();
      final result = await _readPattern(int.parse(arguments[1]), echo: true);
      if (result != 'OK ${arguments[1]}') {
        stderr.writeln(result);
        exitCode = 65;
      }
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

Future<void> _waitForGate() async {
  await for (final chunk in stdin) {
    if (chunk.isNotEmpty) return;
  }
  throw StateError('output gate reached EOF');
}

int _pattern(int offset) => 32 + ((offset * 31 + 17) % 95);

Future<void> _writePattern(int byteCount) async {
  final chunk = Uint8List(64 * 1024);
  var offset = 0;
  while (offset != byteCount) {
    final count = byteCount - offset < chunk.length
        ? byteCount - offset
        : chunk.length;
    for (var index = 0; index < count; index++) {
      chunk[index] = _pattern(offset + index);
    }
    stdout.add(count == chunk.length ? chunk : chunk.sublist(0, count));
    offset += count;
  }
  await stdout.flush();
}

Future<String> _readPattern(int byteCount, {required bool echo}) async {
  var received = 0;
  await for (final chunk in stdin) {
    final count = byteCount - received < chunk.length
        ? byteCount - received
        : chunk.length;
    for (var index = 0; index < count; index++) {
      final expected = _pattern(received + index);
      if (chunk[index] != expected) {
        return 'MISMATCH ${received + index} $expected ${chunk[index]}';
      }
    }
    if (echo) {
      stdout.add(count == chunk.length ? chunk : chunk.sublist(0, count));
      await stdout.flush();
    }
    received += count;
    if (received == byteCount) break;
  }
  return received == byteCount ? 'OK $received' : 'SHORT $received $byteCount';
}
