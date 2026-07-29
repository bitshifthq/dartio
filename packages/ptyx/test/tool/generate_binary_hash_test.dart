import 'dart:io';

import 'package:test/test.dart';

import '../../tool/generate_binary_hash.dart';

void main() {
  group('generateAssetHashesSource', () {
    group('manifest generation', () {
      test('sorts artifacts by filename', () {
        final directory = Directory.systemTemp.createTempSync(
          'ptyx-asset-hashes-',
        );
        addTearDown(() => directory.deleteSync(recursive: true));
        final second = File('${directory.path}/libptyx-x86_64-windows.dll')
          ..writeAsStringSync('second');
        final first = File('${directory.path}/libptyx-aarch64-linux-gnu.so')
          ..writeAsStringSync('first');

        final source = generateAssetHashesSource(
          releaseTag: 'ptyx-v1.2.3',
          artifacts: [second, first],
        );

        expect(
          source.indexOf("'libptyx-aarch64-linux-gnu.so'"),
          lessThan(source.indexOf("'libptyx-x86_64-windows.dll'")),
        );
      });

      test('hashes artifact contents with SHA256', () {
        final directory = Directory.systemTemp.createTempSync(
          'ptyx-asset-hashes-',
        );
        addTearDown(() => directory.deleteSync(recursive: true));
        final artifact = File('${directory.path}/libptyx-x86_64-linux-gnu.so')
          ..writeAsStringSync('ptyx');

        final source = generateAssetHashesSource(
          releaseTag: 'ptyx-v1.2.3',
          artifacts: [artifact],
        );

        expect(
          source,
          contains(
            "'libptyx-x86_64-linux-gnu.so': "
            "'be3f6b89cb755835d5dcc2b2f9caeef2"
            "cce2b51981e8e450c88debcfd8b55450'",
          ),
        );
      });

      test('generates a source-first manifest without artifacts', () {
        final source = generateAssetHashesSource(
          releaseTag: 'ptyx-v0.0.1',
          artifacts: const [],
        );

        expect(source, contains('const assetHashes = <String, String>{};'));
      });
    });

    group('input validation', () {
      test('rejects a malformed release tag', () {
        expect(
          () =>
              generateAssetHashesSource(releaseTag: 'v1', artifacts: const []),
          throwsA(isA<FormatException>()),
        );
      });

      test('rejects an unsupported artifact filename', () {
        final directory = Directory.systemTemp.createTempSync(
          'ptyx-asset-hashes-',
        );
        addTearDown(() => directory.deleteSync(recursive: true));
        final artifact = File('${directory.path}/libptyx-riscv64-linux-gnu.so')
          ..writeAsStringSync('unsupported native artifact');

        expect(
          () => generateAssetHashesSource(
            releaseTag: 'ptyx-v1.2.3',
            artifacts: [artifact],
          ),
          throwsA(isA<FormatException>()),
        );
      });

      test('rejects duplicate artifact filenames', () {
        final firstDirectory = Directory.systemTemp.createTempSync(
          'ptyx-asset-hashes-a-',
        );
        final secondDirectory = Directory.systemTemp.createTempSync(
          'ptyx-asset-hashes-b-',
        );
        addTearDown(() => firstDirectory.deleteSync(recursive: true));
        addTearDown(() => secondDirectory.deleteSync(recursive: true));
        final first = File('${firstDirectory.path}/libptyx-x86_64-linux-gnu.so')
          ..writeAsStringSync('first');
        final second = File(
          '${secondDirectory.path}/libptyx-x86_64-linux-gnu.so',
        )..writeAsStringSync('second');

        expect(
          () => generateAssetHashesSource(
            releaseTag: 'ptyx-v1.2.3',
            artifacts: [first, second],
          ),
          throwsA(isA<ArgumentError>()),
        );
      });
    });
  });

  group('assetHashesAreCurrent', () {
    group('manifest comparison', () {
      test('accepts matching generated source', () {
        final directory = Directory.systemTemp.createTempSync(
          'ptyx-asset-hashes-',
        );
        addTearDown(() => directory.deleteSync(recursive: true));
        final output = File('${directory.path}/asset_hashes.dart')
          ..writeAsStringSync('generated source');

        final isCurrent = assetHashesAreCurrent(output, 'generated source');

        expect(isCurrent, isTrue);
      });

      test('rejects stale generated source', () {
        final directory = Directory.systemTemp.createTempSync(
          'ptyx-asset-hashes-',
        );
        addTearDown(() => directory.deleteSync(recursive: true));
        final output = File('${directory.path}/asset_hashes.dart')
          ..writeAsStringSync('stale source');

        final isCurrent = assetHashesAreCurrent(output, 'generated source');

        expect(isCurrent, isFalse);
      });

      test('rejects a missing generated file', () {
        final directory = Directory.systemTemp.createTempSync(
          'ptyx-asset-hashes-',
        );
        addTearDown(() => directory.deleteSync(recursive: true));
        final output = File('${directory.path}/asset_hashes.dart');

        final isCurrent = assetHashesAreCurrent(output, 'generated source');

        expect(isCurrent, isFalse);
      });
    });
  });
}
