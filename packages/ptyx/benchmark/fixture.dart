import 'dart:async';
import 'dart:io';
import 'dart:typed_data';

const _ready = [82, 69, 65, 68, 89];
const _outputFlushBytes = 1024 * 1024;
const _conptyLineWidth = 64;

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
      await _writeReady();
      await _waitForGate();
      await _writePattern(int.parse(arguments[1]));
    case 'input-verify':
      await _writeReady();
      final result = await _readPattern(int.parse(arguments[1]), echo: false);
      stdout.writeln(result);
      await stdout.flush();
    case 'gated-input-verify':
      await _writeReady();
      final gate = File(arguments[2]);
      while (!gate.existsSync()) {
        await Future<void>.delayed(const Duration(milliseconds: 1));
      }
      final result = await _readPattern(int.parse(arguments[1]), echo: false);
      stdout.writeln(result);
      await stdout.flush();
    case 'echo-count':
      await _writeReady();
      final result = await _readPattern(int.parse(arguments[1]), echo: true);
      if (result != 'OK ${arguments[1]}') {
        stderr.writeln(result);
        exitCode = 65;
      }
    case 'ready-cat':
      await _writeReady();
      await for (final chunk in stdin) {
        _writePayload(chunk);
        await stdout.flush();
      }
    case 'environment':
      stdout.write(
        '${Platform.environment[arguments[1]] ?? ''}|'
        '${Platform.environment['PATH'] ?? ''}',
      );
      await stdout.flush();
    case 'size':
      await _writeSize();
      await stdin.first;
      await _writeSize();
    case 'idle':
      await stdin.drain<void>();
    default:
      stderr.writeln('unknown fixture operation: ${arguments.first}');
      exitCode = 64;
  }
}

Future<void> _writeReady() async {
  stdout.add(_ready);
  await stdout.flush();
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
    _writePayload(count == chunk.length ? chunk : chunk.sublist(0, count));
    offset += count;
    if (offset % _outputFlushBytes == 0 || offset == byteCount) {
      await stdout.flush();
    }
  }
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
      _writePayload(count == chunk.length ? chunk : chunk.sublist(0, count));
      await stdout.flush();
    }
    received += count;
    if (received == byteCount) break;
  }
  return received == byteCount ? 'OK $received' : 'SHORT $received $byteCount';
}

Future<void> _writeSize() async {
  stdout.writeln('${stdout.terminalLines} ${stdout.terminalColumns}');
  await stdout.flush();
}

void _writePayload(List<int> bytes) {
  if (!Platform.isWindows) {
    stdout.add(bytes);
    return;
  }
  // Keep ConPTY on one row. Scrolling can represent screen updates by
  // repainting cells, which is unsuitable for byte-integrity fixtures.
  for (var offset = 0; offset < bytes.length; offset += _conptyLineWidth) {
    final end = offset + _conptyLineWidth < bytes.length
        ? offset + _conptyLineWidth
        : bytes.length;
    stdout.add(bytes.sublist(offset, end));
    stdout.add(const [13]);
  }
}
