import 'dart:async';
import 'dart:typed_data';

/// Removes terminal-control sequences from fixture output.
///
/// ConPTY transports UTF-8 text interleaved with VT sequences. Benchmark
/// fixtures use printable ASCII payloads and reserve ESC for the terminal, so
/// filtering controls gives integrity checks an unambiguous byte stream without
/// changing the package's raw PTY output contract.
Stream<Uint8List> fixturePayload(Stream<Uint8List> source) async* {
  var state = _VtState.ground;
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
          } else {
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
    if (output.length != 0) yield output.takeBytes();
  }
}

enum _VtState { ground, escape, csi, string, stringEscape }
