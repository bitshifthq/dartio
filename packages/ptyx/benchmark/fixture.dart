import 'dart:async';
import 'dart:collection';
import 'dart:io';
import 'dart:typed_data';

const _ready = [82, 69, 65, 68, 89];
const _outputFlushBytes = 1024 * 1024;

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
  final input = _FixtureInput();
  try {
    switch (arguments.first) {
      case 'exit':
        exit(int.parse(arguments[1]));
      case 'output':
        await _writeReady();
        await _waitForGate(input);
        final byteCount = int.parse(arguments[1]);
        await _writePattern(byteCount);
        await _writeTerminalReport('PTYX-OUTPUT-OK $byteCount');
      case 'output-constant':
        await _writeReady();
        await _waitForGate(input);
        final byteCount = int.parse(arguments[1]);
        await _writeConstant(byteCount, 120);
        await _writeTerminalReport('PTYX-OUTPUT-OK $byteCount');
      case 'output-raw':
        await _writeReady();
        await _waitForGate(input);
        await _writePattern(int.parse(arguments[1]));
      case 'input-verify':
        await _writeReady();
        final result = await _readPattern(
          int.parse(arguments[1]),
          input,
          echo: false,
        );
        stdout.writeln(result);
        await stdout.flush();
      case 'gated-input-verify':
        await _writeReady();
        final gate = File(arguments[2]);
        while (!gate.existsSync()) {
          await Future<void>.delayed(const Duration(milliseconds: 1));
        }
        final result = await _readPattern(
          int.parse(arguments[1]),
          input,
          echo: false,
        );
        stdout.writeln(result);
        await stdout.flush();
      case 'echo-count':
        await _writeReady();
        final result = await _readPattern(
          int.parse(arguments[1]),
          input,
          echo: true,
        );
        if (result != 'OK ${arguments[1]}') {
          stderr.writeln(result);
          exitCode = 65;
        }
      case 'ready-cat':
        await _writeReady();
        while (true) {
          final chunk = await input.readApplication();
          if (chunk == null) break;
          stdout.add(chunk);
          await stdout.flush();
        }
      case 'input-report':
        await _writeReady();
        var received = 0;
        while (true) {
          final chunk = await input.readApplication(maxBytes: 1);
          if (chunk == null) break;
          received++;
          await _writeTerminalReport('PTYX-INPUT $received ${chunk.single}');
        }
      case 'environment':
        stdout.write(
          '${Platform.environment[arguments[1]] ?? ''}|'
          '${Platform.environment['PATH'] ?? ''}',
        );
        await stdout.flush();
      case 'size':
        await _writeSize((stdout.terminalLines, stdout.terminalColumns));
        await input.readApplication(maxBytes: 1);
        await _writeSize((stdout.terminalLines, stdout.terminalColumns));
      case 'idle':
        while (await input.readApplication() != null) {}
      default:
        stderr.writeln('unknown fixture operation: ${arguments.first}');
        exitCode = 64;
    }
  } finally {
    await input.close();
  }
}

Future<void> _writeReady() async {
  stdout.add(_ready);
  await stdout.flush();
}

Future<void> _writeTerminalReport(String report) async {
  if (!Platform.isWindows) return;
  stdout
    ..write('\x1b[2J\x1b[H')
    ..writeln(report);
  await stdout.flush();
}

Future<void> _waitForGate(_FixtureInput input) async {
  if (await input.readApplication(maxBytes: 1) != null) return;
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
    final bytes = count == chunk.length ? chunk : chunk.sublist(0, count);
    stdout.add(bytes);
    offset += count;
    if (offset % _outputFlushBytes == 0 || offset == byteCount) {
      await stdout.flush();
    }
  }
}

Future<void> _writeConstant(int byteCount, int byte) async {
  final chunk = Uint8List(64 * 1024)..fillRange(0, 64 * 1024, byte);
  var offset = 0;
  while (offset != byteCount) {
    final count = byteCount - offset < chunk.length
        ? byteCount - offset
        : chunk.length;
    stdout.add(count == chunk.length ? chunk : chunk.sublist(0, count));
    offset += count;
    if (offset % _outputFlushBytes == 0 || offset == byteCount) {
      await stdout.flush();
    }
  }
}

Future<String> _readPattern(
  int byteCount,
  _FixtureInput input, {
  required bool echo,
}) async {
  var received = 0;
  while (true) {
    final chunk = await input.readApplication();
    if (chunk == null) break;
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

Future<void> _writeSize((int, int) size) async {
  final (rows, columns) = size;
  stdout.writeln('$rows $columns');
  await stdout.flush();
}

final class _FixtureInput {
  final _chunks = StreamIterator<List<int>>(stdin);
  final _application = ListQueue<int>();
  var _done = false;

  Future<void> close() => _chunks.cancel();

  Future<Uint8List?> readApplication({int maxBytes = 64 * 1024}) async {
    while (_application.isEmpty && !_done) {
      await _readChunk();
    }
    if (_application.isEmpty) return null;
    final count = _application.length < maxBytes
        ? _application.length
        : maxBytes;
    return Uint8List.fromList([
      for (var index = 0; index < count; index++) _application.removeFirst(),
    ]);
  }

  Future<void> _readChunk() async {
    if (!await _chunks.moveNext()) {
      _done = true;
      return;
    }
    _application.addAll(_chunks.current);
  }
}
