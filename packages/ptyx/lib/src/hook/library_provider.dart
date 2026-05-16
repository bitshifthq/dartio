import 'dart:io';

import 'package:code_assets/code_assets.dart';
import 'package:crypto/crypto.dart';
import 'package:hooks/hooks.dart';
import 'package:meta/meta.dart';

import 'android_ndk.dart';
import 'asset_hashes.dart';
import 'cargo_target.dart';
import 'dart_sdk.dart';

@visibleForTesting
@internal
Map<String, String> androidToolchainEnvironment(
  BuildInput input, {
  Map<String, String>? environment,
}) {
  if (input.config.code.targetOS != .android) return const {};

  final cargoTarget = input.cargoTargetTriple();
  if (cargoTarget == null) return const {};

  return androidCargoToolchainEnvironment(
    cargoTarget: cargoTarget,
    architecture: input.config.code.targetArchitecture,
    environment: environment,
  );
}

@visibleForTesting
@internal
Directory cargoBuildDirectory(BuildInput input) {
  final target = input.artifactTargetTriple();
  return Directory.fromUri(
    _asDirectoryUri(input.outputDirectoryShared).resolve('cargo/$target/'),
  );
}

@internal
String libraryExtension(OS os) => switch (os) {
  OS.macOS => 'dylib',
  OS.windows => 'dll',
  _ => 'so',
};

Uri _asDirectoryUri(Uri uri) {
  if (uri.path.endsWith('/')) return uri;
  return uri.replace(path: '${uri.path}/');
}

String _prebuiltFileName(BuildInput input) {
  final target = input.artifactTargetTriple();
  final extension = libraryExtension(input.config.code.targetOS);
  return 'libptyx-$target.$extension';
}

@internal
final class AutoProvider extends LibraryProvider {
  final BuildInput input;

  const AutoProvider(this.input);

  @override
  Future<void> provide(File target) async {
    final fileName = _prebuiltFileName(input);
    if (assetHashes.containsKey(fileName)) {
      await DownloadPrebuilt(input).provide(target);
      return;
    }

    await CompileFromSource(input).provide(target);
  }
}

@internal
final class CompileFromSource extends LibraryProvider {
  final BuildInput input;

  const CompileFromSource(this.input);

  @override
  Future<void> provide(File target) async {
    if (!LibraryProvider.cargoAvailable()) {
      throw StateError(
        'Cargo is required to build ptyx from source but was not found on '
        'PATH. Install Rust from https://rustup.rs or use source=prebuilt '
        'when release artifacts are available.',
      );
    }

    final dartSdk = resolveDartSdk();
    final packageRoot = Directory.fromUri(input.packageRoot);
    final crateDir = Directory.fromUri(packageRoot.uri.resolve('native/'));
    final targetDir = cargoBuildDirectory(input);
    final cargoTarget = input.cargoTargetTriple();
    final targetOS = input.config.code.targetOS;
    final targetArch = input.config.code.targetArchitecture;
    final isHost = targetOS == OS.current && targetArch == Architecture.current;

    final args = <String>[
      'build',
      '--manifest-path',
      File.fromUri(crateDir.uri.resolve('Cargo.toml')).path,
      '--release',
      '--target-dir',
      targetDir.path,
      if (!isHost && cargoTarget != null) ...['--target', cargoTarget],
    ];

    final env = Map<String, String>.of(Platform.environment)
      ..['PTYX_DART_SDK'] = dartSdk.path;
    env.addAll(androidToolchainEnvironment(input, environment: env));

    final result = Process.runSync('cargo', args, environment: env);
    if (result.exitCode != 0) {
      throw Exception(
        'Cargo build failed (exit ${result.exitCode}):\n'
        'stdout: ${result.stdout}\n'
        'stderr: ${result.stderr}',
      );
    }

    final releaseDir = isHost || cargoTarget == null
        ? Directory.fromUri(targetDir.uri.resolve('release/'))
        : Directory.fromUri(targetDir.uri.resolve('$cargoTarget/release/'));
    final built = File.fromUri(
      releaseDir.uri.resolve(targetOS.dylibFileName('ptyx')),
    );
    if (!built.existsSync()) {
      throw StateError(
        'Cargo reported success but ${built.path} was not found.',
      );
    }

    target.parent.createSync(recursive: true);
    built.copySync(target.path);
  }
}

