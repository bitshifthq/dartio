import 'dart:convert';
import 'dart:ffi';
import 'dart:io';

const _expectedAbi = 2;
const _symbols = [
  'ptyi_abi_version',
  'ptyi_capabilities',
  'ptyi_init',
  'ptyi_spawn',
  'ptyi_activate',
  'ptyi_write',
  'ptyi_credit_async',
  'ptyi_pause',
  'ptyi_wait_capacity',
  'ptyi_wait_flush',
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
