import 'dart:convert';
import 'dart:ffi';
import 'dart:io';

const _expectedAbi = 1;
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
  if (abi != _expectedAbi) {
    throw StateError('ABI mismatch: expected $_expectedAbi, found $abi');
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
      return RegExp(
        r'(?:^|[^a-zA-Z0-9_])_?(ptyx_[a-z0-9_]+)\b',
        multiLine: true,
      ).allMatches(result.stdout as String).map((match) => match[1]!).toSet();
    } on ProcessException catch (error) {
      lastFailure = error;
    }
  }
  throw StateError('could not inspect native exports: $lastFailure');
}
