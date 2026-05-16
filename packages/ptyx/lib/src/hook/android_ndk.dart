import 'dart:io';

import 'package:code_assets/code_assets.dart';
import 'package:meta/meta.dart';

const _androidApiLevel = 21;
const _androidArName = 'llvm-ar';

@internal
Map<String, String> androidCargoToolchainEnvironment({
  required String cargoTarget,
  required Architecture architecture,
  Map<String, String>? environment,
}) {
  final clangName = androidClangName(architecture);
  final (:ar, :clang) = _AndroidNdkLocator(
    environment ?? Platform.environment,
  ).toolchain(clangName: clangName);
  final cargoTargetEnv = cargoTarget.toUpperCase().replaceAll('-', '_');
  final ccTargetEnv = cargoTarget.replaceAll('-', '_');

  return {
    'CARGO_TARGET_${cargoTargetEnv}_LINKER': clang.path,
    'CC_$ccTargetEnv': clang.path,
    'AR_$ccTargetEnv': ar.path,
  };
}

@visibleForTesting
@internal
String androidClangName(Architecture architecture) => switch (architecture) {
  .arm64 => 'aarch64-linux-android$_androidApiLevel-clang',
  .x64 => 'x86_64-linux-android$_androidApiLevel-clang',
  .arm => 'armv7a-linux-androideabi$_androidApiLevel-clang',
  _ => throw ArgumentError('Unsupported Android architecture: $architecture'),
};

int _compareAndroidNdkVersions(Directory a, Directory b) {
  final aParts = _versionParts(a);
  final bParts = _versionParts(b);
  final length = aParts.length > bParts.length ? aParts.length : bParts.length;

  for (var i = 0; i < length; i += 1) {
    final aPart = i < aParts.length ? aParts[i] : 0;
    final bPart = i < bParts.length ? bParts[i] : 0;
    if (aPart != bPart) return aPart.compareTo(bPart);
  }

  return a.path.compareTo(b.path);
}

Uri _directoryUri(Uri uri) {
  if (uri.path.endsWith('/')) return uri;
  return uri.replace(path: '${uri.path}/');
}

List<int> _versionParts(Directory directory) {
  final segments = directory.uri.pathSegments.where((s) => s.isNotEmpty);
  final version = segments.last;
  return [for (final part in version.split('.')) int.tryParse(part) ?? 0];
}

final class _AndroidNdk {
  final Directory directory;

  const _AndroidNdk(this.directory);

  Iterable<Directory> get _hostBins sync* {
    final prebuilt = _prebuiltDirectory;
    if (!prebuilt.existsSync()) return;

    final hosts = prebuilt.listSync().whereType<Directory>().toList()
      ..sort((a, b) => a.path.compareTo(b.path));
    for (final host in hosts) {
      yield Directory.fromUri(_directoryUri(host.uri).resolve('bin/'));
    }
  }

  Directory get _prebuiltDirectory {
    return Directory.fromUri(
      _directoryUri(directory.uri).resolve('toolchains/llvm/prebuilt/'),
    );
  }

  bool containsTools(Iterable<String> names) {
    return _hostBins.any((bin) {
      return names.every((name) {
        return _toolCandidates(bin, name).any((tool) => tool.existsSync());
      });
    });
  }

  File findTool(String name) {
    final prebuilt = _prebuiltDirectory;
    if (!prebuilt.existsSync()) {
      throw StateError(
        'Android NDK LLVM toolchain was not found in ${directory.path}.',
      );
    }

    for (final bin in _hostBins) {
      for (final tool in _toolCandidates(bin, name)) {
        if (tool.existsSync()) return tool;
      }
    }

    throw StateError(
      'Android NDK tool $name was not found in ${prebuilt.path}.',
    );
  }

  Iterable<File> _toolCandidates(Directory bin, String name) {
    return [
      File.fromUri(bin.uri.resolve(name)),
      File.fromUri(bin.uri.resolve('$name.cmd')),
      File.fromUri(bin.uri.resolve('$name.exe')),
    ];
  }
}

final class _AndroidNdkLocator {
  final Map<String, String> environment;

  const _AndroidNdkLocator(this.environment);

  Iterable<Directory> get _explicitNdkDirectories sync* {
    for (final key in ['ANDROID_NDK_HOME', 'ANDROID_NDK_ROOT']) {
      final path = environment[key];
      if (path != null && path.isNotEmpty) yield Directory(path);
    }
  }

  Iterable<Directory> get _sdkDirectories sync* {
    for (final key in ['ANDROID_HOME', 'ANDROID_SDK_ROOT']) {
      final path = environment[key];
      if (path != null && path.isNotEmpty) yield Directory(path);
    }

    final home = environment['HOME'] ?? environment['USERPROFILE'];
    if (home == null || home.isEmpty) return;

    yield Directory('$home/Library/Android/sdk');
    yield Directory('$home/Android/Sdk');
    yield Directory('$home/AppData/Local/Android/Sdk');
  }

  _AndroidNdk locate({Iterable<String> requiredTools = const []}) {
    for (final directory in _explicitNdkDirectories) {
      if (directory.existsSync()) return _AndroidNdk(directory);
    }

    for (final sdk in _sdkDirectories) {
      final ndk = _latestSideBySideNdk(sdk, requiredTools: requiredTools);
      if (ndk != null) return ndk;
    }

    throw StateError(
      'Android NDK was not found. Set ANDROID_NDK_HOME or ANDROID_NDK_ROOT '
      'to the NDK directory.',
    );
  }

  ({File ar, File clang}) toolchain({required String clangName}) {
    final ndk = locate(requiredTools: [clangName, _androidArName]);
    return (ar: ndk.findTool(_androidArName), clang: ndk.findTool(clangName));
  }

  _AndroidNdk? _latestSideBySideNdk(
    Directory sdk, {
    Iterable<String> requiredTools = const [],
  }) {
    final ndkRoot = Directory.fromUri(_directoryUri(sdk.uri).resolve('ndk/'));
    if (!ndkRoot.existsSync()) return null;

    final ndks = [
      for (final directory in ndkRoot.listSync().whereType<Directory>())
        _AndroidNdk(directory),
    ]..sort((a, b) => _compareAndroidNdkVersions(b.directory, a.directory));
    return ndks.where((ndk) => ndk.containsTools(requiredTools)).firstOrNull;
  }
}
