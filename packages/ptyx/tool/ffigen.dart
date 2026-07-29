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
    entryPoints: [
      Uri.file('native/include/ptyx/ptyx.h'),
      Uri.file('native/dart/include/ptyx_dart.h'),
    ],
    include: (header) =>
        header.path.endsWith('/native/include/ptyx/ptyx.h') ||
        header.path == 'native/include/ptyx/ptyx.h' ||
        header.path.endsWith('/native/dart/include/ptyx_dart.h') ||
        header.path == 'native/dart/include/ptyx_dart.h',
    compilerOptions: _compilerOptions(),
  ),
  functions: Functions(
    include: (declaration) =>
        (declaration.originalName.startsWith('ptyx_') ||
            declaration.originalName.startsWith('ptyd_')) &&
        !declaration.originalName.startsWith('ptyd_test_'),
    // The private Dart wrapper rejects buffers above 1 MiB before this call.
    // Native admission is nonblocking and copies at most that proven bound.
    isLeaf: (declaration) => declaration.originalName == 'ptyx_session_write',
  ),
  structs: const Structs(include: _includeType),
  unions: const Unions(include: _exclude),
  enums: Enums(
    include: _includeType,
    style: (_, _) => EnumStyle.intConstants,
    silenceWarning: true,
  ),
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
    entryPoints: [Uri.file('native/dart/include/ptyx_dart.h')],
    include: (header) =>
        header.path.endsWith('/native/dart/include/ptyx_dart.h') ||
        header.path == 'native/dart/include/ptyx_dart.h',
    compilerOptions: [..._compilerOptions(), '-DPTYX_TEST_CONTROLS'],
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

List<String> _compilerOptions() {
  final options = <String>['-Inative/include', '-Inative/dart/include'];
  if (!Platform.isMacOS) {
    return options;
  }
  final configured = Platform.environment['SDKROOT'];
  if (configured != null && configured.isNotEmpty) {
    return [...options, '-isysroot', configured];
  }
  final result = Process.runSync('xcrun', ['--show-sdk-path']);
  if (result.exitCode == 0) {
    final sdk = (result.stdout as String).trim();
    if (sdk.isNotEmpty) {
      return [...options, '-isysroot', sdk];
    }
  }
  return options;
}

bool _exclude(Declaration _) => false;
