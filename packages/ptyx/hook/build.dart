import 'dart:io';

import 'package:code_assets/code_assets.dart';
import 'package:hooks/hooks.dart';
import 'package:ptyx/src/hook/library_provider.dart';

void main(List<String> args) async {
  await build(args, _build);
}

Future<void> _build(BuildInput input, BuildOutputBuilder output) async {
  if (!input.config.buildCodeAssets) return;
  _addBuildDependencies(input, output);

  final targetOS = input.config.code.targetOS;
  final libFileName = targetOS.dylibFileName('ptyx');
  final installDir = input.outputDirectory;
  final libFile = File.fromUri(installDir.resolve('lib/$libFileName'));

  if (!libFile.existsSync()) {
    final provider = LibraryProvider.resolve(input);
    await provider.provide(libFile);
  }

  if (!libFile.existsSync()) {
    throw Exception(
      'Native library not found at ${libFile.path} after build.\n'
      'Options:\n'
      '  - Install Rust/Cargo to compile from source\n'
      '  - Ensure a GitHub Release exists for the current version',
    );
  }

  output.assets.code.add(
    CodeAsset(
      package: input.packageName,
      name: 'ptyx.dart',
      linkMode: DynamicLoadingBundled(),
      file: libFile.uri,
    ),
  );
}

void _addBuildDependencies(BuildInput input, BuildOutputBuilder output) {
  final packageRoot = Directory.fromUri(input.packageRoot);
  final dependencyFiles = [
    packageRoot.uri.resolve('include/ptyx.h'),
    packageRoot.uri.resolve('native/Cargo.toml'),
    packageRoot.uri.resolve('native/Cargo.lock'),
    packageRoot.uri.resolve('native/build.rs'),
  ];

  for (final uri in dependencyFiles) {
    if (File.fromUri(uri).existsSync()) output.dependencies.add(uri);
  }

  final sourceDir = Directory.fromUri(packageRoot.uri.resolve('native/src/'));
  if (!sourceDir.existsSync()) return;
  for (final entity in sourceDir.listSync(recursive: true)) {
    if (entity is File) output.dependencies.add(entity.uri);
  }
}
