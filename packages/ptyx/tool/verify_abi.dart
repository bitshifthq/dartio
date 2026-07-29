import 'dart:convert';
import 'dart:ffi';
import 'dart:io';

const _symbols = [
  'ptyx_abi_version',
  'ptyx_error_format',
  'ptyx_event_release',
  'ptyx_runtime_capabilities',
  'ptyx_runtime_create',
  'ptyx_runtime_next_event',
  'ptyx_runtime_release',
  'ptyx_runtime_shutdown',
  'ptyx_session_cancel_output',
  'ptyx_session_close',
  'ptyx_session_observe_mode',
  'ptyx_session_release',
  'ptyx_session_resize',
  'ptyx_session_snapshot',
  'ptyx_session_spawn_start',
  'ptyx_session_terminate',
  'ptyx_session_write',
];

void main(List<String> arguments) {
  if (arguments.length != 1) {
    stderr.writeln('usage: dart run tool/verify_abi.dart <native-library>');
    exitCode = 64;
    return;
  }
  final file = File(arguments.single).absolute;
  final library = DynamicLibrary.open(file.path);
  final abi = library.lookupFunction<Uint32 Function(), int Function()>(
    'ptyx_abi_version',
  )();
  final header = File.fromUri(
    Platform.script.resolve('../native/include/ptyx/ptyx.h'),
  );
  final expectedAbi = parseAbiVersion(header.readAsStringSync());
  if (abi != expectedAbi) {
    throw StateError('ABI mismatch: expected $expectedAbi, found $abi');
  }
  for (final symbol in _symbols) {
    library.lookup<NativeFunction<Void Function()>>(symbol);
  }
  final exported = _exportedPtySymbols(file);
  final expected = _symbols.toSet();
  if (exported.difference(expected).isNotEmpty ||
      expected.difference(exported).isNotEmpty) {
    throw StateError(
      'export mismatch: expected ${expected.toList()..sort()}, '
      'found ${exported.toList()..sort()}',
    );
  }
  stdout.writeln(
    jsonEncode({'abi': abi, 'symbols': _symbols.length, 'library': file.path}),
  );
}

/// Reads the packed ABI version from the authoritative public [header].
int parseAbiVersion(String header) {
  int value(String name) {
    final match = RegExp(
      '^#define[ \\t]+$name[ \\t]+UINT32_C\\((\\d+)\\)',
      multiLine: true,
    ).firstMatch(header);
    if (match == null) {
      throw FormatException('Public header does not define $name.');
    }
    final value = int.parse(match[1]!);
    if (value > 0xffff) {
      throw FormatException('$name exceeds its packed 16-bit field.');
    }
    return value;
  }

  final major = value('PTYX_ABI_VERSION_MAJOR');
  final minor = value('PTYX_ABI_VERSION_MINOR');
  return (major << 16) | minor;
}

Set<String> _exportedPtySymbols(File library) {
  final attempts = Platform.isWindows
      ? [
          ('dumpbin', ['/nologo', '/exports', library.path]),
          ('llvm-readobj', ['--coff-exports', library.path]),
        ]
      : Platform.isMacOS
      ? [
          ('nm', ['-gU', library.path]),
        ]
      : [
          ('nm', ['-D', '--defined-only', library.path]),
        ];
  Object? lastFailure;
  for (final (executable, arguments) in attempts) {
    try {
      final result = Process.runSync(executable, arguments);
      if (result.exitCode != 0) {
        lastFailure = '${result.stdout}\n${result.stderr}';
        continue;
      }
      return parseExportedPtySymbols(result.stdout as String);
    } on ProcessException catch (error) {
      lastFailure = error;
    }
  }
  throw StateError('could not inspect native exports: $lastFailure');
}

Set<String> parseExportedPtySymbols(String output) {
  final pattern = RegExp(r'(?:^|[^a-zA-Z0-9_])_?(ptyx_[a-z0-9_]+)\b');
  final symbols = <String>{};
  for (final line in const LineSplitter().convert(output)) {
    for (final match in pattern.allMatches(line)) {
      final suffix = line.substring(match.end).toLowerCase();
      // Inspection tools print the DLL path outside the export table. Exclude
      // that filename occurrence without hiding a real export whose exact
      // name happens to match the library stem.
      if (suffix.startsWith('.dll')) {
        continue;
      }
      symbols.add(match[1]!);
    }
  }
  return symbols;
}
