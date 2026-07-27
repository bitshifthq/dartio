import 'dart:async';
import 'dart:collection';
import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';

const _ready = [82, 69, 65, 68, 89];
const _outputFlushBytes = 1024 * 1024;
const _conptyFrameBytes = 48;
const _conptyPageFrames = 16;
const _conptyEraseLine = [0x1b, 0x5b, 0x32, 0x4b, 13];
var _conptyFrameSequence = 0;

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
        await _writePattern(int.parse(arguments[1]), input);
      case 'output-constant':
        await _writeReady();
        await _waitForGate(input);
        await _writeConstant(int.parse(arguments[1]), 120, input);
      case 'output-raw':
        await _writeReady();
        await _waitForGate(input);
        await _writePattern(int.parse(arguments[1]), input, framed: false);
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
          final chunk = await input.readApplication(
            maxBytes: Platform.isWindows
                ? _conptyFrameBytes * _conptyPageFrames
                : 64 * 1024,
          );
          if (chunk == null) break;
          await _writePayload(chunk, input);
          if (!Platform.isWindows) await stdout.flush();
        }
      case 'environment':
        stdout.write(
          '${Platform.environment[arguments[1]] ?? ''}|'
          '${Platform.environment['PATH'] ?? ''}',
        );
        await stdout.flush();
      case 'size':
        if (Platform.isWindows) _enableWindowInput();
        final initialSize = _terminalSize();
        await _writeSize(initialSize);
        await input.readApplication(maxBytes: 1);
        await _writeSize(
          Platform.isWindows
              ? await _waitForTerminalSizeChange(initialSize)
              : _terminalSize(),
        );
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

Future<void> _waitForGate(_FixtureInput input) async {
  if (await input.readApplication(maxBytes: 1) != null) return;
  throw StateError('output gate reached EOF');
}

int _pattern(int offset) => 32 + ((offset * 31 + 17) % 95);

Future<void> _writePattern(
  int byteCount,
  _FixtureInput input, {
  bool framed = true,
}) async {
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
    if (framed) {
      await _writePayload(bytes, input);
    } else {
      stdout.add(bytes);
    }
    offset += count;
    if (offset % _outputFlushBytes == 0 || offset == byteCount) {
      await stdout.flush();
    }
  }
}

