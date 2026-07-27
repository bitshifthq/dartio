/// Generates the package-private Dart/native ABI from `include/ptyx.h`.
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

const _output = 'lib/src/ffi/controller.dart';

void main() {
  Logger.root.onRecord.listen((record) => stderr.writeln(record));
  try {
    _generator().generate(logger: Logger.root);
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
    entryPoints: [Uri.file('include/ptyx.h')],
    include: (header) =>
        header.path.endsWith('/include/ptyx.h') ||
        header.path == 'include/ptyx.h',
    compilerOptions: const ['-Iinclude'],
  ),
  functions: Functions(
    include: (declaration) => declaration.originalName.startsWith('ptyi_'),
    rename: (declaration) =>
        _functionNames[declaration.originalName] ?? declaration.originalName,
  ),
  structs: const Structs(include: _exclude),
  unions: const Unions(include: _exclude),
  enums: const Enums(include: _exclude),
  typedefs: const Typedefs(include: _exclude),
  globals: const Globals(include: _exclude),
  macros: Macros(
    include: (declaration) => declaration.originalName == 'PTYX_ABI_VERSION',
    rename: (_) => 'controllerAbiVersionExpected',
  ),
);

const _functionNames = {
  'ptyi_abi_version': 'controllerAbiVersion',
  'ptyi_capabilities': 'controllerCapabilities',
  'ptyi_init': 'controllerInit',
  'ptyi_spawn': 'controllerSpawn',
  'ptyi_activate': 'controllerActivate',
  'ptyi_write': 'controllerWrite',
  'ptyi_credit_async': 'controllerCredit',
  'ptyi_pause': 'controllerPause',
  'ptyi_wait_capacity': 'controllerWaitCapacity',
  'ptyi_wait_flush': 'controllerWaitFlush',
  'ptyi_exit_status': 'controllerExitStatus',
  'ptyi_pid': 'controllerPid',
  'ptyi_size': 'controllerSize',
  'ptyi_resize': 'controllerResize',
  'ptyi_signal': 'controllerSignal',
  'ptyi_mode': 'controllerMode',
  'ptyi_tty_name': 'controllerTtyName',
  'ptyi_close': 'controllerClose',
  'ptyi_destroy': 'controllerDestroy',
};

bool _exclude(Declaration _) => false;
