import 'package:code_assets/code_assets.dart';
import 'package:hooks/hooks.dart';
import 'package:meta/meta.dart';

@internal
String artifactTarget(OS targetOS, Architecture targetArch) {
  final archStr = switch (targetArch) {
    .x64 => 'x86_64',
    .arm64 => 'aarch64',
    .arm => 'arm',
    .ia32 => 'i686',
    _ => throw ArgumentError('Unsupported architecture: $targetArch'),
  };

  final osStr = switch (targetOS) {
    .macOS => 'macos',
    .linux => 'linux-gnu',
    .windows => 'windows',
    .android => switch (targetArch) {
      .arm64 || .x64 => 'linux-android',
      .arm => 'linux-androideabi',
      _ => throw ArgumentError('Unsupported Android architecture: $targetArch'),
    },
    _ => throw ArgumentError('Unsupported OS: $targetOS'),
  };

  return '$archStr-$osStr';
}

@internal
String? cargoTarget(OS targetOS, Architecture targetArch) {
  final archStr = switch (targetArch) {
    .x64 => 'x86_64',
    .arm64 => 'aarch64',
    .arm => 'arm',
    .ia32 => 'i686',
    _ => throw ArgumentError('Unsupported architecture: $targetArch'),
  };

  final osStr = switch (targetOS) {
    .macOS => 'apple-darwin',
    .linux => 'unknown-linux-gnu',
    .windows => 'pc-windows-msvc',
    .android => switch (targetArch) {
      .arm64 => 'linux-android',
      .x64 => 'linux-android',
      .arm => 'linux-androideabi',
      _ => throw ArgumentError('Unsupported Android architecture: $targetArch'),
    },
    _ => throw ArgumentError('Unsupported OS: $targetOS'),
  };

  return '$archStr-$osStr';
}

@internal
extension BuildInputCargoTarget on BuildInput {
  String artifactTargetTriple() {
    final os = config.code.targetOS;
    final arch = config.code.targetArchitecture;
    return artifactTarget(os, arch);
  }

  String? cargoTargetTriple() {
    final os = config.code.targetOS;
    final arch = config.code.targetArchitecture;
    return cargoTarget(os, arch);
  }
}