Future<void> _writeConstant(
  int byteCount,
  int byte,
  _FixtureInput input,
) async {
  final chunk = Uint8List(64 * 1024)..fillRange(0, 64 * 1024, byte);
  var offset = 0;
  while (offset != byteCount) {
    final count = byteCount - offset < chunk.length
        ? byteCount - offset
        : chunk.length;
    await _writePayload(
      count == chunk.length ? chunk : chunk.sublist(0, count),
      input,
    );
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
    final chunk = await input.readApplication(
      maxBytes: echo && Platform.isWindows
          ? _conptyFrameBytes * _conptyPageFrames
          : 64 * 1024,
    );
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
      await _writePayload(
        count == chunk.length ? chunk : chunk.sublist(0, count),
        input,
      );
      if (!Platform.isWindows) await stdout.flush();
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

Future<(int, int)> _waitForTerminalSizeChange((int, int) initial) async {
  final deadline = DateTime.now().add(const Duration(seconds: 5));
  var current = _terminalSize();
  while (current == initial && DateTime.now().isBefore(deadline)) {
    await Future<void>.delayed(const Duration(milliseconds: 1));
    current = _terminalSize();
  }
  return current;
}

void _enableWindowInput() {
  final kernel32 = DynamicLibrary.open('kernel32.dll');
  final getStdHandle = kernel32
      .lookupFunction<
        Pointer<Void> Function(Uint32),
        Pointer<Void> Function(int)
      >('GetStdHandle');
  final getConsoleMode = kernel32
      .lookupFunction<
        Int32 Function(Pointer<Void>, Pointer<Uint32>),
        int Function(Pointer<Void>, Pointer<Uint32>)
      >('GetConsoleMode');
  final setConsoleMode = kernel32
      .lookupFunction<
        Int32 Function(Pointer<Void>, Uint32),
        int Function(Pointer<Void>, int)
      >('SetConsoleMode');
  final mode = calloc<Uint32>();
  try {
    final handle = getStdHandle(_stdInputHandle);
    if (getConsoleMode(handle, mode) == 0 ||
        setConsoleMode(handle, mode.value | _enableWindowInputMode) == 0) {
      throw StateError('enabling Windows terminal resize events failed');
    }
  } finally {
    calloc.free(mode);
  }
}

(int, int) _terminalSize() {
  if (!Platform.isWindows) {
    return (stdout.terminalLines, stdout.terminalColumns);
  }
  // Dart 3.11 caches Stdout's Windows terminal dimensions. Querying the
  // screen buffer directly makes this resize fixture observe ConPTY's current
  // internal buffer instead of Dart's cached initial viewport.
  final kernel32 = DynamicLibrary.open('kernel32.dll');
  final getStdHandle = kernel32
      .lookupFunction<
        Pointer<Void> Function(Uint32),
        Pointer<Void> Function(int)
      >('GetStdHandle');
  final getConsoleScreenBufferInfo = kernel32
      .lookupFunction<
        Int32 Function(Pointer<Void>, Pointer<_ConsoleScreenBufferInfo>),
        int Function(Pointer<Void>, Pointer<_ConsoleScreenBufferInfo>)
      >('GetConsoleScreenBufferInfo');
  final information = calloc<_ConsoleScreenBufferInfo>();
  try {
    final handle = getStdHandle(_stdOutputHandle);
    if (getConsoleScreenBufferInfo(handle, information) == 0) {
      throw StateError('GetConsoleScreenBufferInfo failed');
    }
    final size = information.ref.size;
    return (size.y, size.x);
  } finally {
    calloc.free(information);
  }
}

Future<void> _writePayload(List<int> bytes, _FixtureInput input) async {
  if (!Platform.isWindows) {
    stdout.add(bytes);
    return;
  }
  // ConPTY transports screen differences rather than application writes.
  // A page fits in the visible buffer and is acknowledged before it can be
  // scrolled away. Sequence frames reject redraw duplicates while still
  // verifying every original byte. The base64 alphabet excludes '~', so frame
  // boundaries remain unambiguous after VT controls are removed.
  var pageFrames = 0;
  var lastSequence = -1;
  for (var offset = 0; offset < bytes.length; offset += _conptyFrameBytes) {
    final end = offset + _conptyFrameBytes < bytes.length
        ? offset + _conptyFrameBytes
        : bytes.length;
    final payload = base64.encode(bytes.sublist(offset, end));
    lastSequence = _conptyFrameSequence++;
    final sequence = lastSequence.toRadixString(36);
    stdout.add(_conptyEraseLine);
    stdout.write('~PF~~$sequence:$payload~');
    stdout.add(const [13, 10]);
    pageFrames++;
    if (pageFrames == _conptyPageFrames || end == bytes.length) {
      stdout.add(_conptyEraseLine);
      stdout.write('~PA:${lastSequence.toRadixString(36)}~');
      stdout.add(const [13, 10]);
      await stdout.flush();
      await input.waitForAcknowledgement(lastSequence);
      pageFrames = 0;
    }
  }
}

final class _FixtureInput {
  final _chunks = StreamIterator<List<int>>(stdin);
  final _application = ListQueue<int>();
  final _acknowledgements = ListQueue<int>();
  final _acknowledgementBytes = <int>[];
  var _readingAcknowledgement = false;
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

  Future<void> waitForAcknowledgement(int expected) async {
    while (true) {
      while (_acknowledgements.isNotEmpty) {
        final sequence = _acknowledgements.removeFirst();
        if (sequence < expected) continue;
        if (sequence != expected) {
          throw StateError(
            'fixture acknowledgement $expected was skipped before $sequence',
          );
        }
        return;
      }
      if (_done) {
        throw StateError('fixture acknowledgement $expected reached EOF');
      }
      await _readChunk();
    }
  }

  Future<void> _readChunk() async {
    if (!await _chunks.moveNext()) {
      _done = true;
      return;
    }
    for (final byte in _chunks.current) {
      if (!_readingAcknowledgement) {
        if (byte == 0) {
          _readingAcknowledgement = true;
          _acknowledgementBytes.clear();
        } else {
          _application.add(byte);
        }
        continue;
      }
      if (byte == 10) {
        final encoded = ascii.decode(_acknowledgementBytes);
        _acknowledgements.add(int.parse(encoded, radix: 36));
        _readingAcknowledgement = false;
      } else {
        _acknowledgementBytes.add(byte);
      }
    }
  }
}

const _stdInputHandle = 0xfffffff6;
const _stdOutputHandle = 0xfffffff5;
const _enableWindowInputMode = 0x0008;

final class _Coord extends Struct {
  @Int16()
  external int x;

  @Int16()
  external int y;
}

final class _SmallRect extends Struct {
  @Int16()
  external int left;

  @Int16()
  external int top;

  @Int16()
  external int right;

  @Int16()
  external int bottom;
}

final class _ConsoleScreenBufferInfo extends Struct {
  external _Coord size;
  external _Coord cursorPosition;

  @Uint16()
  external int attributes;

  external _SmallRect window;
  external _Coord maximumWindowSize;
}
