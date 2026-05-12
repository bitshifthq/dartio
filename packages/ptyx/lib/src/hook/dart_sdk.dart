import 'dart:io';

import 'package:meta/meta.dart';

@internal
Directory resolveDartSdk() {
  final env = Platform.environment['DART_SDK'];
  if (env != null && env.isNotEmpty) {
    final dir = Directory(env);
    if (_hasDartHeaders(dir)) return dir;
  }

  final resolved = File(Platform.resolvedExecutable);
  final sdk = resolved.parent.parent;
  if (_hasDartHeaders(sdk)) return sdk;

  throw StateError(
    'Could not locate Dart SDK headers. Set DART_SDK to the SDK directory '
    'containing include/dart_api_dl.h.',
  );
}

bool _hasDartHeaders(Directory sdk) =>
    File.fromUri(sdk.uri.resolve('include/dart_api_dl.h')).existsSync();