@internal
final class DownloadPrebuilt extends LibraryProvider {
  static const _repoUrl = 'https://github.com/elias8/libghostty';
  static const _defaultBaseUrl = '$_repoUrl/releases/download';

  final BuildInput input;
  final String baseUrl;
  final Map<String, String> hashes;

  const DownloadPrebuilt(
    this.input, {
    this.baseUrl = _defaultBaseUrl,
    Map<String, String>? hashes,
  }) : hashes = hashes ?? assetHashes;

  @override
  Future<void> provide(File target) async {
    final fileName = _prebuiltFileName(input);
    final cacheDir = Directory.fromUri(
      _asDirectoryUri(
        input.outputDirectoryShared,
      ).resolve('prebuilt-$releaseTag/'),
    );
    final cachedFile = File('${cacheDir.path}/$fileName');

    if (cachedFile.existsSync() && !_validateHash(cachedFile, fileName)) {
      cachedFile.deleteSync();
    }

    if (!cachedFile.existsSync()) {
      await _download(fileName, cachedFile);
      if (!_validateHash(cachedFile, fileName)) {
        cachedFile.deleteSync();
        throw Exception(
          'SHA256 hash mismatch for downloaded $fileName. The file may be '
          'corrupted. Try again, or build from source with source=compile.',
        );
      }
    }

    target.parent.createSync(recursive: true);
    cachedFile.copySync(target.path);
  }

  Future<void> _download(String fileName, File destination) async {
    final expectedHash = hashes[fileName];
    if (expectedHash == null) {
      throw Exception(
        'No known hash for $fileName. This target is not included in '
        '$releaseTag. Use source=auto or source=compile to build locally.',
      );
    }

    final url = '$baseUrl/$releaseTag/$fileName';
    destination.parent.createSync(recursive: true);
    final tmp = File('${destination.path}.tmp');

    final httpClient = HttpClient();
    try {
      final request = await httpClient.getUrl(Uri.parse(url));
      final response = await request.close();
      if (response.statusCode != 200) {
        throw Exception(
          'Failed to download prebuilt library from $url '
          '(HTTP ${response.statusCode}). Use source=compile to build '
          'the bundled Rust crate.',
        );
      }
      final sink = tmp.openWrite();
      await response.pipe(sink);
    } finally {
      httpClient.close();
    }

    tmp.renameSync(destination.path);
  }

  bool _validateHash(File file, String hashKey) {
    final expectedHash = hashes[hashKey];
    if (expectedHash == null) {
      throw Exception(
        'No known hash for $hashKey. This target is not included in '
        '$releaseTag.',
      );
    }

    final bytes = file.readAsBytesSync();
    final digest = sha256.convert(bytes).toString();
    return digest == expectedHash;
  }
}

@internal
sealed class LibraryProvider {
  const LibraryProvider();

  Future<void> provide(File target);

  static bool cargoAvailable() {
    try {
      final result = Process.runSync('cargo', ['--version']);
      return result.exitCode == 0;
    } on ProcessException {
      return false;
    }
  }

  static LibraryProvider resolve(BuildInput input) {
    final source = input.userDefines['source'];
    return switch (source) {
      'auto' || null => AutoProvider(input),
      'prebuilt' => DownloadPrebuilt(input),
      'compile' => CompileFromSource(input),
      _ => throw ArgumentError(
        'Invalid source: $source. Valid options are "auto", "prebuilt", '
        'or "compile".',
      ),
    };
  }
}
