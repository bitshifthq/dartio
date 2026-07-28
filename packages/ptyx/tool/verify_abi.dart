import 'dart:convert';
import 'dart:ffi';
import 'dart:io';

const _expectedAbi = 6;
const _symbols = [
  'ptyi_abi_version',
  'ptyi_capabilities',
  'ptyi_last_error_code',
  'ptyi_init',
  'ptyi_finalize',
  'ptyi_abandon',
  'ptyi_spawn',
  'ptyi_activate',
  'ptyi_write',
  'ptyi_credit_async',
  'ptyi_pause',
  'ptyi_exit_status',
  'ptyi_pid',
  'ptyi_size',
  'ptyi_resize',
  'ptyi_signal',
  'ptyi_mode',
  'ptyi_tty_name',
  'ptyi_close',
  'ptyi_destroy',
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
    'ptyi_abi_version',
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
  final capabilities = library
      .lookupFunction<Uint32 Function(), int Function()>('ptyi_capabilities')();
  stdout.writeln(
    jsonEncode({
      'abi': abi,
      'capabilities': capabilities,
      'symbols': _symbols.length,
      'library': file.path,
    }),
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
        r'(?:^|[^a-zA-Z0-9_])_?(ptyi_[a-z0-9_]+)\b',
        multiLine: true,
      ).allMatches(result.stdout as String).map((match) => match[1]!).toSet();
    } on ProcessException catch (error) {
      lastFailure = error;
    }
  }
  throw StateError('could not inspect native exports: $lastFailure');
}
