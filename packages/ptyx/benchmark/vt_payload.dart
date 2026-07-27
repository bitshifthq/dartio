import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

/// Maximum logical payload carried by one acknowledged fixture page.
const fixturePagePayloadBytes = 48 * 16;

/// Encodes the acknowledgement for a fully observed ConPTY fixture page.
Uint8List fixturePageAcknowledgement(int sequence) =>
    Uint8List.fromList([0, ...ascii.encode(sequence.toRadixString(36)), 10]);

/// Removes terminal-control sequences from fixture output.
///
/// ConPTY transports UTF-8 text interleaved with VT sequences. Benchmark
/// fixtures use printable ASCII payloads and reserve ESC for the terminal, so
/// filtering controls gives integrity checks an unambiguous byte stream without
/// changing the package's raw PTY output contract.
Stream<Uint8List> fixturePayload(
  Stream<Uint8List> source, {
  bool discardC0 = false,
  FutureOr<void> Function(int sequence)? acknowledgePage,
}) async* {
  var state = _VtState.ground;
  final frames = _FixtureFrames();
  await for (final chunk in source) {
    final output = BytesBuilder(copy: false);
    for (final byte in chunk) {
      switch (state) {
        case _VtState.ground:
          if (byte == 0x1b) {
            state = _VtState.escape;
          } else if (byte == 0x9b) {
            state = _VtState.csi;
          } else if (byte == 0x9d) {
            state = _VtState.string;
          } else if (!discardC0 || byte >= 0x20) {
            output.addByte(byte);
          }
        case _VtState.escape:
          if (byte == 0x5b) {
            state = _VtState.csi;
          } else if (byte == 0x5d ||
              byte == 0x50 ||
              byte == 0x58 ||
              byte == 0x5e ||
              byte == 0x5f) {
            state = _VtState.string;
          } else if (byte < 0x20 || byte > 0x2f) {
            state = _VtState.ground;
          }
        case _VtState.csi:
          if (byte >= 0x40 && byte <= 0x7e) {
            state = _VtState.ground;
          }
        case _VtState.string:
          if (byte == 0x07) {
            state = _VtState.ground;
          } else if (byte == 0x1b) {
            state = _VtState.stringEscape;
          }
        case _VtState.stringEscape:
          state = byte == 0x5c ? _VtState.ground : _VtState.string;
      }
    }
    if (output.length != 0) {
      for (final event in frames.add(output.takeBytes())) {
        if (event.bytes case final bytes?) yield bytes;
        if (event.page case final page?) {
          await acknowledgePage?.call(page);
        }
      }
    }
  }
  for (final event in frames.close()) {
    if (event.bytes case final bytes?) yield bytes;
    if (event.page case final page?) {
      await acknowledgePage?.call(page);
    }
  }
}

enum _VtState { ground, escape, csi, string, stringEscape }

final class _FixtureFrames {
  final _buffer = <int>[];
  var _framed = false;
  var _nextSequence = 0;

  Iterable<({Uint8List? bytes, int? page})> add(Uint8List bytes) sync* {
    _buffer.addAll(bytes);
    if (!_framed) {
      final marker = _indexOf(_buffer, _framePreamble);
      if (marker < 0) {
        final retained = _matchingSuffix(_buffer, _framePreamble);
        final emitted = _buffer.length - retained;
        if (emitted != 0) {
          yield (
            bytes: Uint8List.fromList(_buffer.sublist(0, emitted)),
            page: null,
          );
          _buffer.removeRange(0, emitted);
        }
        return;
      }
      if (marker != 0) {
        yield (
          bytes: Uint8List.fromList(_buffer.sublist(0, marker)),
          page: null,
        );
      }
      _buffer.removeRange(0, marker + _framePreamble.length);
      _framed = true;
    }

    while (_buffer.isNotEmpty) {
      final marker = _buffer.indexOf(_frameBoundary);
      if (marker < 0) {
        _buffer.clear();
        return;
      } else if (marker > 0) {
        _buffer.removeRange(0, marker);
      }

      final end = _buffer.indexOf(_frameBoundary, 1);
      if (end < 0) return;
      final record = _decodeRecord(_buffer.sublist(1, end));
      _buffer.removeRange(0, end + 1);
      if (record == null) {
        continue;
      }
      if (record.page case final page?) {
        if (page >= _nextSequence) {
          throw StateError(
            'fixture page ended at $page before frame $_nextSequence',
          );
        }
        yield (bytes: null, page: page);
        continue;
      }
      final frame = record.frame!;

      if (frame.sequence < _nextSequence) {
        continue;
      }
      if (frame.sequence != _nextSequence) {
        throw StateError(
          'fixture frame $_nextSequence was skipped before ${frame.sequence}',
        );
      }
      _nextSequence++;
      yield (bytes: frame.bytes, page: null);
    }
  }

  Iterable<({Uint8List? bytes, int? page})> close() sync* {
    if (!_framed && _buffer.isNotEmpty) {
      yield (bytes: Uint8List.fromList(_buffer), page: null);
    }
    _buffer.clear();
  }
}

int _indexOf(List<int> bytes, List<int> pattern) {
  for (var offset = 0; offset <= bytes.length - pattern.length; offset++) {
    var matches = true;
    for (var index = 0; index < pattern.length; index++) {
      if (bytes[offset + index] != pattern[index]) {
        matches = false;
        break;
      }
    }
    if (matches) return offset;
  }
  return -1;
}

int _matchingSuffix(List<int> bytes, List<int> pattern) {
  final maximum = bytes.length < pattern.length - 1
      ? bytes.length
      : pattern.length - 1;
  for (var length = maximum; length > 0; length--) {
    var matches = true;
    for (var index = 0; index < length; index++) {
      if (bytes[bytes.length - length + index] != pattern[index]) {
        matches = false;
        break;
      }
    }
    if (matches) return length;
  }
  return 0;
}

({({int sequence, Uint8List bytes})? frame, int? page})? _decodeRecord(
  List<int> record,
) {
  if (record.length > _pagePrefix.length && _startsWith(record, _pagePrefix)) {
    try {
      return (
        frame: null,
        page: int.parse(
          ascii.decode(record.sublist(_pagePrefix.length)),
          radix: 36,
        ),
      );
    } on FormatException {
      return null;
    }
  }
  final separator = record.indexOf(_frameSeparator);
  if (separator <= 0 || separator == record.length - 1) return null;
  try {
    final sequence = int.parse(
      ascii.decode(record.sublist(0, separator)),
      radix: 36,
    );
    final bytes = base64.decode(ascii.decode(record.sublist(separator + 1)));
    return (frame: (sequence: sequence, bytes: bytes), page: null);
  } on FormatException {
    return null;
  }
}

bool _startsWith(List<int> bytes, List<int> prefix) {
  if (bytes.length < prefix.length) return false;
  for (var index = 0; index < prefix.length; index++) {
    if (bytes[index] != prefix[index]) return false;
  }
  return true;
}

const _frameBoundary = 0x7e;
const _frameSeparator = 0x3a;
const _framePreamble = [0x7e, 0x50, 0x46, 0x7e];
const _pagePrefix = [0x50, 0x41, 0x3a];
