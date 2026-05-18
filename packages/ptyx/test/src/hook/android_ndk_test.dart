import 'dart:io' show Directory, File;

import 'package:ptyx/src/hook/android_ndk.dart'
    show androidCargoToolchainEnvironment, androidClangName;
import 'package:test/test.dart';

void main() {
  group('Android NDK', () {
    Directory createTempDirectory(String prefix) {
      final directory = Directory.systemTemp.createTempSync(prefix);
      addTearDown(() => directory.deleteSync(recursive: true));
      return directory;
    }

    Directory createNdkBin(Directory root, String path) {
      final prefix = path.isEmpty ? '' : '$path/';
      return Directory.fromUri(
        root.uri.resolve('${prefix}toolchains/llvm/prebuilt/linux-x86_64/bin/'),
      )..createSync(recursive: true);
    }

    File createTool(Directory bin, String name) {
      return File.fromUri(bin.uri.resolve(name))..createSync();
    }

    group('androidClangName', () {
      test('returns clang wrappers for supported architectures', () {
        final names = [
          androidClangName(.arm64),
          androidClangName(.x64),
          androidClangName(.arm),
        ];

        expect(names, [
          'aarch64-linux-android21-clang',
          'x86_64-linux-android21-clang',
          'armv7a-linux-androideabi21-clang',
        ]);
      });

      test('throws ArgumentError for unsupported architectures', () {
        expect(
          () => androidClangName(.ia32),
          throwsA(
            isA<ArgumentError>().having(
              (error) => error.message,
              'message',
              contains('Unsupported Android architecture'),
            ),
          ),
        );
      });
    });

    group('androidCargoToolchainEnvironment', () {
      test('returns linker, compiler, and archiver variables', () {
        final ndk = createTempDirectory('ptyx_ndk_');
        final bin = createNdkBin(ndk, '');
        final clang = createTool(bin, 'x86_64-linux-android21-clang');
        final ar = createTool(bin, 'llvm-ar');

        final environment = androidCargoToolchainEnvironment(
          cargoTarget: 'x86_64-linux-android',
          architecture: .x64,
          environment: {'ANDROID_NDK_HOME': ndk.path},
        );

        expect(
          (
            linker: environment['CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER'],
            compiler: environment['CC_x86_64_linux_android'],
            archiver: environment['AR_x86_64_linux_android'],
          ),
          (linker: clang.path, compiler: clang.path, archiver: ar.path),
        );
      });

      test('uses the newest SDK NDK that contains LLVM tools', () {
        final sdk = createTempDirectory('ptyx_sdk_');
        Directory.fromUri(
          sdk.uri.resolve('ndk/30.0.0/'),
        ).createSync(recursive: true);
        final bin = createNdkBin(sdk, 'ndk/29.0.0');
        final clang = createTool(bin, 'aarch64-linux-android21-clang');
        createTool(bin, 'llvm-ar');

        final environment = androidCargoToolchainEnvironment(
          cargoTarget: 'aarch64-linux-android',
          architecture: .arm64,
          environment: {'ANDROID_HOME': sdk.path},
        );

        expect(
          environment['CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER'],
          clang.path,
        );
      });

      test('orders side-by-side SDK NDK versions numerically', () {
        final sdk = createTempDirectory('ptyx_sdk_');
        final olderBin = createNdkBin(sdk, 'ndk/9.0.0');
        createTool(olderBin, 'aarch64-linux-android21-clang');
        createTool(olderBin, 'llvm-ar');
        final newerBin = createNdkBin(sdk, 'ndk/10.0.0');
        final clang = createTool(newerBin, 'aarch64-linux-android21-clang');
        createTool(newerBin, 'llvm-ar');

        final environment = androidCargoToolchainEnvironment(
          cargoTarget: 'aarch64-linux-android',
          architecture: .arm64,
          environment: {'ANDROID_HOME': sdk.path},
        );

        expect(
          environment['CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER'],
          clang.path,
        );
      });

      test('skips SDK NDKs missing required LLVM tools', () {
        final sdk = createTempDirectory('ptyx_sdk_');
        final incompleteBin = createNdkBin(sdk, 'ndk/30.0.0');
        createTool(incompleteBin, 'aarch64-linux-android21-clang');
        final bin = createNdkBin(sdk, 'ndk/29.0.0');
        final clang = createTool(bin, 'aarch64-linux-android21-clang');
        createTool(bin, 'llvm-ar');

        final environment = androidCargoToolchainEnvironment(
          cargoTarget: 'aarch64-linux-android',
          architecture: .arm64,
          environment: {'ANDROID_HOME': sdk.path},
        );

        expect(
          environment['CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER'],
          clang.path,
        );
      });

      test('finds Windows command wrapper tools', () {
        final ndk = createTempDirectory('ptyx_ndk_');
        final bin = createNdkBin(ndk, '');
        final clang = createTool(bin, 'aarch64-linux-android21-clang.cmd');
        createTool(bin, 'llvm-ar.cmd');

        final environment = androidCargoToolchainEnvironment(
          cargoTarget: 'aarch64-linux-android',
          architecture: .arm64,
          environment: {'ANDROID_NDK_HOME': ndk.path},
        );

        expect(
          environment['CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER'],
          clang.path,
        );
      });

      test('throws StateError when no NDK exists', () {
        final sdk = createTempDirectory('ptyx_sdk_');

        expect(
          () => androidCargoToolchainEnvironment(
            cargoTarget: 'aarch64-linux-android',
            architecture: .arm64,
            environment: {'ANDROID_HOME': sdk.path},
          ),
          throwsA(
            isA<StateError>().having(
              (error) => error.message,
              'message',
              contains('Android NDK was not found'),
            ),
          ),
        );
      });

      test('throws StateError when a required tool is missing', () {
        final ndk = createTempDirectory('ptyx_ndk_');
        final bin = createNdkBin(ndk, '');
        createTool(bin, 'llvm-ar');

        expect(
          () => androidCargoToolchainEnvironment(
            cargoTarget: 'aarch64-linux-android',
            architecture: .arm64,
            environment: {'ANDROID_NDK_HOME': ndk.path},
          ),
          throwsA(
            isA<StateError>().having(
              (error) => error.message,
              'message',
              contains('Android NDK tool aarch64-linux-android21-clang'),
            ),
          ),
        );
      });
    });
  });
}
