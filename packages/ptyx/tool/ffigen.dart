/// Generates private Dart declarations for the public ptyx C ABI.
///
/// Run from `packages/ptyx` with:
///
/// ```text
/// dart run tool/ffigen.dart
/// ```
library;

import 'dart:io';

import 'package:ffigen/ffigen.dart';
import 'package:logging/logging.dart';

const _output = 'lib/src/ffi/ptyx.g.dart';
const _testOutput = 'test/src/ffi/ptyx_test.g.dart';

void main() {
  Logger.root.onRecord.listen((record) => stderr.writeln(record));
  try {
    _generator().generate(logger: Logger.root);
    _testGenerator().generate(logger: Logger.root);
  } on Object catch (error, stackTrace) {
    stderr.writeln('Failed to generate bindings: $error\n$stackTrace');
    exitCode = 1;
  }
}

FfiGenerator _generator() => FfiGenerator(
  output: Output(
    dartFile: Uri.file(_output),
    sort: true,
    preamble: '// ignore_for_file: type=lint',
    style: const NativeExternalBindings(assetId: 'package:ptyx/ptyx.dart'),
  ),
  headers: Headers(
    entryPoints: [Uri.file('include/ptyx.h'), Uri.file('include/ptyx_dart.h')],
    include: (header) =>
        header.path.endsWith('/include/ptyx.h') ||
        header.path == 'include/ptyx.h' ||
        header.path.endsWith('/include/ptyx_dart.h') ||
        header.path == 'include/ptyx_dart.h',
    compilerOptions: const ['-Iinclude'],
  ),
  functions: Functions(
    include: (declaration) =>
        (declaration.originalName.startsWith('ptyx_') ||
            declaration.originalName.startsWith('ptyd_')) &&
        !declaration.originalName.startsWith('ptyd_test_'),
    isLeaf: (declaration) => declaration.originalName == 'ptyx_session_write',
  ),
  structs: const Structs(include: _includeType),
  unions: const Unions(include: _exclude),
  enums: const Enums(include: _exclude),
  typedefs: const Typedefs(include: _includeType),
  globals: const Globals(include: _exclude),
  macros: Macros(
    include: (declaration) =>
        declaration.originalName.startsWith('PTYX_') ||
        declaration.originalName.startsWith('PTYD_'),
  ),
);

bool _includeType(Declaration declaration) =>
    declaration.originalName.startsWith('ptyx_') ||
    declaration.originalName.startsWith('ptyd_');

FfiGenerator _testGenerator() => FfiGenerator(
  output: Output(
    dartFile: Uri.file(_testOutput),
    sort: true,
    preamble: '// ignore_for_file: type=lint',
    style: const NativeExternalBindings(assetId: 'package:ptyx/ptyx.dart'),
  ),
  headers: Headers(
    entryPoints: [Uri.file('include/ptyx_dart.h')],
    include: (header) =>
        header.path.endsWith('/include/ptyx_dart.h') ||
        header.path == 'include/ptyx_dart.h',
    compilerOptions: const ['-Iinclude', '-DPTYX_TEST_CONTROLS'],
  ),
  functions: Functions(
    include: (declaration) => declaration.originalName.startsWith('ptyd_test_'),
  ),
  structs: const Structs(include: _exclude),
  unions: const Unions(include: _exclude),
  enums: const Enums(include: _exclude),
  typedefs: const Typedefs(include: _exclude),
  globals: const Globals(include: _exclude),
  macros: const Macros(include: _exclude),
);

bool _exclude(Declaration _) => false;
