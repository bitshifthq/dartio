import 'dart:io';

import 'package:code_assets/code_assets.dart' show Architecture, OS;
import 'package:hooks/hooks.dart' show BuildInput;
import 'package:ptyx/src/hook/cargo_target.dart'
    show BuildInputCargoTarget, artifactTarget, cargoTarget;
import 'package:test/test.dart';

void main() {
  group('cargoTarget', () {
    BuildInput createBuildInput({
      OS os = .macOS,
      Architecture architecture = .arm64,
    }) {
      final tmp = Directory.systemTemp.createTempSync('ptyx_hook_test_');
      addTearDown(() => tmp.deleteSync(recursive: true));

      return BuildInput(<String, Object?>{
        'package_name': 'ptyx',
        'package_root': '.',
        'out_dir': '${tmp.path}/out',
        'out_dir_shared': '${tmp.path}/shared',
        'user_defines': <String, Object?>{},
        'config': <String, Object?>{
          'build_code_assets': true,
          'build_asset_types': <String>[],
          'extensions': <String, Object?>{
            'code_assets': <String, Object?>{
              'target_os': os.name,
              'target_architecture': architecture.name,
            },
          },
        },
      });
    }

    group('cargoTarget', () {
      test('maps supported Rust target triples', () {
        final targets = [
          cargoTarget(.macOS, .arm64),
          cargoTarget(.macOS, .x64),
          cargoTarget(.linux, .x64),
          cargoTarget(.windows, .x64),
          cargoTarget(.android, .arm64),
        ];

        expect(targets, [
          'aarch64-apple-darwin',
          'x86_64-apple-darwin',
          'x86_64-unknown-linux-gnu',
          'x86_64-pc-windows-msvc',
          'aarch64-linux-android',
        ]);
      });
    });

    group('artifactTarget', () {
      test('maps supported prebuilt artifact targets', () {
        final targets = [
          artifactTarget(.macOS, .arm64),
          artifactTarget(.linux, .x64),
          artifactTarget(.windows, .x64),
          artifactTarget(.android, .arm64),
        ];

        expect(targets, [
          'aarch64-macos',
          'x86_64-linux-gnu',
          'x86_64-windows',
          'aarch64-linux-android',
        ]);
      });
    });

    group('BuildInputCargoTarget', () {
      test('reads the target triple from code asset configuration', () {
        final input = createBuildInput(os: .linux, architecture: .x64);

        final target = input.cargoTargetTriple();

        expect(target, 'x86_64-unknown-linux-gnu');
      });

      test('reads the artifact triple from code asset configuration', () {
        final input = createBuildInput(os: .linux, architecture: .x64);

        final target = input.artifactTargetTriple();

        expect(target, 'x86_64-linux-gnu');
      });
    });
  });
}
