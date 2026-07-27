import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:math';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';

const _byteCount = 32 * 1024 * 1024;
const _bufferSize = 64 * 1024;

void main(List<String> arguments) {
  if (arguments.length != 2) {
    stderr.writeln(
      'usage: dart run scorecard.dart <candidate-name> <dynamic-library>',
    );
    exitCode = 64;
    return;
  }

  final candidate = _Candidate.open(arguments[1]);
  if (candidate.abi() != 1) {
    throw StateError('unsupported candidate ABI ${candidate.abi()}');
  }
  final result = <String, Object?>{
    'schema': 1,
    'candidate': arguments[0],
    'library': arguments[1],
    'boundary':
        'Dart FFI, copied buffers, candidate registry, direct PTY, child',
    'pid': pid,
    'rss_before_bytes': ProcessInfo.currentRss,
    'interactive': _repeat(5, () => _interactive(candidate, 400)),
    'output': _repeat(5, () => _output(candidate, _byteCount)),
    'input': _repeat(5, () => _input(candidate, _byteCount)),
    'spawn_close': _spawnClose(candidate, 50),
    'idle_100': _idle(candidate, 100),
    'stale_handle_rejected': _staleHandle(candidate),
    'rss_after_bytes': ProcessInfo.currentRss,
  };

  stdout.writeln(const JsonEncoder.withIndent('  ').convert(result));
}

List<T> _repeat<T>(int count, T Function() run) {
  return [for (var i = 0; i < count; i++) run()];
}

Map<String, Object?> _interactive(_Candidate candidate, int repetitions) {
  final session = candidate.spawn('stty raw -echo; printf READY; cat');
  final buffer = calloc<Uint8>();
  final samples = <int>[];
  try {
    _expectReady(candidate, session);
    for (var i = 0; i < repetitions; i++) {
      buffer.value = i & 0xff;
      final stopwatch = Stopwatch()..start();
      candidate.writeAll(session, buffer, 1);
      candidate.readExact(session, buffer, 1);
      stopwatch.stop();
      if (buffer.value != i & 0xff) {
        throw StateError('interactive byte mismatch at $i');
      }
      samples.add(stopwatch.elapsedMicroseconds);
    }
  } finally {
    calloc.free(buffer);
    candidate.close(session);
  }
  return _distribution(samples);
}

Map<String, Object?> _output(_Candidate candidate, int byteCount) {
  final session = candidate.spawn(
    'stty raw -echo; printf READY; '
    'dd bs=1 count=1 of=/dev/null 2>/dev/null; '
    'head -c $byteCount /dev/zero',
  );
  final gate = calloc<Uint8>();
  final buffer = calloc<Uint8>(_bufferSize);
  final stopwatch = Stopwatch();
  var received = 0;
  try {
    _expectReady(candidate, session);
    gate.value = 1;
    candidate.writeAll(session, gate, 1);
    stopwatch.start();
    while (received < byteCount) {
      final count = candidate.readSome(
        session,
        buffer,
        min(_bufferSize, byteCount - received),
      );
      if (count == 0) {
        throw StateError('output EOF at $received of $byteCount bytes');
      }
      final transferred = Uint8List.fromList(buffer.asTypedList(count));
      if (transferred.any((byte) => byte != 0)) {
        throw StateError('output mismatch at $received');
      }
      received += count;
    }
    stopwatch.stop();
    final exitCode = candidate.wait(session);
    return {
      'bytes': received,
      'elapsed_us': stopwatch.elapsedMicroseconds,
      'mib_per_second': _throughput(received, stopwatch),
      'exit_code': exitCode,
    };
  } finally {
    calloc.free(gate);
    calloc.free(buffer);
    candidate.close(session);
  }
}

