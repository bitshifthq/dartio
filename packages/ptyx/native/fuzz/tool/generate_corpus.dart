import 'dart:io';
import 'dart:typed_data';

const _headerLength = 32;
const _magic = 0x42585450;
const _version = 1;
const _hello = 1;
const _spawn = 2;

void main(List<String> arguments) {
  final check = arguments.contains('--check');
  if (arguments.any((argument) => argument != '--check')) {
    stderr.writeln('usage: dart run tool/generate_corpus.dart [--check]');
    exitCode = 64;
    return;
  }

  final corpus = Directory.fromUri(
    Platform.script.resolve('../corpus/'),
  ).absolute;
  final files = {
    'broker_decoder/short_header': 'PTYX'.codeUnits,
    'broker_decoder/valid_spawn_frame': _frame(_spawn),
    'controller_decoder/short_header': 'PTYX'.codeUnits,
    'controller_decoder/valid_empty_frame': _frame(_hello),
  };
  var matches = true;
  for (final entry in files.entries) {
    final file = File.fromUri(corpus.uri.resolve(entry.key));
    if (check) {
      matches =
          matches &&
          file.existsSync() &&
          _equalBytes(file.readAsBytesSync(), entry.value);
      continue;
    }
    file.parent.createSync(recursive: true);
    file.writeAsBytesSync(entry.value, flush: true);
  }
  if (check && !matches) {
    stderr.writeln(
      'fuzz corpus differs from tool/generate_corpus.dart; regenerate it',
    );
    exitCode = 1;
  }
}

Uint8List _frame(int kind) {
  final frame = ByteData(_headerLength)
    ..setUint32(0, _magic, Endian.host)
    ..setUint16(4, _version, Endian.host)
    ..setUint16(6, kind, Endian.host);
  return frame.buffer.asUint8List();
}

bool _equalBytes(List<int> left, List<int> right) {
  if (left.length != right.length) return false;
  for (var index = 0; index < left.length; index++) {
    if (left[index] != right[index]) return false;
  }
  return true;
}