Map<String, Object?> _input(_Candidate candidate, int byteCount) {
  final session = candidate.spawn(
    'stty raw -echo; printf READY; head -c $byteCount | wc -c',
  );
  final source = Uint8List(_bufferSize)..fillRange(0, _bufferSize, 120);
  final buffer = calloc<Uint8>(_bufferSize);
  final readBuffer = calloc<Uint8>(_bufferSize);
  final stopwatch = Stopwatch();
  try {
    _expectReady(candidate, session);
    var sent = 0;
    stopwatch.start();
    while (sent < byteCount) {
      final count = min(source.length, byteCount - sent);
      buffer.asTypedList(count).setAll(0, source.take(count));
      candidate.writeAll(session, buffer, count);
      sent += count;
    }

    final result = StringBuffer();
    while (true) {
      final count = candidate.readSome(session, readBuffer, _bufferSize);
      if (count == 0) {
        throw StateError('input child reached EOF before reporting count');
      }
      final transferred = Uint8List.fromList(readBuffer.asTypedList(count));
      for (final value in transferred) {
        if (value == 10 || value == 13) {
          if (result.isNotEmpty) break;
        } else {
          result.writeCharCode(value);
        }
      }
      if (result.isNotEmpty && transferred.any((value) => value == 10)) break;
    }
    stopwatch.stop();
    final received = int.parse(result.toString().trim());
    final exitCode = candidate.wait(session);
    if (received != byteCount) {
      throw StateError('input mismatch: $received != $byteCount');
    }
    return {
      'bytes': received,
      'elapsed_us': stopwatch.elapsedMicroseconds,
      'mib_per_second': _throughput(received, stopwatch),
      'exit_code': exitCode,
    };
  } finally {
    calloc.free(buffer);
    calloc.free(readBuffer);
    candidate.close(session);
  }
}

Map<String, Object?> _spawnClose(_Candidate candidate, int repetitions) {
  final samples = <int>[];
  for (var i = 0; i < repetitions; i++) {
    final stopwatch = Stopwatch()..start();
    final session = candidate.spawn('exit 0');
    final exitCode = candidate.wait(session);
    candidate.close(session);
    stopwatch.stop();
    if (exitCode != 0) throw StateError('spawn child exited $exitCode');
    samples.add(stopwatch.elapsedMicroseconds);
  }
  return _distribution(samples);
}

Map<String, Object?> _idle(_Candidate candidate, int count) {
  final beforeRss = ProcessInfo.currentRss;
  final beforeThreads = _threadCount();
  final sessions = <int>[];
  Object? failure;
  final stopwatch = Stopwatch()..start();
  try {
    for (var i = 0; i < count; i++) {
      try {
        sessions.add(candidate.spawn('cat'));
      } on Object catch (error) {
        failure = error;
        break;
      }
    }
    stopwatch.stop();
    return {
      'requested_sessions': count,
      'created_sessions': sessions.length,
      'failure': failure?.toString(),
      'spawn_elapsed_us': stopwatch.elapsedMicroseconds,
      'rss_delta_bytes': ProcessInfo.currentRss - beforeRss,
      'threads_before': beforeThreads,
      'threads_after': _threadCount(),
    };
  } finally {
    for (final session in sessions.reversed) {
      candidate.close(session);
    }
  }
}

bool _staleHandle(_Candidate candidate) {
  final session = candidate.spawn('exit 0');
  candidate.wait(session);
  candidate.close(session);
  return candidate.closeStatus(session) == 3;
}

void _expectReady(_Candidate candidate, int session) {
  final ready = calloc<Uint8>(5);
  try {
    candidate.readExact(session, ready, 5);
    final text = ascii.decode(Uint8List.fromList(ready.asTypedList(5)));
    if (text != 'READY') throw StateError('expected READY, received $text');
  } finally {
    calloc.free(ready);
  }
}

double _throughput(int bytes, Stopwatch stopwatch) {
  return bytes / (1024 * 1024) / (stopwatch.elapsedMicroseconds / 1e6);
}

int _threadCount() {
  final result = Process.runSync('ps', ['-M', '-p', '$pid']);
  if (result.exitCode != 0) throw StateError('ps failed: ${result.stderr}');
  return const LineSplitter()
          .convert('${result.stdout}')
          .where((line) => line.trim().isNotEmpty)
          .length -
      1;
}

Map<String, Object?> _distribution(List<int> values) {
  values.sort();
  int percentile(double p) => values[((values.length - 1) * p).round()];
  final mean = values.reduce((a, b) => a + b) / values.length;
  final squaredError = values
      .map((value) => pow(value - mean, 2))
      .reduce((a, b) => a + b);
  return {
    'samples': values.length,
    'mean_us': mean,
    'stddev_us': sqrt(squaredError / values.length),
    'p50_us': percentile(0.50),
    'p95_us': percentile(0.95),
    'p99_us': percentile(0.99),
    'min_us': values.first,
    'max_us': values.last,
  };
}

final class _Candidate {
  final int Function() abi;
  final int Function(Pointer<Uint8>, int, Pointer<Uint64>) _spawn;
  final int Function(int, Pointer<Uint8>, int) _read;
  final int Function(int, Pointer<Uint8>, int) _write;
  final int Function(int, Pointer<Int32>) _wait;
  final int Function(int) _close;
  final int Function() _lastOsError;

  _Candidate._(
    this.abi,
    this._spawn,
    this._read,
    this._write,
    this._wait,
    this._close,
    this._lastOsError,
  );

  factory _Candidate.open(String path) {
    final library = DynamicLibrary.open(path);
    return _Candidate._(
      library.lookupFunction<Uint32 Function(), int Function()>(
        'ptyx_candidate_abi',
      ),
      library.lookupFunction<
        Int32 Function(Pointer<Uint8>, Size, Pointer<Uint64>),
        int Function(Pointer<Uint8>, int, Pointer<Uint64>)
      >('ptyx_candidate_spawn'),
      library.lookupFunction<
        Int64 Function(Uint64, Pointer<Uint8>, Size),
        int Function(int, Pointer<Uint8>, int)
      >('ptyx_candidate_read'),
      library.lookupFunction<
        Int64 Function(Uint64, Pointer<Uint8>, Size),
        int Function(int, Pointer<Uint8>, int)
      >('ptyx_candidate_write'),
      library.lookupFunction<
        Int32 Function(Uint64, Pointer<Int32>),
        int Function(int, Pointer<Int32>)
      >('ptyx_candidate_wait'),
      library.lookupFunction<Int32 Function(Uint64), int Function(int)>(
        'ptyx_candidate_close',
      ),
      library.lookupFunction<Int32 Function(), int Function()>(
        'ptyx_candidate_last_os_error',
      ),
    );
  }

  int spawn(String script) {
    return using((arena) {
      final bytes = utf8.encode(script);
      final scriptPointer = arena<Uint8>(bytes.length);
      scriptPointer.asTypedList(bytes.length).setAll(0, bytes);
      final out = arena<Uint64>();
      _check(_spawn(scriptPointer, bytes.length, out), 'spawn');
      return out.value;
    });
  }

  int readSome(int handle, Pointer<Uint8> bytes, int capacity) {
    final result = _read(handle, bytes, capacity);
    if (result < 0) _check(-result, 'read');
    return result;
  }

  void readExact(int handle, Pointer<Uint8> bytes, int length) {
    var offset = 0;
    while (offset < length) {
      final count = readSome(handle, bytes + offset, length - offset);
      if (count == 0) throw StateError('unexpected candidate EOF');
      offset += count;
    }
  }

  void writeAll(int handle, Pointer<Uint8> bytes, int length) {
    var offset = 0;
    while (offset < length) {
      final result = _write(handle, bytes + offset, length - offset);
      if (result < 0) _check(-result, 'write');
      if (result == 0) throw StateError('candidate write made no progress');
      offset += result;
    }
  }

  int wait(int handle) {
    return using((arena) {
      final out = arena<Int32>();
      _check(_wait(handle, out), 'wait');
      return out.value;
    });
  }

  void close(int handle) => _check(_close(handle), 'close');

  int closeStatus(int handle) => _close(handle);

  Never _fail(String operation, int status) {
    throw StateError(
      '$operation failed: status $status, os error ${_lastOsError()}',
    );
  }

  void _check(int status, String operation) {
    if (status != 0) _fail(operation, status);
  }
}
